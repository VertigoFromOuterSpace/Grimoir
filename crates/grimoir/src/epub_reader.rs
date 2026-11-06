//! Lightweight EPUB reader. Text is rendered with a GtkTextView until the
//! WebKit-based renderer lands; images and complex CSS are intentionally not
//! part of this fallback.

use std::cell::Cell;
use std::cell::RefCell;
use std::fs::File;
use std::io::Read;
use std::path::Path;
use std::rc::Rc;
use std::sync::Arc;

use anyhow::{Context, Result, anyhow};
use grimoir_library::Library;
use gtk::gdk;
use gtk::glib;
use gtk::prelude::*;
use zip::ZipArchive;

use crate::reader;

const BAR_HIDE_DELAY: u32 = 2600;

pub struct EpubReaderWindow {
    window: gtk::ApplicationWindow,
    _state: Rc<EpubState>,
}

type BackHandler = Rc<dyn Fn()>;

struct EpubState {
    window: gtk::ApplicationWindow,
    archive: std::cell::RefCell<ZipArchive<File>>,
    pages: Vec<String>,
    page: Cell<usize>,
    view: gtk::TextView,
    label: gtk::Label,
    bar: gtk::Revealer,
    bar_timer: Cell<Option<glib::SourceId>>,
    bar_hovered: Cell<bool>,
    library: Option<Arc<Library>>,
    book_id: Option<i64>,
    back: RefCell<Option<BackHandler>>,
    controllers: RefCell<Vec<gtk::EventController>>,
}

impl EpubReaderWindow {
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
            File::open(path).with_context(|| format!("failed to open EPUB {}", path.display()))?;
        let mut archive = ZipArchive::new(file)
            .with_context(|| format!("failed to read EPUB {}", path.display()))?;
        let mut pages = Vec::new();
        for index in 0..archive.len() {
            let entry = archive.by_index(index)?;
            if !entry.is_dir() && is_text_page(entry.name()) {
                pages.push(entry.name().to_owned());
            }
        }
        pages.sort_by_key(|name| natural_page_key(name));
        let mut readable = Vec::new();
        for name in pages {
            let Ok(mut entry) = archive.by_name(&name) else {
                continue;
            };
            let mut bytes = Vec::new();
            if entry.read_to_end(&mut bytes).is_ok() {
                let text = html2text::config::with_decorator(
                    html2text::render::text_renderer::TrivialDecorator::new(),
                )
                .string_from_read(bytes.as_slice(), 90)
                .unwrap_or_default();
                let text = text.trim();
                let image_alt = text.len() < 160
                    && [".jpg", ".jpeg", ".png", ".webp", ".gif"]
                        .iter()
                        .any(|ext| text.to_ascii_lowercase().contains(ext));
                if !(text.is_empty() || image_alt || (text.starts_with('[') && text.ends_with(']')))
                {
                    readable.push(name);
                }
            }
        }
        let pages = readable;
        if pages.is_empty() {
            return Err(anyhow!("EPUB has no text pages: {}", path.display()));
        }

        let title = library
            .as_ref()
            .and_then(|library| book_id.and_then(|id| library.book_by_id(id).ok().flatten()))
            .map(|book| book.title)
            .unwrap_or_else(|| {
                path.file_stem()
                    .and_then(|stem| stem.to_str())
                    .unwrap_or("EPUB")
                    .to_owned()
            });
        let authors = library
            .as_ref()
            .and_then(|library| book_id.and_then(|id| library.book_by_id(id).ok().flatten()))
            .and_then(|book| book.authors);

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

        let view = gtk::TextView::new();
        view.set_editable(false);
        view.set_cursor_visible(false);
        view.set_wrap_mode(gtk::WrapMode::WordChar);
        view.set_left_margin(44);
        view.set_right_margin(44);
        view.set_top_margin(36);
        view.set_bottom_margin(36);
        view.add_css_class("epub-text");
        view.set_vexpand(true);
        let scroller = gtk::ScrolledWindow::new();
        scroller.set_child(Some(&view));
        scroller.set_policy(gtk::PolicyType::Never, gtk::PolicyType::Automatic);
        scroller.set_hexpand(true);
        scroller.set_vexpand(true);
        scroller.set_margin_top(74);
        scroller.set_margin_bottom(56);
        scroller.set_margin_start(120);
        scroller.set_margin_end(120);
        scroller.add_css_class("epub-scroll");
        scroller.add_css_class("epub-page");

        let overlay = gtk::Overlay::new();
        overlay.set_hexpand(true);
        overlay.set_vexpand(true);
        overlay.set_child(Some(&scroller));

        let bar = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        bar.add_css_class("reader-bar");
        let title_label = gtk::Label::new(Some(&title));
        title_label.add_css_class("reader-title");
        title_label.set_ellipsize(gtk::pango::EllipsizeMode::End);
        title_label.set_max_width_chars(28);
        title_label.set_hexpand(true);
        title_label.set_xalign(0.0);
        title_label.set_tooltip_text(authors.as_deref());
        bar.append(&title_label);

        let prev = gtk::Button::from_icon_name("go-previous-symbolic");
        prev.set_tooltip_text(Some("Previous section"));
        bar.append(&prev);
        let label = gtk::Label::new(Some("1 / 1"));
        label.add_css_class("reader-muted");
        label.set_width_chars(9);
        bar.append(&label);
        let next = gtk::Button::from_icon_name("go-next-symbolic");
        next.set_tooltip_text(Some("Next section"));
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

        let state = Rc::new(EpubState {
            window: window.clone(),
            archive: std::cell::RefCell::new(archive),
            pages,
            page: Cell::new(0),
            view: view.clone(),
            label: label.clone(),
            bar: bar_revealer.clone(),
            bar_timer: Cell::new(None),
            bar_hovered: Cell::new(false),
            library,
            book_id,
            back: RefCell::new(back),
            controllers: RefCell::new(Vec::new()),
        });

        show_page(&state, 0);
        connect_actions(&state, &prev, &next, &fullscreen, &close, &bar);
        if !embedded {
            window.present();
            window.fullscreen();
        }
        show_bar(&state);
        {
            let window = window.clone();
            let view = view.clone();
            glib::timeout_add_local_once(std::time::Duration::from_millis(800), move || {
                eprintln!(
                    "epub alloc window={} view={} text={}",
                    window.height(),
                    view.height(),
                    view.buffer().char_count()
                );
            });
        }
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
    state: &Rc<EpubState>,
    prev: &gtk::Button,
    next: &gtk::Button,
    fullscreen: &gtk::Button,
    close: &gtk::Button,
    bar: &gtk::Box,
) {
    {
        let state = state.clone();
        prev.connect_clicked(move |_| show_page(&state, state.page.get().saturating_sub(1)));
    }
    {
        let state = state.clone();
        next.connect_clicked(move |_| show_page(&state, state.page.get() + 1));
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

    let motion = gtk::EventControllerMotion::new();
    {
        let state = state.clone();
        motion.connect_enter(move |_, _, _| {
            state.bar_hovered.set(true);
            state.bar.set_reveal_child(true);
        });
    }
    {
        let state = state.clone();
        motion.connect_leave(move |_| {
            state.bar_hovered.set(false);
            schedule_bar_hide(&state);
        });
    }
    bar.add_controller(motion);

    let keys = gtk::EventControllerKey::new();
    keys.set_propagation_phase(gtk::PropagationPhase::Capture);
    {
        let state = state.clone();
        keys.connect_key_pressed(move |_, key, _, _| {
            match key {
                gdk::Key::Left | gdk::Key::Page_Up => {
                    show_page(&state, state.page.get().saturating_sub(1))
                }
                gdk::Key::Right | gdk::Key::Page_Down => show_page(&state, state.page.get() + 1),
                gdk::Key::Home => show_page(&state, 0),
                gdk::Key::End => show_page(&state, state.pages.len().saturating_sub(1)),
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

fn show_page(state: &Rc<EpubState>, index: usize) {
    if index >= state.pages.len() {
        return;
    }
    let name = state.pages[index].clone();
    let mut bytes = Vec::new();
    let read = state
        .archive
        .borrow_mut()
        .by_name(&name)
        .ok()
        .map(|mut entry| entry.read_to_end(&mut bytes));
    if read.is_none() {
        return;
    }
    let html = String::from_utf8_lossy(&bytes);
    let text = html2text::config::with_decorator(
        html2text::render::text_renderer::TrivialDecorator::new(),
    )
    .string_from_read(html.as_bytes(), 90)
    .unwrap_or_default();
    eprintln!(
        "epub page {index} {name} bytes={} chars={} start={:?}",
        bytes.len(),
        text.chars().count(),
        text.chars().take(100).collect::<String>()
    );
    state.view.buffer().set_text(&text);
    state.page.set(index);
    state
        .label
        .set_text(&format!("{} / {}", index + 1, state.pages.len()));
    let buffer = state.view.buffer();
    buffer.place_cursor(&buffer.start_iter());
    state
        .view
        .scroll_to_iter(&mut buffer.start_iter(), 0.0, false, 0.0, 0.0);
    save_progress(state);
    show_bar(state);
}

fn save_progress(state: &EpubState) {
    let (Some(library), Some(book_id)) = (state.library.as_ref(), state.book_id) else {
        return;
    };
    let page = state.page.get();
    let total = state.pages.len().max(1);
    let _ = library.save_progress(book_id, page, (page + 1) as f64 / total as f64);
}

fn show_bar(state: &Rc<EpubState>) {
    state.bar.set_reveal_child(true);
    schedule_bar_hide(state);
}

fn schedule_bar_hide(state: &Rc<EpubState>) {
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

fn is_text_page(name: &str) -> bool {
    ["xhtml", "html", "htm"].iter().any(|extension| {
        name.to_ascii_lowercase()
            .ends_with(&format!(".{extension}"))
    })
}

fn natural_page_key(name: &str) -> (u64, String) {
    let stem = Path::new(name)
        .file_stem()
        .and_then(|stem| stem.to_str())
        .unwrap_or(name);
    let digits: String = stem.chars().filter(|c| c.is_ascii_digit()).collect();
    (
        digits.parse().unwrap_or(u64::MAX),
        name.to_ascii_lowercase(),
    )
}
