//! Minimal CBZ reader: one image per page, fitted to the reader window.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::fs::File;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;

use anyhow::{Context, Result, anyhow};
use grimoir_library::Library;
use gtk::gdk;
use gtk::glib;
use gtk::prelude::*;
use zip::ZipArchive;

use crate::canvas::{CanvasMode, FitMode, PageCanvas, PageImage, ReadingDirection};
use crate::reader;

const BAR_HIDE_DELAY: u32 = 2600;
const TEXTURE_CACHE_LIMIT: usize = 8;

pub struct CbzReaderWindow {
    window: gtk::ApplicationWindow,
    _state: Rc<CbzState>,
}

type BackHandler = Rc<dyn Fn()>;

struct CbzState {
    window: gtk::ApplicationWindow,
    canvas: PageCanvas,
    archive: RefCell<ZipArchive<File>>,
    pages: Vec<String>,
    page: Cell<usize>,
    cache: RefCell<HashMap<usize, gdk::Texture>>,
    cache_order: RefCell<Vec<usize>>,
    label: gtk::Label,
    bar: gtk::Revealer,
    bar_timer: Cell<Option<glib::SourceId>>,
    bar_hovered: Cell<bool>,
    library: Option<Arc<Library>>,
    book_id: Option<i64>,
    back: RefCell<Option<BackHandler>>,
    controllers: RefCell<Vec<gtk::EventController>>,
}

impl CbzReaderWindow {
    pub fn open(
        app: &gtk::Application,
        path: &Path,
        library: Option<Arc<Library>>,
        book_id: Option<i64>,
    ) -> Result<Self> {
        Self::open_inner(app, path, library, book_id, None, None, None)
    }

    pub fn open_embedded(
        app: &gtk::Application,
        path: &Path,
        library: Option<Arc<Library>>,
        book_id: Option<i64>,
        host: &gtk::ApplicationWindow,
        container: &gtk::Box,
        back: BackHandler,
    ) -> Result<Self> {
        Self::open_inner(
            app,
            path,
            library,
            book_id,
            Some(host),
            Some(container),
            Some(back),
        )
    }

    fn open_inner(
        app: &gtk::Application,
        path: &Path,
        library: Option<Arc<Library>>,
        book_id: Option<i64>,
        host: Option<&gtk::ApplicationWindow>,
        container: Option<&gtk::Box>,
        back: Option<BackHandler>,
    ) -> Result<Self> {
        let embedded = host.is_some();
        let back_mode = back.is_some();
        let file =
            File::open(path).with_context(|| format!("failed to open CBZ {}", path.display()))?;
        let mut archive = ZipArchive::new(file)
            .with_context(|| format!("failed to read CBZ {}", path.display()))?;
        let mut pages = Vec::new();
        for index in 0..archive.len() {
            let entry = archive.by_index(index)?;
            if !entry.is_dir() && is_image(entry.name()) {
                pages.push(entry.name().to_owned());
            }
        }
        pages.sort_by_key(|name| natural_page_key(name));
        if pages.is_empty() {
            return Err(anyhow!("CBZ has no image pages: {}", path.display()));
        }

        let title = library
            .as_ref()
            .and_then(|library| book_id.and_then(|id| library.book_by_id(id).ok().flatten()))
            .map(|book| book.title)
            .unwrap_or_else(|| {
                path.file_stem()
                    .and_then(|stem| stem.to_str())
                    .unwrap_or("CBZ")
                    .to_owned()
            });

        let window = if let Some(host) = host {
            host.clone()
        } else {
            let window = gtk::ApplicationWindow::builder()
                .application(app)
                .title(&title)
                .decorated(false)
                .build();
            window.add_css_class("grimoir-reader");
            window
        };

        let (theme, _theme_path) = reader::load_initial_theme();
        let provider = gtk::CssProvider::new();
        let backdrop = gtk::check_version(4, 24, 0).is_some();
        provider.load_from_data(&theme.css(backdrop));
        if let Some(display) = gdk::Display::default() {
            gtk::style_context_add_provider_for_display(
                &display,
                &provider,
                gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
            );
        }

        let canvas = PageCanvas::new();
        canvas.set_mode(CanvasMode::Paged);
        canvas.set_fit(FitMode::Page);
        canvas.set_direction(ReadingDirection::Ltr);

        let overlay = gtk::Overlay::new();
        overlay.set_child(Some(&canvas));

        let bar = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        bar.add_css_class("reader-bar");
        let title_label = gtk::Label::new(Some(&title));
        title_label.add_css_class("reader-title");
        title_label.set_ellipsize(gtk::pango::EllipsizeMode::End);
        title_label.set_max_width_chars(28);
        title_label.set_hexpand(true);
        title_label.set_xalign(0.0);
        bar.append(&title_label);

        let prev = gtk::Button::from_icon_name("go-previous-symbolic");
        prev.set_tooltip_text(Some("Previous page"));
        bar.append(&prev);
        let label = gtk::Label::new(Some("1 / 1"));
        label.add_css_class("reader-muted");
        label.set_width_chars(9);
        bar.append(&label);
        let next = gtk::Button::from_icon_name("go-next-symbolic");
        next.set_tooltip_text(Some("Next page"));
        bar.append(&next);

        let fullscreen = gtk::Button::from_icon_name("view-fullscreen-symbolic");
        fullscreen.set_tooltip_text(Some("Toggle fullscreen (F)"));
        bar.append(&fullscreen);
        let close = gtk::Button::from_icon_name(if back_mode {
            "go-previous-symbolic"
        } else {
            "window-close-symbolic"
        });
        close.set_tooltip_text(Some(if back_mode {
            "Back to library (Esc)"
        } else {
            "Close (Esc)"
        }));
        bar.append(&close);

        let bar_revealer = gtk::Revealer::new();
        bar_revealer.set_transition_type(gtk::RevealerTransitionType::SlideDown);
        bar_revealer.set_transition_duration(180);
        bar_revealer.set_child(Some(&bar));
        bar_revealer.set_halign(gtk::Align::Fill);
        bar_revealer.set_valign(gtk::Align::Start);
        bar_revealer.set_margin_top(10);
        bar_revealer.set_margin_start(10);
        bar_revealer.set_margin_end(10);
        overlay.add_overlay(&bar_revealer);

        if let Some(container) = container {
            container.append(&overlay);
        } else {
            window.set_child(Some(&overlay));
        }

        let state = Rc::new(CbzState {
            window: window.clone(),
            canvas: canvas.clone(),
            archive: RefCell::new(archive),
            pages,
            page: Cell::new(0),
            cache: RefCell::new(HashMap::new()),
            cache_order: RefCell::new(Vec::new()),
            label: label.clone(),
            bar: bar_revealer.clone(),
            bar_timer: Cell::new(None),
            bar_hovered: Cell::new(false),
            library,
            book_id,
            back: RefCell::new(back),
            controllers: RefCell::new(Vec::new()),
        });

        show_page(&state, 0, false);
        update_label(&state);

        connect_actions(&state, &prev, &next, &fullscreen, &close, &bar);
        if !embedded {
            window.present();
            window.fullscreen();
        }
        show_bar(&state);
        Ok(Self {
            window,
            _state: state,
        })
    }

    pub fn window(&self) -> &gtk::ApplicationWindow {
        &self.window
    }

    pub fn shutdown(&self) {
        if let Some(timer) = self._state.bar_timer.take() {
            timer.remove();
        }
        for controller in self._state.controllers.borrow_mut().drain(..) {
            self.window.remove_controller(&controller);
        }
    }
}

fn connect_actions(
    state: &Rc<CbzState>,
    prev: &gtk::Button,
    next: &gtk::Button,
    fullscreen: &gtk::Button,
    close: &gtk::Button,
    bar: &gtk::Box,
) {
    {
        let state = state.clone();
        prev.connect_clicked(move |_| show_page(&state, state.page.get().saturating_sub(1), true));
    }
    {
        let state = state.clone();
        let next = next.clone();
        next.connect_clicked(move |_| show_page(&state, state.page.get() + 1, true));
    }
    {
        let window = state.window.clone();
        fullscreen.connect_clicked(move |_| {
            if window.is_fullscreen() {
                window.unfullscreen();
            } else {
                window.fullscreen();
            }
        });
    }
    {
        let state = state.clone();
        close.connect_clicked(move |_| {
            if let Some(back) = state.back.borrow().as_ref() {
                back();
            } else {
                state.window.close();
            }
        });
    }

    let bar_clone = bar.clone();
    let state_bar = state.clone();
    let motion = gtk::EventControllerMotion::new();
    motion.connect_enter(move |_, _, _| {
        state_bar.bar_hovered.set(true);
        state_bar.bar.set_reveal_child(true);
    });
    motion.connect_leave({
        let state = state.clone();
        move |_| {
            state.bar_hovered.set(false);
            schedule_bar_hide(&state);
        }
    });
    bar_clone.add_controller(motion);

    let window_motion = gtk::EventControllerMotion::new();
    {
        let state = state.clone();
        window_motion.connect_motion(move |_, _, y| {
            if y <= 10.0 {
                show_bar(&state);
            }
        });
    }
    state.window.add_controller(window_motion.clone());
    state.controllers.borrow_mut().push(window_motion.upcast());

    let keys = gtk::EventControllerKey::new();
    {
        let state = state.clone();
        keys.connect_key_pressed(move |_, key, _, _| {
            match key {
                gdk::Key::Left | gdk::Key::Page_Up => {
                    show_page(&state, state.page.get().saturating_sub(1), true);
                }
                gdk::Key::Right | gdk::Key::Page_Down => {
                    show_page(&state, state.page.get() + 1, true);
                }
                gdk::Key::Home => show_page(&state, 0, true),
                gdk::Key::End => show_page(&state, state.pages.len().saturating_sub(1), true),
                gdk::Key::f | gdk::Key::F | gdk::Key::F11 => {
                    if state.window.is_fullscreen() {
                        state.window.unfullscreen();
                    } else {
                        state.window.fullscreen();
                    }
                }
                gdk::Key::Escape => {
                    if let Some(back) = state.back.borrow().as_ref() {
                        back();
                    } else {
                        state.window.close();
                    }
                }
                _ => return glib::Propagation::Proceed,
            }
            glib::Propagation::Stop
        });
    }
    state.window.add_controller(keys.clone());
    state.controllers.borrow_mut().push(keys.upcast());

    let scroll = gtk::EventControllerScroll::new(gtk::EventControllerScrollFlags::VERTICAL);
    {
        let state = state.clone();
        scroll.connect_scroll(move |_, _, dy| {
            if dy > 0.0 {
                show_page(&state, state.page.get() + 1, true);
            } else if dy < 0.0 {
                show_page(&state, state.page.get().saturating_sub(1), true);
            }
            glib::Propagation::Stop
        });
    }
    state.window.add_controller(scroll.clone());
    state.controllers.borrow_mut().push(scroll.upcast());

    {
        let window = state.window.clone();
        let state = state.clone();
        window.connect_close_request(move |_| {
            if let Some(source) = state.bar_timer.take() {
                source.remove();
            }
            glib::Propagation::Proceed
        });
    }
}

fn show_page(state: &Rc<CbzState>, index: usize, forward: bool) {
    if index >= state.pages.len() {
        return;
    }
    if let Some(texture) = texture_for(state, index) {
        let page = PageImage {
            index,
            point_width: texture.width() as f64,
            point_height: texture.height() as f64,
            texture: Some(texture),
        };
        state.canvas.set_spread(vec![page], false, forward);
        state.page.set(index);
        update_label(state);
        save_progress(state);
    }
    show_bar(state);
}

fn texture_for(state: &Rc<CbzState>, index: usize) -> Option<gdk::Texture> {
    if let Some(texture) = state.cache.borrow().get(&index).cloned() {
        return Some(texture);
    }
    let name = state.pages.get(index)?.clone();
    let mut bytes = Vec::new();
    state
        .archive
        .borrow_mut()
        .by_name(&name)
        .ok()?
        .read_to_end(&mut bytes)
        .ok()?;
    let image = image::load_from_memory(&bytes).ok()?.to_rgba8();
    let (width, height) = image.dimensions();
    let bytes = glib::Bytes::from_owned(image.into_raw());
    let texture: gdk::Texture = gdk::MemoryTexture::new(
        width as i32,
        height as i32,
        gdk::MemoryFormat::R8g8b8a8,
        &bytes,
        width as usize * 4,
    )
    .upcast();
    state.cache.borrow_mut().insert(index, texture.clone());
    let mut order = state.cache_order.borrow_mut();
    order.retain(|existing| *existing != index);
    order.push(index);
    while order.len() > TEXTURE_CACHE_LIMIT {
        if let Some(oldest) = order.first().copied() {
            order.remove(0);
            state.cache.borrow_mut().remove(&oldest);
        }
    }
    Some(texture)
}

fn update_label(state: &CbzState) {
    state
        .label
        .set_text(&format!("{} / {}", state.page.get() + 1, state.pages.len()));
}

fn save_progress(state: &CbzState) {
    let (Some(library), Some(book_id)) = (state.library.as_ref(), state.book_id) else {
        return;
    };
    let page = state.page.get();
    let total = state.pages.len().max(1);
    let _ = library.save_progress(book_id, page, (page + 1) as f64 / total as f64);
}

fn show_bar(state: &Rc<CbzState>) {
    state.bar.set_reveal_child(true);
    schedule_bar_hide(state);
}

fn schedule_bar_hide(state: &Rc<CbzState>) {
    if let Some(source) = state.bar_timer.take() {
        source.remove();
    }
    let pending = state.clone();
    let source = glib::timeout_add_local_once(
        std::time::Duration::from_millis(BAR_HIDE_DELAY as u64),
        move || {
            pending.bar_timer.set(None);
            if !pending.bar_hovered.get() {
                pending.bar.set_reveal_child(false);
            }
        },
    );
    state.bar_timer.set(Some(source));
}

fn is_image(name: &str) -> bool {
    ["jpg", "jpeg", "png", "webp", "gif", "bmp"]
        .iter()
        .any(|extension| {
            name.to_ascii_lowercase()
                .ends_with(&format!(".{extension}"))
        })
}

fn natural_page_key(name: &str) -> (u64, String) {
    let stem = PathBuf::from(name)
        .file_stem()
        .and_then(|stem| stem.to_str())
        .unwrap_or(name)
        .to_owned();
    let digits: String = stem.chars().filter(|c| c.is_ascii_digit()).collect();
    (
        digits.parse().unwrap_or(u64::MAX),
        name.to_ascii_lowercase(),
    )
}
