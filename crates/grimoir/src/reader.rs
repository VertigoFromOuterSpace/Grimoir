use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet, VecDeque};
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use anyhow::{Context, Result};
use grimoir_library::{Annotation, Library, ReaderPrefs};
use grimoir_pdf::glass;
use grimoir_pdf::worker::{PdfWorker, TocEntry, WorkerEvent};
use grimoir_pdf::{RawPage, SearchHit};
use grimoir_theme::Theme;
use gtk::prelude::*;
use gtk::{gdk, gio, glib};

use crate::canvas::{CanvasMode, FitMode, PageCanvas, PageImage, PageMark, ReadingDirection};
use crate::obsidian;

const RENDER_WIDTH: u32 = 1000;
const RENDER_HEIGHT: u32 = 1500;
const CACHE_LIMIT: usize = 9;
const TEXTURE_CACHE_LIMIT: usize = 12;
const BAR_HIDE_DELAY: u64 = 2600;

#[derive(Clone, Copy)]
struct PendingTurn {
    anchor: usize,
    forward: bool,
    animate: bool,
}

pub struct ReaderWindow {
    window: gtk::ApplicationWindow,
    _state: Rc<ReaderState>,
}

impl ReaderState {
    fn cleanup(&self) {
        self.closed.set(true);
        for source in [
            self.filter_source.take(),
            self.css_source.take(),
            self.warm_source.take(),
            self.bar_timer.take(),
        ]
        .into_iter()
        .flatten()
        {
            source.remove();
        }
        if let Some(monitor) = self.theme_monitor.borrow_mut().take() {
            monitor.cancel();
        }
        for controller in self.controllers.borrow_mut().drain(..) {
            self.window.remove_controller(&controller);
        }
        if let Some(handler) = self.close_handler.take() {
            self.window.disconnect(handler);
        }
        self.worker.shutdown();
        self.cache.borrow_mut().clear();
        self.cache_order.borrow_mut().clear();
        self.texture_cache.borrow_mut().clear();
        self.texture_order.borrow_mut().clear();
        self.requested.borrow_mut().clear();
        self.pending_turn.set(None);
        self.canvas.set_spread(Vec::new(), false, false);
        self.canvas.set_continuous_pages(Vec::new());
    }
}

type AnnotationDrag = (usize, (f64, f64), (f64, f64));
type BackHandler = Rc<dyn Fn()>;

struct ReaderState {
    window: gtk::ApplicationWindow,
    worker: PdfWorker,
    page_count: usize,
    page_sizes: Vec<(f32, f32)>,
    toc: Vec<TocEntry>,
    cache: RefCell<HashMap<usize, RawPage>>,
    cache_order: RefCell<VecDeque<usize>>,
    texture_cache: RefCell<HashMap<usize, gdk::Texture>>,
    texture_order: RefCell<VecDeque<usize>>,
    texture_key: Cell<(u16, u16, bool)>,
    requested: RefCell<HashSet<usize>>,
    pending_turn: Cell<Option<PendingTurn>>,
    page_index: Cell<usize>,
    canvas: PageCanvas,
    double_toggle: gtk::ToggleButton,
    rtl_toggle: gtk::ToggleButton,
    scroll_toggle: gtk::ToggleButton,
    motion_toggle: gtk::ToggleButton,
    fit_dropdown: gtk::DropDown,
    zoom_label: gtk::Label,
    page_label: gtk::Label,
    progress: gtk::ProgressBar,
    search_entry: gtk::SearchEntry,
    search_list: gtk::ListBox,
    search_popover: gtk::Popover,
    search_hits: RefCell<Vec<SearchHit>>,
    search_marks: RefCell<Vec<PageMark>>,
    annotations: RefCell<Vec<Annotation>>,
    annotation_toggle: gtk::ToggleButton,
    annotation_list: gtk::ListBox,
    annotation_popover: gtk::Popover,
    annotation_start: Cell<Option<AnnotationDrag>>,
    annotation_preview: RefCell<Option<PageMark>>,
    controllers: RefCell<Vec<gtk::EventController>>,
    close_handler: Cell<Option<glib::SignalHandlerId>>,
    book_title: String,
    paper_scale: gtk::Scale,
    ink_scale: gtk::Scale,
    veil_scale: gtk::Scale,
    glass_toggle: gtk::ToggleButton,
    css_provider: gtk::CssProvider,
    theme: RefCell<Theme>,
    theme_path: PathBuf,
    theme_mtime: Cell<Option<SystemTime>>,
    theme_monitor: RefCell<Option<gio::FileMonitor>>,
    filter_source: Cell<Option<glib::SourceId>>,
    css_source: Cell<Option<glib::SourceId>>,
    warm_source: Cell<Option<glib::SourceId>>,
    bar_revealer: gtk::Revealer,
    bar_timer: Cell<Option<glib::SourceId>>,
    bar_hovered: Cell<bool>,
    library: Option<Arc<Library>>,
    book_id: Option<i64>,
    last_saved_page: Cell<Option<usize>>,
    session_id: Cell<Option<i64>>,
    session_start_page: Cell<usize>,
    session_started: Cell<Option<std::time::Instant>>,
    back: RefCell<Option<BackHandler>>,
    closed: Cell<bool>,
}

impl ReaderWindow {
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
        let back_mode = back.is_some();
        let (event_tx, event_rx) = async_channel::unbounded();
        let (worker, info) = PdfWorker::spawn(path, event_tx)?;
        let page_count = info.page_count;
        let page_sizes = info.page_sizes;
        let toc = info.table_of_contents;
        let linked_book = match (library.as_ref(), book_id) {
            (Some(library), Some(id)) => library.book_by_id(id).ok().flatten(),
            _ => None,
        };
        let initial_page = linked_book
            .as_ref()
            .map(|book| book.progress_page())
            .unwrap_or(0);
        let prefs = match (library.as_ref(), book_id) {
            (Some(library), Some(id)) => library.load_reader_prefs(id).unwrap_or_default(),
            _ => ReaderPrefs::default(),
        };
        if let (Some(library), Some(id)) = (library.as_ref(), book_id) {
            let _ = library.mark_opened(id);
        }
        let (theme, theme_path) = load_initial_theme();
        let theme_mtime = std::fs::metadata(&theme_path)
            .ok()
            .and_then(|metadata| metadata.modified().ok());

        let title = linked_book
            .as_ref()
            .map(|book| book.title.clone())
            .or_else(|| {
                path.file_name()
                    .map(|name| name.to_string_lossy().into_owned())
            })
            .unwrap_or_else(|| "Grimoir".to_owned());
        let book_title = title.clone();

        let window = if let Some(host) = host {
            host.clone()
        } else {
            let window = gtk::ApplicationWindow::builder()
                .application(app)
                .title(&title)
                .decorated(false)
                .default_width(1280)
                .default_height(800)
                .build();
            window.add_css_class("grimoir-reader");
            window
        };

        let display = gdk::Display::default().context("no GDK display available")?;
        let css_provider = gtk::CssProvider::new();
        gtk::style_context_add_provider_for_display(
            &display,
            &css_provider,
            gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
        );

        let overlay = gtk::Overlay::new();
        let canvas = PageCanvas::new();
        canvas.set_direction(if prefs.rtl {
            ReadingDirection::Rtl
        } else {
            ReadingDirection::Ltr
        });
        canvas.set_fit(match prefs.fit {
            1 => FitMode::Width,
            2 => FitMode::Height,
            3 => FitMode::Original,
            _ => FitMode::Page,
        });
        canvas.set_zoom(prefs.zoom.clamp(0.25, 4.0) as f64);
        overlay.set_child(Some(&canvas));

        let bar = gtk::Box::new(gtk::Orientation::Vertical, 4);
        bar.add_css_class("reader-bar");
        bar.set_halign(gtk::Align::Center);

        let line1 = gtk::Box::new(gtk::Orientation::Horizontal, 6);
        line1.set_halign(gtk::Align::Center);
        let line2 = gtk::Box::new(gtk::Orientation::Horizontal, 6);
        line2.set_halign(gtk::Align::Center);
        bar.append(&line1);
        bar.append(&line2);

        let title_label = gtk::Label::new(Some(&title));
        title_label.add_css_class("reader-title");
        title_label.set_ellipsize(gtk::pango::EllipsizeMode::End);
        title_label.set_max_width_chars(26);
        line1.append(&title_label);

        let prev = gtk::Button::from_icon_name("go-previous-symbolic");
        prev.set_tooltip_text(Some("Previous page (Page Up)"));
        line1.append(&prev);

        let page_label = gtk::Label::new(Some(&format!("1 / {page_count}")));
        page_label.add_css_class("reader-page-label");
        page_label.set_xalign(0.5);
        line1.append(&page_label);

        let progress = gtk::ProgressBar::new();
        progress.set_size_request(110, -1);
        progress.set_valign(gtk::Align::Center);
        line1.append(&progress);

        let next = gtk::Button::from_icon_name("go-next-symbolic");
        next.set_tooltip_text(Some("Next page (Page Down)"));
        line1.append(&next);

        let search_button = gtk::MenuButton::new();
        search_button.set_icon_name("system-search-symbolic");
        search_button.set_tooltip_text(Some("Search inside the book (Ctrl+F)"));
        let search_popover = gtk::Popover::new();
        search_popover.add_css_class("reader-popover");
        let search_box = gtk::Box::new(gtk::Orientation::Vertical, 8);
        search_box.set_margin_top(10);
        search_box.set_margin_bottom(10);
        search_box.set_margin_start(10);
        search_box.set_margin_end(10);
        let search_entry = gtk::SearchEntry::new();
        search_entry.set_placeholder_text(Some("Search inside the book…"));
        search_entry.set_size_request(380, -1);
        search_box.append(&search_entry);
        let search_scroll = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::Never)
            .vscrollbar_policy(gtk::PolicyType::Automatic)
            .min_content_width(400)
            .min_content_height(220)
            .max_content_height(440)
            .propagate_natural_height(true)
            .build();
        let search_list = gtk::ListBox::new();
        search_list.set_selection_mode(gtk::SelectionMode::None);
        search_scroll.set_child(Some(&search_list));
        search_box.append(&search_scroll);
        search_popover.set_child(Some(&search_box));
        search_button.set_popover(Some(&search_popover));
        line1.append(&search_button);

        let toc_button = gtk::MenuButton::new();
        toc_button.set_icon_name("view-list-symbolic");
        toc_button.set_tooltip_text(Some("Table of contents"));
        toc_button.set_sensitive(!toc.is_empty());
        let toc_popover = gtk::Popover::new();
        toc_popover.add_css_class("reader-popover");
        let toc_scroll = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::Never)
            .vscrollbar_policy(gtk::PolicyType::Automatic)
            .min_content_width(380)
            .min_content_height(300)
            .max_content_height(520)
            .propagate_natural_height(true)
            .build();
        let toc_list = gtk::ListBox::new();
        toc_list.set_selection_mode(gtk::SelectionMode::None);
        for entry in &toc {
            let row = gtk::ListBoxRow::new();
            row.set_activatable(entry.page.is_some());
            let label = gtk::Label::new(Some(&entry.title));
            label.set_xalign(0.0);
            label.set_wrap(true);
            label.set_margin_start(10 + (entry.depth.min(6) as i32) * 14);
            label.set_margin_end(10);
            label.set_margin_top(5);
            label.set_margin_bottom(5);
            row.set_child(Some(&label));
            toc_list.append(&row);
        }
        toc_scroll.set_child(Some(&toc_list));
        toc_popover.set_child(Some(&toc_scroll));
        toc_button.set_popover(Some(&toc_popover));
        line1.append(&toc_button);

        let fullscreen = gtk::Button::from_icon_name("view-fullscreen-symbolic");
        fullscreen.set_tooltip_text(Some("Fullscreen (F11)"));
        line1.append(&fullscreen);

        let close = gtk::Button::from_icon_name(if back_mode {
            "go-previous-symbolic"
        } else {
            "window-close-symbolic"
        });
        close.set_tooltip_text(Some(if back_mode {
            "Back to library"
        } else {
            "Close"
        }));
        line1.append(&close);

        let glass_toggle = gtk::ToggleButton::with_label("Glass");
        glass_toggle.set_tooltip_text(Some("Toggle ink-on-glass mode (G)"));
        glass_toggle.set_active(prefs.glass);
        line2.append(&glass_toggle);

        let paper_label = gtk::Label::new(Some("Paper"));
        paper_label.add_css_class("reader-muted");
        line2.append(&paper_label);
        let paper_scale = gtk::Scale::with_range(gtk::Orientation::Horizontal, 0.0, 1.0, 0.01);
        paper_scale.set_draw_value(false);
        paper_scale.set_size_request(88, -1);
        paper_scale.set_value(theme.opacity.paper as f64);
        line2.append(&paper_scale);

        let ink_label = gtk::Label::new(Some("Ink"));
        ink_label.add_css_class("reader-muted");
        line2.append(&ink_label);
        let ink_scale = gtk::Scale::with_range(gtk::Orientation::Horizontal, 0.0, 1.0, 0.01);
        ink_scale.set_draw_value(false);
        ink_scale.set_size_request(88, -1);
        ink_scale.set_value(0.65);
        paper_scale.set_sensitive(prefs.glass);
        ink_scale.set_sensitive(prefs.glass);
        line2.append(&ink_scale);

        let veil_label = gtk::Label::new(Some("Veil"));
        veil_label.add_css_class("reader-muted");
        line2.append(&veil_label);
        let veil_scale = gtk::Scale::with_range(gtk::Orientation::Horizontal, 0.0, 1.0, 0.01);
        veil_scale.set_draw_value(false);
        veil_scale.set_size_request(88, -1);
        veil_scale.set_value(theme.opacity.letterbox as f64);
        line2.append(&veil_scale);

        line2.append(&separator());

        let double_toggle = gtk::ToggleButton::with_label("Double");
        double_toggle.set_tooltip_text(Some("Two-page spread (D)"));
        double_toggle.set_active(prefs.double_page);
        line2.append(&double_toggle);

        let rtl_toggle = gtk::ToggleButton::with_label("RTL");
        rtl_toggle.set_tooltip_text(Some("Right-to-left reading order (R)"));
        rtl_toggle.set_active(prefs.rtl);
        line2.append(&rtl_toggle);

        let scroll_toggle = gtk::ToggleButton::with_label("Scroll");
        scroll_toggle.set_tooltip_text(Some("Continuous vertical scrolling (S)"));
        scroll_toggle.set_active(prefs.scroll);
        line2.append(&scroll_toggle);

        let motion_toggle = gtk::ToggleButton::with_label("Motion");
        motion_toggle.set_tooltip_text(Some("Animate page turns (M)"));
        motion_toggle.set_active(prefs.motion);
        line2.append(&motion_toggle);

        let annotation_toggle = gtk::ToggleButton::with_label("Mark");
        annotation_toggle.set_tooltip_text(Some("Highlight a region (A)"));
        line1.append(&annotation_toggle);

        let notes_button = gtk::MenuButton::new();
        notes_button.set_label("Notes");
        notes_button.set_tooltip_text(Some("Annotations in this book"));
        notes_button.set_always_show_arrow(false);
        let annotation_popover = gtk::Popover::new();
        annotation_popover.add_css_class("reader-popover");
        let annotation_box = gtk::Box::new(gtk::Orientation::Vertical, 6);
        annotation_box.set_margin_top(8);
        annotation_box.set_margin_bottom(8);
        annotation_box.set_margin_start(8);
        annotation_box.set_margin_end(8);
        let annotation_scroll = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::Never)
            .vscrollbar_policy(gtk::PolicyType::Automatic)
            .min_content_width(360)
            .min_content_height(180)
            .max_content_height(420)
            .propagate_natural_height(true)
            .build();
        let annotation_list = gtk::ListBox::new();
        annotation_list.set_selection_mode(gtk::SelectionMode::None);
        annotation_list.set_size_request(360, -1);
        annotation_scroll.set_child(Some(&annotation_list));
        annotation_box.append(&annotation_scroll);
        annotation_popover.set_child(Some(&annotation_box));
        annotation_popover.set_size_request(380, -1);
        annotation_popover.set_autohide(false);
        notes_button.set_popover(Some(&annotation_popover));
        line1.append(&notes_button);

        let obsidian_button = gtk::Button::from_icon_name("send-to-symbolic");
        obsidian_button.set_tooltip_text(Some("Send this page and its highlights to Obsidian"));
        line1.append(&obsidian_button);

        line2.append(&separator());

        let fit_label = gtk::Label::new(Some("Fit"));
        fit_label.add_css_class("reader-muted");
        line2.append(&fit_label);
        let fit_dropdown = gtk::DropDown::from_strings(&["Page", "Width", "Height", "Original"]);
        fit_dropdown.set_selected(prefs.fit.min(3) as u32);
        fit_dropdown.set_tooltip_text(Some("Page fit mode"));
        line2.append(&fit_dropdown);

        let zoom_out = gtk::Button::with_label("-");
        zoom_out.set_tooltip_text(Some("Zoom out (-)"));
        line2.append(&zoom_out);
        let zoom_label = gtk::Label::new(Some(&format!(
            "{:.0}%",
            prefs.zoom.clamp(0.25, 4.0) * 100.0
        )));
        zoom_label.add_css_class("reader-page-label");
        zoom_label.set_size_request(48, -1);
        line2.append(&zoom_label);
        let zoom_in = gtk::Button::with_label("+");
        zoom_in.set_tooltip_text(Some("Zoom in (+)"));
        line2.append(&zoom_in);

        let bar_revealer = gtk::Revealer::new();
        bar_revealer.set_transition_type(gtk::RevealerTransitionType::Crossfade);
        bar_revealer.set_transition_duration(160);
        bar_revealer.set_child(Some(&bar));
        bar_revealer.set_halign(gtk::Align::Center);
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

        let state = Rc::new(ReaderState {
            window: window.clone(),
            worker,
            page_count,
            page_sizes,
            toc,
            cache: RefCell::new(HashMap::new()),
            cache_order: RefCell::new(VecDeque::new()),
            texture_cache: RefCell::new(HashMap::new()),
            texture_order: RefCell::new(VecDeque::new()),
            texture_key: Cell::new((0, 0, false)),
            requested: RefCell::new(HashSet::new()),
            pending_turn: Cell::new(None),
            page_index: Cell::new(0),
            canvas: canvas.clone(),
            double_toggle: double_toggle.clone(),
            rtl_toggle: rtl_toggle.clone(),
            scroll_toggle: scroll_toggle.clone(),
            motion_toggle: motion_toggle.clone(),
            fit_dropdown: fit_dropdown.clone(),
            zoom_label: zoom_label.clone(),
            page_label: page_label.clone(),
            progress: progress.clone(),
            search_entry: search_entry.clone(),
            search_list: search_list.clone(),
            search_popover: search_popover.clone(),
            search_hits: RefCell::new(Vec::new()),
            search_marks: RefCell::new(Vec::new()),
            annotations: RefCell::new(Vec::new()),
            annotation_toggle: annotation_toggle.clone(),
            annotation_list: annotation_list.clone(),
            annotation_popover: annotation_popover.clone(),
            annotation_start: Cell::new(None),
            annotation_preview: RefCell::new(None),
            controllers: RefCell::new(Vec::new()),
            close_handler: Cell::new(None),
            book_title,
            paper_scale: paper_scale.clone(),
            ink_scale: ink_scale.clone(),
            veil_scale: veil_scale.clone(),
            glass_toggle: glass_toggle.clone(),
            css_provider,
            theme: RefCell::new(theme),
            theme_path,
            theme_mtime: Cell::new(theme_mtime),
            theme_monitor: RefCell::new(None),
            filter_source: Cell::new(None),
            css_source: Cell::new(None),
            warm_source: Cell::new(None),
            bar_revealer: bar_revealer.clone(),
            bar_timer: Cell::new(None),
            bar_hovered: Cell::new(false),
            library,
            book_id,
            last_saved_page: Cell::new(None),
            session_id: Cell::new(None),
            session_start_page: Cell::new(initial_page),
            session_started: Cell::new(None),
            back: RefCell::new(back),
            closed: Cell::new(false),
        });

        connect_controls(
            &state,
            &prev,
            &next,
            &glass_toggle,
            &paper_scale,
            &ink_scale,
            &veil_scale,
            &double_toggle,
            &rtl_toggle,
            &scroll_toggle,
            &motion_toggle,
            &fit_dropdown,
            &zoom_out,
            &zoom_in,
            &fullscreen,
            &close,
        );
        connect_toc(&state, &toc_list, &toc_popover);
        connect_search(&state);
        connect_annotations(&state);
        connect_obsidian(&state, &obsidian_button);
        connect_window_actions(&state, &window, &bar_revealer);
        connect_worker_events(&state, event_rx);
        reload_annotations(&state);

        if let (Some(library), Some(id)) = (&state.library, state.book_id) {
            state.session_id.set(library.start_reading_session(id).ok());
            state.session_started.set(Some(std::time::Instant::now()));
        }

        apply_theme(&state);
        watch_theme(&state);
        show_bar(&state);
        state.page_index.set(initial_page);
        if state.scroll_toggle.is_active() {
            set_scroll_mode(&state, true);
        } else {
            go_to(&state, initial_page);
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
        self._state.cleanup();
    }
}

#[allow(clippy::too_many_arguments)]
fn connect_controls(
    state: &Rc<ReaderState>,
    prev: &gtk::Button,
    next: &gtk::Button,
    glass_toggle: &gtk::ToggleButton,
    paper_scale: &gtk::Scale,
    ink_scale: &gtk::Scale,
    veil_scale: &gtk::Scale,
    double_toggle: &gtk::ToggleButton,
    rtl_toggle: &gtk::ToggleButton,
    scroll_toggle: &gtk::ToggleButton,
    motion_toggle: &gtk::ToggleButton,
    fit_dropdown: &gtk::DropDown,
    zoom_out: &gtk::Button,
    zoom_in: &gtk::Button,
    fullscreen: &gtk::Button,
    close: &gtk::Button,
) {
    {
        let state = state.clone();
        prev.connect_clicked(move |_| navigate(&state, -1));
    }
    {
        let state = state.clone();
        next.connect_clicked(move |_| navigate(&state, 1));
    }
    {
        let state = state.clone();
        glass_toggle.connect_toggled(move |button| {
            state.paper_scale.set_sensitive(button.is_active());
            state.ink_scale.set_sensitive(button.is_active());
            schedule_filter(&state);
            save_reader_prefs(&state);
        });
    }
    {
        let state = state.clone();
        paper_scale.connect_value_changed(move |_| schedule_filter(&state));
    }
    {
        let state = state.clone();
        ink_scale.connect_value_changed(move |_| schedule_filter(&state));
    }
    {
        let state = state.clone();
        veil_scale.connect_value_changed(move |scale| {
            state.theme.borrow_mut().opacity.letterbox = scale.value() as f32;
            schedule_css(&state);
        });
    }
    {
        let state = state.clone();
        double_toggle.connect_toggled(move |_| {
            redraw_paged(&state);
            show_bar(&state);
            save_reader_prefs(&state);
        });
    }
    {
        let state = state.clone();
        rtl_toggle.connect_toggled(move |_| {
            let direction = if state.rtl_toggle.is_active() {
                ReadingDirection::Rtl
            } else {
                ReadingDirection::Ltr
            };
            state.canvas.set_direction(direction);
            redraw_paged(&state);
            show_bar(&state);
            save_reader_prefs(&state);
        });
    }
    {
        let state = state.clone();
        scroll_toggle.connect_toggled(move |button| {
            set_scroll_mode(&state, button.is_active());
            show_bar(&state);
            save_reader_prefs(&state);
        });
    }
    {
        let state = state.clone();
        motion_toggle.connect_toggled(move |_| {
            show_bar(&state);
            save_reader_prefs(&state);
        });
    }
    {
        let state = state.clone();
        fit_dropdown.connect_selected_notify(move |dropdown| {
            let fit = match dropdown.selected() {
                1 => FitMode::Width,
                2 => FitMode::Height,
                3 => FitMode::Original,
                _ => FitMode::Page,
            };
            state.canvas.set_fit(fit);
            if state.scroll_toggle.is_active() {
                sync_continuous(&state);
            }
            show_bar(&state);
            save_reader_prefs(&state);
        });
    }
    {
        let state = state.clone();
        zoom_out.connect_clicked(move |_| {
            state.canvas.zoom_by(1.0 / 1.15);
            update_zoom_label(&state);
            show_bar(&state);
            save_reader_prefs(&state);
        });
    }
    {
        let state = state.clone();
        zoom_in.connect_clicked(move |_| {
            state.canvas.zoom_by(1.15);
            update_zoom_label(&state);
            show_bar(&state);
            save_reader_prefs(&state);
        });
    }
    {
        let window = state.window.clone();
        fullscreen.connect_clicked(move |_| {
            toggle_fullscreen(&window);
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
}

fn connect_toc(state: &Rc<ReaderState>, list: &gtk::ListBox, popover: &gtk::Popover) {
    let pages = Rc::new(state.toc.iter().map(|entry| entry.page).collect::<Vec<_>>());
    {
        let state = state.clone();
        let pages = pages.clone();
        let popover = popover.clone();
        list.connect_row_activated(move |_, row| {
            if let Some(Some(page)) = pages.get(row.index() as usize) {
                go_to(&state, *page);
            }
            popover.popdown();
        });
    }
    {
        let state = state.clone();
        let popover = popover.clone();
        popover.connect_visible_notify(move |popover| {
            if popover.is_visible() {
                state.bar_hovered.set(true);
                show_bar(&state);
            } else {
                state.bar_hovered.set(false);
                schedule_bar_hide(&state);
            }
        });
    }
}

fn connect_search(state: &Rc<ReaderState>) {
    {
        let entry = state.search_entry.clone();
        state.search_popover.connect_visible_notify(move |popover| {
            if popover.is_visible() {
                let entry = entry.clone();
                glib::idle_add_local_once(move || {
                    entry.grab_focus();
                });
            }
        });
    }
    {
        let entry = state.search_entry.clone();
        let state = state.clone();
        entry.connect_activate(move |entry| {
            let query = entry.text().trim().to_owned();
            if query.is_empty() {
                return;
            }
            state.search_hits.borrow_mut().clear();
            state.search_marks.borrow_mut().clear();
            apply_annotation_marks(&state);
            while let Some(row) = state.search_list.first_child() {
                state.search_list.remove(&row);
            }
            let row = gtk::ListBoxRow::new();
            let label = gtk::Label::new(Some("Searching…"));
            label.add_css_class("library-status");
            label.set_xalign(0.0);
            label.set_margin_top(8);
            label.set_margin_bottom(8);
            label.set_margin_start(10);
            row.set_child(Some(&label));
            state.search_list.append(&row);
            state.worker.search(query);
        });
    }
    {
        let list = state.search_list.clone();
        let state = state.clone();
        list.connect_row_activated(move |_, row| {
            let index = row.index() as usize;
            let page = state.search_hits.borrow().get(index).map(|hit| hit.page);
            if let Some(page) = page {
                go_to(&state, page);
            }
            state.search_popover.popdown();
        });
    }
}

fn show_search_results(state: &Rc<ReaderState>, results: &[SearchHit]) {
    while let Some(row) = state.search_list.first_child() {
        state.search_list.remove(&row);
    }
    state.search_marks.borrow_mut().clear();
    let mut marks = Vec::new();
    for hit in results {
        for rect in &hit.rects {
            marks.push(PageMark {
                page: hit.page,
                x: rect.x as f64,
                y: rect.y as f64,
                width: rect.width as f64,
                height: rect.height as f64,
                color: gdk::RGBA::new(0.95, 0.72, 0.2, 0.38),
            });
        }
    }
    state.search_marks.replace(marks);
    apply_annotation_marks(state);
    if results.is_empty() {
        let row = gtk::ListBoxRow::new();
        let label = gtk::Label::new(Some("No matches in this book."));
        label.add_css_class("library-status");
        label.set_xalign(0.0);
        label.set_margin_top(8);
        label.set_margin_bottom(8);
        label.set_margin_start(10);
        row.set_child(Some(&label));
        state.search_list.append(&row);
        return;
    }
    for hit in results {
        let row = gtk::ListBoxRow::new();
        let label = gtk::Label::new(Some(&format!("p. {} — {}", hit.page + 1, hit.snippet)));
        label.set_xalign(0.0);
        label.set_wrap(true);
        label.set_lines(3);
        label.set_ellipsize(gtk::pango::EllipsizeMode::End);
        label.set_margin_top(7);
        label.set_margin_bottom(7);
        label.set_margin_start(10);
        label.set_margin_end(10);
        row.set_child(Some(&label));
        state.search_list.append(&row);
    }
}

fn annotation_color(state: &ReaderState) -> gdk::RGBA {
    let accent = state.theme.borrow().colors.accent.clone();
    let mut color =
        gdk::RGBA::parse(&accent).unwrap_or_else(|_| gdk::RGBA::new(0.72, 0.79, 0.73, 1.0));
    color.set_alpha(0.30);
    color
}

fn reload_annotations(state: &Rc<ReaderState>) {
    let (Some(library), Some(book_id)) = (state.library.as_ref(), state.book_id) else {
        return;
    };
    let annotations = library.annotations_for_book(book_id).unwrap_or_default();
    state.annotations.replace(annotations);
    apply_annotation_marks(state);
    refresh_annotation_list(state);
}

fn annotation_marks(state: &ReaderState) -> Vec<PageMark> {
    let fallback = annotation_color(state);
    let mut marks = Vec::new();
    for annotation in state.annotations.borrow().iter() {
        let Some(raw) = annotation.rects_json.as_deref() else {
            continue;
        };
        let Ok(rects) = serde_json::from_str::<Vec<[f64; 4]>>(raw) else {
            continue;
        };
        let mut color = annotation
            .color
            .as_deref()
            .and_then(|hex| gdk::RGBA::parse(hex).ok())
            .unwrap_or(fallback);
        color.set_alpha(0.30);
        for rect in rects {
            marks.push(PageMark {
                page: annotation.page,
                x: rect[0].clamp(0.0, 1.0),
                y: rect[1].clamp(0.0, 1.0),
                width: rect[2].clamp(0.0, 1.0),
                height: rect[3].clamp(0.0, 1.0),
                color,
            });
        }
    }
    marks
}

fn apply_annotation_marks(state: &ReaderState) {
    let mut marks = annotation_marks(state);
    marks.extend(state.search_marks.borrow().iter().cloned());
    if let Some(preview) = state.annotation_preview.borrow().clone() {
        marks.push(preview);
    }
    state.canvas.set_marks(marks);
}

fn refresh_annotation_list(state: &Rc<ReaderState>) {
    while let Some(row) = state.annotation_list.first_child() {
        state.annotation_list.remove(&row);
    }
    let annotations = state.annotations.borrow().clone();
    if annotations.is_empty() {
        let row = gtk::ListBoxRow::new();
        let label = gtk::Label::new(Some("No annotations yet. Use Mark to highlight a region."));
        label.add_css_class("library-status");
        label.set_wrap(true);
        label.set_xalign(0.0);
        label.set_margin_top(8);
        label.set_margin_bottom(8);
        label.set_margin_start(10);
        label.set_margin_end(10);
        row.set_child(Some(&label));
        row.set_activatable(false);
        state.annotation_list.append(&row);
        return;
    }
    for annotation in annotations {
        let row = gtk::ListBoxRow::new();
        let content = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        content.set_margin_top(6);
        content.set_margin_bottom(6);
        content.set_margin_start(10);
        content.set_margin_end(8);
        let label = gtk::Label::new(Some(&format!(
            "p. {} — {}",
            annotation.page + 1,
            annotation.note.as_deref().unwrap_or("Region highlight")
        )));
        label.set_xalign(0.0);
        label.set_hexpand(true);
        label.set_wrap(true);
        content.append(&label);
        let delete = gtk::Button::from_icon_name("user-trash-symbolic");
        delete.set_tooltip_text(Some("Delete annotation"));
        delete.add_css_class("flat");
        content.append(&delete);
        row.set_child(Some(&content));
        state.annotation_list.append(&row);

        {
            let state = state.clone();
            let page = annotation.page;
            let popover = state.annotation_popover.clone();
            row.connect_activate(move |_| {
                go_to(&state, page);
                popover.popdown();
            });
        }
        {
            let state = state.clone();
            let annotation_id = annotation.id;
            delete.connect_clicked(move |_| {
                if let (Some(library), Some(_)) = (state.library.as_ref(), state.book_id)
                    && library.delete_annotation(annotation_id).is_ok()
                {
                    reload_annotations(&state);
                }
            });
        }
    }
}

fn connect_annotations(state: &Rc<ReaderState>) {
    {
        let toggle_widget = state.annotation_toggle.clone();
        let state = state.clone();
        toggle_widget.connect_toggled(move |toggle| {
            state.canvas.set_annotating(toggle.is_active());
            if toggle.is_active() {
                show_bar(&state);
            }
        });
    }
    {
        let popover_widget = state.annotation_popover.clone();
        let state = state.clone();
        popover_widget.connect_visible_notify(move |popover| {
            if popover.is_visible() {
                refresh_annotation_list(&state);
            }
        });
    }
    let drag = gtk::GestureDrag::new();
    drag.set_button(gdk::BUTTON_PRIMARY);
    drag.set_propagation_phase(gtk::PropagationPhase::Capture);
    {
        let state = state.clone();
        drag.connect_drag_begin(move |_, x, y| {
            if !state.annotation_toggle.is_active() {
                return;
            }
            if let Some((page, point)) = state.canvas.page_at(x, y) {
                state.annotation_start.set(Some((page, point, (x, y))));
            }
        });
    }
    {
        let state = state.clone();
        drag.connect_drag_update(move |_, offset_x, offset_y| {
            let Some((page, start, widget_start)) = state.annotation_start.get() else {
                return;
            };
            let current = (widget_start.0 + offset_x, widget_start.1 + offset_y);
            let Some((end_page, end)) = state.canvas.page_at(current.0, current.1) else {
                return;
            };
            if end_page != page {
                return;
            }
            let rect_x = start.0.min(end.0);
            let rect_y = start.1.min(end.1);
            let width = (start.0 - end.0).abs();
            let height = (start.1 - end.1).abs();
            let mut color = annotation_color(&state);
            color.set_alpha(0.22);
            state.annotation_preview.replace(Some(PageMark {
                page,
                x: rect_x,
                y: rect_y,
                width,
                height,
                color,
            }));
            apply_annotation_marks(&state);
        });
    }
    {
        let state = state.clone();
        drag.connect_drag_end(move |_, offset_x, offset_y| {
            let Some((page, start, widget_start)) = state.annotation_start.take() else {
                return;
            };
            let current = (widget_start.0 + offset_x, widget_start.1 + offset_y);
            let Some((end_page, end)) = state.canvas.page_at(current.0, current.1) else {
                state.annotation_preview.replace(None);
                apply_annotation_marks(&state);
                return;
            };
            if end_page != page {
                state.annotation_preview.replace(None);
                apply_annotation_marks(&state);
                return;
            }
            let rect_x = start.0.min(end.0);
            let rect_y = start.1.min(end.1);
            let width = (start.0 - end.0).abs();
            let height = (start.1 - end.1).abs();
            state.annotation_preview.replace(None);
            if width < 0.006 || height < 0.006 {
                apply_annotation_marks(&state);
                return;
            }
            if let (Some(library), Some(book_id)) = (state.library.as_ref(), state.book_id) {
                let color = state.theme.borrow().colors.accent.clone();
                let rects = serde_json::to_string(&vec![[rect_x, rect_y, width, height]])
                    .unwrap_or_else(|_| "[]".to_owned());
                if library
                    .add_annotation(
                        book_id,
                        "region",
                        page,
                        Some(&color),
                        None,
                        None,
                        Some(&rects),
                    )
                    .is_ok()
                {
                    reload_annotations(&state);
                }
            }
            apply_annotation_marks(&state);
            state.annotation_toggle.set_active(false);
            state.canvas.set_annotating(false);
        });
    }
    state.canvas.add_controller(drag);
}

fn show_message(state: &ReaderState, text: &str, detail: &str) {
    let dialog = gtk::MessageDialog::builder()
        .modal(false)
        .decorated(false)
        .transient_for(&state.window)
        .text(text)
        .secondary_text(detail)
        .build();
    dialog.set_focusable(false);
    let toast = dialog.clone();
    glib::timeout_add_local_once(std::time::Duration::from_secs(3), move || toast.close());
    dialog.present();
}

fn choose_obsidian_vault(state: &Rc<ReaderState>) {
    let chooser = gtk::FileChooserNative::builder()
        .title("Choose Obsidian vault")
        .action(gtk::FileChooserAction::SelectFolder)
        .transient_for(&state.window)
        .modal(true)
        .build();
    let state = state.clone();
    chooser.connect_response(move |chooser, response| {
        if response == gtk::ResponseType::Accept
            && let Some(file) = chooser.file()
            && let Some(path) = file.path()
            && let Some(library) = state.library.as_ref()
            && library
                .set_setting("obsidian_vault", &path.to_string_lossy())
                .is_ok()
        {
            export_current_page(&state);
        }
        chooser.destroy();
    });
    chooser.show();
}

fn export_current_page(state: &Rc<ReaderState>) {
    let vault = obsidian::vault_from_env().or_else(|| {
        state
            .library
            .as_ref()
            .and_then(|library| library.setting("obsidian_vault").ok().flatten())
            .map(PathBuf::from)
    });
    let Some(vault) = vault else {
        choose_obsidian_vault(state);
        return;
    };
    let page = state.page_index.get();
    let Some(raw) = state.cache.borrow().get(&page).cloned() else {
        show_message(
            state,
            "Page not ready",
            "Wait for the page to finish rendering and try again.",
        );
        return;
    };
    let annotations = state.annotations.borrow().clone();
    match obsidian::export_page(&vault, &state.book_title, page, &raw, &annotations) {
        Ok(note) => show_message(
            state,
            "Sent to Obsidian",
            &format!("Page {} was added to {}", page + 1, note.display()),
        ),
        Err(error) => show_message(state, "Could not write to Obsidian", &error.to_string()),
    }
}

fn connect_obsidian(state: &Rc<ReaderState>, button: &gtk::Button) {
    let state = state.clone();
    button.connect_clicked(move |_| export_current_page(&state));
}

fn connect_window_actions(
    state: &Rc<ReaderState>,
    window: &gtk::ApplicationWindow,
    bar_revealer: &gtk::Revealer,
) {
    {
        let handler_state = state.clone();
        let handler = window.connect_close_request(move |_| {
            save_progress(&handler_state);
            save_reader_prefs(&handler_state);
            finish_session(&handler_state);
            handler_state.cleanup();
            glib::Propagation::Proceed
        });
        state.close_handler.set(Some(handler));
    }

    {
        let motion_state = state.clone();
        let motion = gtk::EventControllerMotion::new();
        motion.set_propagation_phase(gtk::PropagationPhase::Capture);
        motion.connect_motion(move |_, _, y| {
            if y <= 10.0 {
                show_bar(&motion_state);
            }
        });
        window.add_controller(motion.clone());
        state.controllers.borrow_mut().push(motion.upcast());
    }

    {
        let state = state.clone();
        let motion = gtk::EventControllerMotion::new();
        {
            let state = state.clone();
            motion.connect_enter(move |_, _, _| {
                state.bar_hovered.set(true);
                show_bar(&state);
            });
        }
        motion.connect_leave({
            let state = state.clone();
            move |_| {
                state.bar_hovered.set(false);
                schedule_bar_hide(&state);
            }
        });
        bar_revealer.add_controller(motion);
    }

    let key = gtk::EventControllerKey::new();
    key.set_propagation_phase(gtk::PropagationPhase::Capture);
    {
        let state = state.clone();
        key.connect_key_pressed(move |_, keyval, _, modifiers| {
            if state.search_entry.has_focus() {
                return glib::Propagation::Proceed;
            }
            if keyval == gdk::Key::f && modifiers.contains(gdk::ModifierType::CONTROL_MASK) {
                state.search_popover.popup();
                state.search_entry.grab_focus();
                return glib::Propagation::Stop;
            }
            match keyval {
                gdk::Key::Left => {
                    navigate(&state, -1);
                    glib::Propagation::Stop
                }
                gdk::Key::Right => {
                    navigate(&state, 1);
                    glib::Propagation::Stop
                }
                gdk::Key::Page_Up => {
                    navigate(&state, -1);
                    glib::Propagation::Stop
                }
                gdk::Key::Page_Down => {
                    navigate(&state, 1);
                    glib::Propagation::Stop
                }
                gdk::Key::Home => {
                    go_to(&state, 0);
                    glib::Propagation::Stop
                }
                gdk::Key::End => {
                    go_to_end(&state);
                    glib::Propagation::Stop
                }
                gdk::Key::g => {
                    state
                        .glass_toggle
                        .set_active(!state.glass_toggle.is_active());
                    glib::Propagation::Stop
                }
                gdk::Key::d => {
                    state
                        .double_toggle
                        .set_active(!state.double_toggle.is_active());
                    glib::Propagation::Stop
                }
                gdk::Key::r => {
                    state.rtl_toggle.set_active(!state.rtl_toggle.is_active());
                    glib::Propagation::Stop
                }
                gdk::Key::s => {
                    state
                        .scroll_toggle
                        .set_active(!state.scroll_toggle.is_active());
                    glib::Propagation::Stop
                }
                gdk::Key::m => {
                    state
                        .motion_toggle
                        .set_active(!state.motion_toggle.is_active());
                    glib::Propagation::Stop
                }
                gdk::Key::a => {
                    state
                        .annotation_toggle
                        .set_active(!state.annotation_toggle.is_active());
                    glib::Propagation::Stop
                }
                gdk::Key::n => {
                    state.annotation_popover.popup();
                    glib::Propagation::Stop
                }
                gdk::Key::f | gdk::Key::F11 => {
                    toggle_fullscreen(&state.window);
                    glib::Propagation::Stop
                }
                gdk::Key::plus | gdk::Key::equal => {
                    state.canvas.zoom_by(1.15);
                    update_zoom_label(&state);
                    glib::Propagation::Stop
                }
                gdk::Key::minus => {
                    state.canvas.zoom_by(1.0 / 1.15);
                    update_zoom_label(&state);
                    glib::Propagation::Stop
                }
                gdk::Key::_0 => {
                    state.canvas.reset_view();
                    update_zoom_label(&state);
                    glib::Propagation::Stop
                }
                gdk::Key::Escape => {
                    if let Some(back) = state.back.borrow().as_ref() {
                        back();
                    } else {
                        state.window.close();
                    }
                    glib::Propagation::Stop
                }
                _ => glib::Propagation::Proceed,
            }
        });
    }
    window.add_controller(key.clone());
    state.controllers.borrow_mut().push(key.upcast());

    let scroll = gtk::EventControllerScroll::new(gtk::EventControllerScrollFlags::VERTICAL);
    {
        let state = state.clone();
        scroll.connect_scroll(move |controller, _dx, dy| {
            if controller
                .current_event_state()
                .contains(gdk::ModifierType::CONTROL_MASK)
            {
                if dy < 0.0 {
                    state.canvas.zoom_by(1.15);
                } else if dy > 0.0 {
                    state.canvas.zoom_by(1.0 / 1.15);
                }
                update_zoom_label(&state);
                show_bar(&state);
            } else if state.scroll_toggle.is_active() {
                state.canvas.scroll_by(dy * 160.0);
                sync_continuous(&state);
            } else if dy > 0.0 {
                navigate(&state, 1);
            } else if dy < 0.0 {
                navigate(&state, -1);
            }
            glib::Propagation::Stop
        });
    }
    window.add_controller(scroll.clone());
    state.controllers.borrow_mut().push(scroll.upcast());
}

fn connect_worker_events(state: &Rc<ReaderState>, events: async_channel::Receiver<WorkerEvent>) {
    let weak = Rc::downgrade(state);
    glib::spawn_future_local(async move {
        while let Ok(event) = events.recv().await {
            let Some(state) = weak.upgrade() else {
                break;
            };
            handle_worker_event(&state, event);
        }
    });
}

fn handle_worker_event(state: &Rc<ReaderState>, event: WorkerEvent) {
    if state.closed.get() {
        return;
    }
    let (index, result) = match event {
        WorkerEvent::SearchFinished { results, .. } => {
            let results = results.unwrap_or_default();
            state.search_hits.replace(results.clone());
            show_search_results(state, &results);
            return;
        }
        WorkerEvent::Rendered { index, result } => (index, result),
    };
    state.requested.borrow_mut().remove(&index);
    match result {
        Ok(page) => {
            insert_cache(state, index, page);
            if state.scroll_toggle.is_active() {
                if let Some((first, last)) = state.canvas.visible_range()
                    && index >= first
                    && index <= last
                {
                    update_continuous_texture(state, index);
                }
                sync_continuous_label(state);
            } else if let Some(pending) = state.pending_turn.get() {
                let target = spread_indices(
                    pending.anchor,
                    state.double_toggle.is_active(),
                    state.page_count,
                );
                if all_cached(state, &target) {
                    state.pending_turn.set(None);
                    display_spread(state, pending.anchor, pending.animate, pending.forward);
                }
            } else {
                let target = spread_indices(
                    state.page_index.get(),
                    state.double_toggle.is_active(),
                    state.page_count,
                );
                if target.contains(&index) {
                    refresh_current_spread(state, &target);
                }
            }
        }
        Err(error) => {
            eprintln!("failed to render page {}: {error}", index + 1);
            if let Some(pending) = state.pending_turn.take() {
                display_spread(state, pending.anchor, pending.animate, pending.forward);
            }
        }
    }
    prefetch(state);
    schedule_warm(state);
}

/// Precomputes the ink-on-glass texture for pages that are already in the raw
/// cache but not on screen yet. Doing this shortly after a page settles makes
/// the next turn much cheaper; it runs after the turn animation would finish.
fn schedule_warm(state: &Rc<ReaderState>) {
    if state.closed.get() {
        return;
    }
    if let Some(source) = state.warm_source.take() {
        source.remove();
    }
    let pending = state.clone();
    let source = glib::timeout_add_local_once(Duration::from_millis(450), move || {
        pending.warm_source.set(None);
        warm_textures(&pending);
    });
    state.warm_source.set(Some(source));
}

fn warm_textures(state: &Rc<ReaderState>) {
    if state.closed.get() || state.page_count == 0 {
        return;
    }
    let mut indices = if state.scroll_toggle.is_active() {
        state
            .canvas
            .visible_range()
            .map(|(first, last)| {
                (first..=last.saturating_add(2).min(state.page_count - 1)).collect::<Vec<_>>()
            })
            .unwrap_or_default()
    } else {
        let double = state.double_toggle.is_active();
        let start = spread_start(state.page_index.get(), double);
        let next = if double {
            if start == 0 { 1 } else { start + 2 }
        } else {
            start + 1
        };
        let previous = if double {
            if start <= 1 { 0 } else { start - 2 }
        } else {
            start.saturating_sub(1)
        };
        let mut indices = spread_indices(start, double, state.page_count);
        indices.extend(spread_indices(next, double, state.page_count));
        indices.extend(spread_indices(previous, double, state.page_count));
        indices
    };
    indices.sort_unstable();
    indices.dedup();
    for index in indices {
        if state.cache.borrow().contains_key(&index) {
            let _ = page_image(state, index);
        }
    }
}

fn insert_cache(state: &ReaderState, index: usize, page: RawPage) {
    state.cache.borrow_mut().insert(index, page);
    {
        let mut order = state.cache_order.borrow_mut();
        order.retain(|existing| *existing != index);
        order.push_back(index);
    }
    evict_cache(state);
}

fn evict_cache(state: &ReaderState) {
    let protected = current_indices(state);
    let mut skipped = 0;
    while state.cache.borrow().len() > CACHE_LIMIT {
        let Some(candidate) = state.cache_order.borrow_mut().pop_front() else {
            break;
        };
        if protected.contains(&candidate) {
            let order_len = state.cache_order.borrow().len();
            state.cache_order.borrow_mut().push_back(candidate);
            skipped += 1;
            // If every cached page is currently visible, allow the cache to
            // grow a little instead of spinning forever.
            if skipped > order_len {
                break;
            }
            continue;
        }
        state.cache.borrow_mut().remove(&candidate);
        skipped = 0;
    }
}

fn current_indices(state: &ReaderState) -> HashSet<usize> {
    let mut indices = spread_indices(
        state.page_index.get(),
        state.double_toggle.is_active(),
        state.page_count,
    )
    .into_iter()
    .collect::<HashSet<_>>();
    if state.scroll_toggle.is_active()
        && let Some((first, last)) = state.canvas.visible_range()
    {
        for index in first..=last {
            indices.insert(index);
        }
    }
    indices
}

fn request_page(state: &Rc<ReaderState>, index: usize) {
    if index >= state.page_count || state.cache.borrow().contains_key(&index) {
        return;
    }
    // Always send an urgent request, even if a background prefetch already
    // claimed this page: the worker promotes it ahead of queued prefetches.
    state.requested.borrow_mut().insert(index);
    state
        .worker
        .request_page(index, RENDER_WIDTH, RENDER_HEIGHT);
}

fn prefetch_page(state: &Rc<ReaderState>, index: usize) {
    if index >= state.page_count
        || state.cache.borrow().contains_key(&index)
        || !state.requested.borrow_mut().insert(index)
    {
        return;
    }
    state
        .worker
        .prefetch_page(index, RENDER_WIDTH, RENDER_HEIGHT);
}

fn prefetch(state: &Rc<ReaderState>) {
    if state.scroll_toggle.is_active() {
        if let Some((first, last)) = state.canvas.visible_range() {
            for index in first..=last.saturating_add(2).min(state.page_count - 1) {
                prefetch_page(state, index);
            }
        }
        return;
    }

    let double = state.double_toggle.is_active();
    let anchor = state.page_index.get();
    let start = spread_start(anchor, double);
    let next = if double {
        if start == 0 { 1 } else { start + 2 }
    } else {
        start + 1
    };
    let previous = if double {
        if start <= 1 { 0 } else { start - 2 }
    } else {
        start.saturating_sub(1)
    };
    for index in spread_indices(start, double, state.page_count) {
        prefetch_page(state, index);
    }
    for index in spread_indices(next, double, state.page_count) {
        prefetch_page(state, index);
    }
    for index in spread_indices(previous, double, state.page_count) {
        prefetch_page(state, index);
    }
}

fn all_cached(state: &ReaderState, indices: &[usize]) -> bool {
    let cache = state.cache.borrow();
    indices.iter().all(|index| cache.contains_key(index))
}

fn spread_start(anchor: usize, double: bool) -> usize {
    if double && anchor > 0 && anchor.is_multiple_of(2) {
        anchor - 1
    } else {
        anchor
    }
}

fn spread_indices(anchor: usize, double: bool, page_count: usize) -> Vec<usize> {
    if page_count == 0 {
        return Vec::new();
    }
    let anchor = anchor.min(page_count - 1);
    if !double || anchor == 0 {
        return vec![anchor];
    }
    let start = spread_start(anchor, true);
    if start + 1 < page_count {
        vec![start, start + 1]
    } else {
        vec![start]
    }
}

fn ordered_spread_indices(state: &ReaderState, anchor: usize) -> Vec<usize> {
    let mut indices = spread_indices(anchor, state.double_toggle.is_active(), state.page_count);
    if state.rtl_toggle.is_active() && indices.len() > 1 {
        indices.reverse();
    }
    indices
}

fn page_image(state: &ReaderState, index: usize) -> PageImage {
    let (point_width, point_height) = state.page_sizes.get(index).copied().unwrap_or((0.0, 0.0));

    let key = (
        (state.paper_scale.value() * 1000.0).round() as u16,
        (state.ink_scale.value() * 1000.0).round() as u16,
        state.glass_toggle.is_active(),
    );
    if state.texture_key.get() != key {
        state.texture_key.set(key);
        state.texture_cache.borrow_mut().clear();
        state.texture_order.borrow_mut().clear();
    }

    let cached = state.texture_cache.borrow().get(&index).cloned();
    let texture = cached.or_else(|| {
        let texture = {
            let raw = state.cache.borrow();
            let raw = raw.get(&index)?;
            make_texture(state, raw)
        };

        state
            .texture_cache
            .borrow_mut()
            .insert(index, texture.clone());
        let mut order = state.texture_order.borrow_mut();
        order.retain(|existing| *existing != index);
        order.push_back(index);
        while order.len() > TEXTURE_CACHE_LIMIT {
            if let Some(oldest) = order.pop_front() {
                state.texture_cache.borrow_mut().remove(&oldest);
            }
        }
        Some(texture)
    });

    PageImage {
        index,
        texture,
        point_width: point_width as f64,
        point_height: point_height as f64,
    }
}

fn make_texture(state: &ReaderState, raw: &RawPage) -> gdk::Texture {
    let rgba = if state.glass_toggle.is_active() {
        glass::apply_ink_on_glass(
            raw,
            state.paper_scale.value() as f32,
            state.ink_scale.value() as f32,
        )
    } else {
        raw.rgba.clone()
    };
    let bytes = glib::Bytes::from_owned(rgba);
    let texture = gdk::MemoryTexture::new(
        raw.width as i32,
        raw.height as i32,
        gdk::MemoryFormat::R8g8b8a8,
        &bytes,
        raw.width as usize * 4,
    );
    texture.upcast()
}

fn display_spread(state: &Rc<ReaderState>, anchor: usize, animate: bool, forward: bool) {
    let anchor = spread_start(anchor, state.double_toggle.is_active());
    let indices = ordered_spread_indices(state, anchor);
    let pages = indices
        .iter()
        .map(|index| page_image(state, *index))
        .collect::<Vec<_>>();
    state.canvas.set_mode(CanvasMode::Paged);
    state
        .canvas
        .set_spread(pages, animate && state.motion_toggle.is_active(), forward);
    state.page_index.set(anchor);
    update_page_status(state, &indices);
    save_progress(state);
}

fn refresh_current_spread(state: &ReaderState, indices: &[usize]) {
    let pages = indices
        .iter()
        .map(|index| page_image(state, *index))
        .collect::<Vec<_>>();
    state.canvas.refresh_spread(pages);
}

fn redraw_paged(state: &Rc<ReaderState>) {
    let anchor = state.page_index.get();
    let target = spread_indices(anchor, state.double_toggle.is_active(), state.page_count);
    if all_cached(state, &target) {
        display_spread(state, anchor, false, true);
    } else {
        for index in &target {
            request_page(state, *index);
        }
        state.pending_turn.set(Some(PendingTurn {
            anchor,
            forward: true,
            animate: false,
        }));
    }
}

fn navigate(state: &Rc<ReaderState>, delta: isize) {
    if state.scroll_toggle.is_active() {
        let current = state.canvas.top_page().unwrap_or(state.page_index.get());
        let next = (current as isize + delta).clamp(0, state.page_count.saturating_sub(1) as isize);
        go_to(state, next as usize);
        return;
    }

    let double = state.double_toggle.is_active();
    let start = spread_start(state.page_index.get(), double);
    let next = if delta < 0 {
        if double {
            if start <= 1 { 0 } else { start - 2 }
        } else {
            start.saturating_sub(1)
        }
    } else if double && start == 0 {
        1
    } else {
        start + if double { 2 } else { 1 }
    };
    go_to(state, next);
}

fn go_to(state: &Rc<ReaderState>, index: usize) {
    if state.page_count == 0 {
        return;
    }
    let index = index.min(state.page_count - 1);
    if state.scroll_toggle.is_active() {
        state.canvas.scroll_to_page(index);
        state.page_index.set(index);
        update_page_status(state, &[index]);
        save_progress(state);
        sync_continuous(state);
        return;
    }

    let double = state.double_toggle.is_active();
    let anchor = spread_start(index, double);
    let previous = state.page_index.get();
    let forward = anchor >= previous;
    let target = spread_indices(anchor, double, state.page_count);
    let animate = state.motion_toggle.is_active() && anchor != previous;
    if all_cached(state, &target) {
        display_spread(state, anchor, animate, forward);
    } else {
        for page in &target {
            request_page(state, *page);
        }
        state.pending_turn.set(Some(PendingTurn {
            anchor,
            forward,
            animate,
        }));
    }
}

fn go_to_end(state: &Rc<ReaderState>) {
    if state.page_count == 0 {
        return;
    }
    let last = state.page_count - 1;
    let anchor = if state.double_toggle.is_active() && last > 0 && last % 2 == 1 {
        last - 1
    } else {
        last
    };
    go_to(state, anchor);
}

fn set_scroll_mode(state: &Rc<ReaderState>, enabled: bool) {
    if enabled {
        let pages = (0..state.page_count)
            .map(|index| page_image(state, index))
            .collect::<Vec<_>>();
        state.canvas.set_mode(CanvasMode::Continuous);
        state.canvas.set_continuous_pages(pages);
        let anchor = state.page_index.get();
        let state = state.clone();
        glib::idle_add_local_once(move || {
            if state.scroll_toggle.is_active() {
                state.canvas.scroll_to_page(anchor);
                sync_continuous(&state);
            }
        });
    } else {
        state.canvas.set_mode(CanvasMode::Paged);
        go_to(state, state.page_index.get());
    }
    sync_continuous_label(state);
}

fn sync_continuous(state: &Rc<ReaderState>) {
    if !state.scroll_toggle.is_active() {
        return;
    }
    sync_continuous_label(state);
    if let Some((first, last)) = state.canvas.visible_range() {
        for index in first..=last {
            update_continuous_texture(state, index);
            request_page(state, index);
        }
        for index in last.saturating_add(1)..=last.saturating_add(2).min(state.page_count - 1) {
            prefetch_page(state, index);
        }
    }
}

fn update_continuous_texture(state: &ReaderState, index: usize) {
    if state.cache.borrow().contains_key(&index) {
        state
            .canvas
            .update_continuous_page(page_image(state, index));
    }
}

fn sync_continuous_label(state: &ReaderState) {
    if !state.scroll_toggle.is_active() {
        return;
    }
    if let Some(top) = state.canvas.top_page() {
        state.page_index.set(top);
        update_page_status(state, &[top]);
        save_progress(state);
    }
}

fn save_progress(state: &ReaderState) {
    let (Some(library), Some(book_id)) = (&state.library, state.book_id) else {
        return;
    };
    let page = state.page_index.get();
    if state.last_saved_page.get() == Some(page) {
        return;
    }
    let percent = (page + 1) as f64 / state.page_count.max(1) as f64;
    if let Err(error) = library.save_progress(book_id, page, percent) {
        eprintln!("failed to save reading progress: {error:#}");
    }
    state.last_saved_page.set(Some(page));
}

fn finish_session(state: &ReaderState) {
    let (Some(library), Some(_book_id), Some(session_id)) =
        (&state.library, state.book_id, state.session_id.get())
    else {
        return;
    };
    let duration = state
        .session_started
        .get()
        .map(|started| started.elapsed().as_secs() as i64)
        .unwrap_or(0);
    let pages = state
        .page_index
        .get()
        .saturating_sub(state.session_start_page.get()) as i64;
    if let Err(error) = library.finish_reading_session(session_id, pages, duration) {
        eprintln!("failed to finish reading session: {error:#}");
    }
    state.session_id.set(None);
}

fn save_reader_prefs(state: &ReaderState) {
    let (Some(library), Some(book_id)) = (&state.library, state.book_id) else {
        return;
    };
    let prefs = ReaderPrefs {
        double_page: state.double_toggle.is_active(),
        rtl: state.rtl_toggle.is_active(),
        scroll: state.scroll_toggle.is_active(),
        motion: state.motion_toggle.is_active(),
        fit: state.fit_dropdown.selected().min(3) as u8,
        zoom: state.canvas.zoom() as f32,
        glass: state.glass_toggle.is_active(),
    };
    if let Err(error) = library.save_reader_prefs(book_id, &prefs) {
        eprintln!("failed to save reader preferences: {error:#}");
    }
}

fn update_page_status(state: &ReaderState, indices: &[usize]) {
    if indices.is_empty() {
        return;
    }
    let first = *indices.iter().min().unwrap_or(&0);
    let last = *indices.iter().max().unwrap_or(&first);
    if first == last {
        state
            .page_label
            .set_text(&format!("{} / {}", first + 1, state.page_count));
    } else {
        state.page_label.set_text(&format!(
            "{}-{} / {}",
            first + 1,
            last + 1,
            state.page_count
        ));
    }
    let fraction = (last + 1) as f64 / state.page_count.max(1) as f64;
    state.progress.set_fraction(fraction.clamp(0.0, 1.0));
}

fn update_zoom_label(state: &ReaderState) {
    state
        .zoom_label
        .set_text(&format!("{:.0}%", state.canvas.zoom() * 100.0));
}

fn schedule_filter(state: &Rc<ReaderState>) {
    if let Some(source) = state.filter_source.take() {
        source.remove();
    }
    let pending = state.clone();
    let source = glib::timeout_add_local_once(Duration::from_millis(30), move || {
        pending.filter_source.set(None);
        refresh_textures(&pending);
    });
    state.filter_source.set(Some(source));
}

fn refresh_textures(state: &Rc<ReaderState>) {
    if state.scroll_toggle.is_active() {
        if let Some((first, last)) = state.canvas.visible_range() {
            for index in first..=last {
                update_continuous_texture(state, index);
            }
        }
        return;
    }
    let target = spread_indices(
        state.page_index.get(),
        state.double_toggle.is_active(),
        state.page_count,
    );
    if all_cached(state, &target) {
        refresh_current_spread(state, &target);
    }
    schedule_warm(state);
}

fn schedule_css(state: &Rc<ReaderState>) {
    if let Some(source) = state.css_source.take() {
        source.remove();
    }
    let pending = state.clone();
    let source = glib::timeout_add_local_once(Duration::from_millis(40), move || {
        pending.css_source.set(None);
        apply_theme(&pending);
    });
    state.css_source.set(Some(source));
}

fn apply_theme(state: &ReaderState) {
    let backdrop = gtk::check_version(4, 24, 0).is_some();
    let css = state.theme.borrow().css(backdrop);
    state.css_provider.load_from_data(&css);
}

fn show_bar(state: &Rc<ReaderState>) {
    state.bar_revealer.set_reveal_child(true);
    schedule_bar_hide(state);
}

fn schedule_bar_hide(state: &Rc<ReaderState>) {
    if let Some(source) = state.bar_timer.take() {
        source.remove();
    }
    let pending = state.clone();
    let source = glib::timeout_add_local_once(Duration::from_millis(BAR_HIDE_DELAY), move || {
        pending.bar_timer.set(None);
        if !pending.bar_hovered.get() {
            pending.bar_revealer.set_reveal_child(false);
        }
    });
    state.bar_timer.set(Some(source));
}

pub(crate) fn load_initial_theme() -> (Theme, PathBuf) {
    let config = config_dir().join("grimoir/theme.json");

    if let Some(path) = std::env::var_os("GRIMOIR_THEME") {
        let path = PathBuf::from(path);
        let theme = load_theme_or_default(&path);
        return (theme, path);
    }

    let hypr = std::env::var_os("GRIMOIR_HYPR_THEME")
        .map(PathBuf::from)
        .unwrap_or_else(|| home_dir().join(".local/state/hypr/theme.json"));

    let mut errors = Vec::new();
    for path in [&config, &hypr] {
        if !path.exists() {
            continue;
        }
        match Theme::load(path) {
            Ok(theme) => {
                eprintln!("loaded theme {} from {}", theme.name, path.display());
                return (theme, path.clone());
            }
            Err(error) => errors.push(format!("{}: {error:#}", path.display())),
        }
    }

    for error in errors {
        eprintln!("theme load failed: {error}");
    }

    (Theme::default(), config)
}

fn load_theme_or_default(path: &Path) -> Theme {
    match Theme::load(path) {
        Ok(theme) => {
            eprintln!("loaded theme {} from {}", theme.name, path.display());
            theme
        }
        Err(error) => {
            eprintln!("theme load failed: {error:#}");
            Theme::default()
        }
    }
}

fn watch_theme(state: &Rc<ReaderState>) {
    let Some(parent) = state.theme_path.parent() else {
        return;
    };
    if let Err(error) = std::fs::create_dir_all(parent) {
        eprintln!(
            "failed to create theme directory {}: {error}",
            parent.display()
        );
        return;
    }
    let file = gio::File::for_path(parent);
    match file.monitor_directory(gio::FileMonitorFlags::NONE, None::<&gio::Cancellable>) {
        Ok(monitor) => {
            let pending = state.clone();
            let target = state.theme_path.clone();
            monitor.connect_changed(move |_, file, _other, _event| {
                // A directory monitor also reports unrelated files (and /tmp
                // is busy). Only react when the event names our theme file.
                if file.path().as_deref() == Some(target.as_path()) {
                    schedule_theme_reload(&pending);
                }
            });
            *state.theme_monitor.borrow_mut() = Some(monitor);
        }
        Err(error) => {
            eprintln!(
                "failed to watch theme directory {}: {error}",
                parent.display()
            );
        }
    }
}

fn schedule_theme_reload(state: &Rc<ReaderState>) {
    if let Some(source) = state.css_source.take() {
        source.remove();
    }
    let pending = state.clone();
    let source = glib::timeout_add_local_once(Duration::from_millis(140), move || {
        pending.css_source.set(None);
        reload_theme(&pending);
    });
    state.css_source.set(Some(source));
}

fn reload_theme(state: &ReaderState) {
    if !state.theme_path.exists() {
        return;
    }
    let modified = std::fs::metadata(&state.theme_path)
        .ok()
        .and_then(|metadata| metadata.modified().ok());
    if modified.is_some() && modified == state.theme_mtime.get() {
        return;
    }
    let previous_paper = state.theme.borrow().opacity.paper;
    match Theme::load(&state.theme_path) {
        Ok(mut theme) => {
            let next_paper = theme.opacity.paper;
            theme.opacity.letterbox = state.veil_scale.value() as f32;
            *state.theme.borrow_mut() = theme;
            apply_theme(state);
            // Follow theme-wide opacity changes unless the reader has moved
            // the slider manually since the last reload.
            if (state.paper_scale.value() as f32 - previous_paper).abs() < 0.005 {
                state.paper_scale.set_value(next_paper as f64);
            }
            state.theme_mtime.set(modified);
            eprintln!("theme reloaded from {}", state.theme_path.display());
        }
        Err(error) => eprintln!("theme reload failed: {error:#}"),
    }
}

fn toggle_fullscreen(window: &gtk::ApplicationWindow) {
    if window.is_fullscreen() {
        window.unfullscreen();
    } else {
        window.fullscreen();
    }
}

fn separator() -> gtk::Separator {
    gtk::Separator::new(gtk::Orientation::Vertical)
}

fn home_dir() -> PathBuf {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."))
}

fn config_dir() -> PathBuf {
    std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| home_dir().join(".config"))
}

pub fn show_error(app: &gtk::Application, message: &str) {
    let window = gtk::ApplicationWindow::builder()
        .application(app)
        .title("Grimoir")
        .default_width(760)
        .default_height(320)
        .build();
    let label = gtk::Label::builder()
        .label(message)
        .wrap(true)
        .selectable(true)
        .xalign(0.0)
        .margin_top(24)
        .margin_bottom(24)
        .margin_start(24)
        .margin_end(24)
        .build();
    window.set_child(Some(&label));
    window.present();
}
