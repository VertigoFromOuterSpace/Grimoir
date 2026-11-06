use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use anyhow::{Context, Result};
use grimoir_library::scan::{self, ScanEvent};
use grimoir_library::watch::{self, WatchEvent, WatchHandle};
use grimoir_library::{Book, Collection, Library};
use grimoir_theme::Theme;
use gtk::prelude::*;
use gtk::{gdk, gio, glib};

use crate::cbz_reader::CbzReaderWindow;
use crate::epub_reader::EpubReaderWindow;
use crate::reader::ReaderWindow;

const COVER_CACHE_LIMIT: usize = 96;

#[derive(Clone, Debug, PartialEq, Eq)]
enum Section {
    Continue,
    All,
    Favorites,
    Recent,
    Series(String),
    Folder(PathBuf),
    Collection(i64, String),
}

#[derive(Clone)]
struct CollectionCard {
    id: i64,
    name: String,
    count: usize,
    covers: Vec<PathBuf>,
}

#[derive(Clone)]
enum CatalogItem {
    Book(Box<Book>),
    Collection(CollectionCard),
}

pub struct LibraryWindow {
    window: gtk::ApplicationWindow,
    _state: Rc<LibraryState>,
}

enum ActiveReader {
    Pdf(ReaderWindow),
    Cbz(CbzReaderWindow),
    Epub(EpubReaderWindow),
}

impl ActiveReader {
    fn window(&self) -> &gtk::ApplicationWindow {
        match self {
            Self::Pdf(reader) => reader.window(),
            Self::Cbz(reader) => reader.window(),
            Self::Epub(reader) => reader.window(),
        }
    }
}

struct LibraryState {
    window: gtk::ApplicationWindow,
    app: gtk::Application,
    library: Arc<Library>,
    books: RefCell<Vec<Book>>,
    section: RefCell<Section>,
    query: RefCell<String>,
    store: gio::ListStore,
    grid: gtk::GridView,
    empty_state: gtk::Box,
    count_label: gtk::Label,
    status_label: gtk::Label,
    series_nav: gtk::Box,
    folder_nav: gtk::Box,
    collection_nav: gtk::Box,
    back_button: gtk::Button,
    opening_book: Cell<bool>,
    selection_mode: Cell<bool>,
    selected: RefCell<HashSet<i64>>,
    selection_bar: gtk::Box,
    selection_count: gtk::Label,
    selection_favorite: gtk::Button,
    selection_collection: gtk::Button,
    selection_remove: gtk::Button,
    nav_buttons: RefCell<Vec<(Section, gtk::Button)>>,
    cover_cache: RefCell<HashMap<i64, gdk::Texture>>,
    last_signature: RefCell<String>,
    nav_signature: RefCell<String>,
    scanning: Cell<bool>,
    watch_handle: RefCell<Option<WatchHandle>>,
    css_provider: gtk::CssProvider,
    theme_path: PathBuf,
    theme_mtime: Cell<Option<SystemTime>>,
    theme_monitor: RefCell<Option<gio::FileMonitor>>,
    theme_source: Cell<Option<glib::SourceId>>,
    active_reader: RefCell<Option<ActiveReader>>,
}

impl LibraryWindow {
    pub fn open(app: &gtk::Application, library: Arc<Library>) -> Result<Self> {
        let (theme, theme_path) = load_theme();
        let theme_mtime = std::fs::metadata(&theme_path)
            .ok()
            .and_then(|metadata| metadata.modified().ok());
        let display = gdk::Display::default().context("no GDK display available")?;
        let provider = gtk::CssProvider::new();
        provider.load_from_data(&theme.css(gtk::check_version(4, 24, 0).is_some()));
        gtk::style_context_add_provider_for_display(
            &display,
            &provider,
            gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
        );

        let window = gtk::ApplicationWindow::builder()
            .application(app)
            .title("Grimoir — Library")
            .default_width(1440)
            .default_height(900)
            .build();
        window.add_css_class("library-window");

        let overlay = gtk::Overlay::new();

        let store = gio::ListStore::new::<glib::BoxedAnyObject>();
        let selection = gtk::NoSelection::new(Some(store.clone()));
        let factory = gtk::SignalListItemFactory::new();
        let grid = gtk::GridView::new(Some(selection), Some(factory.clone()));
        grid.add_css_class("library-grid");
        grid.set_max_columns(6);
        grid.set_min_columns(2);
        grid.set_single_click_activate(true);
        grid.set_vexpand(true);
        grid.set_margin_top(112);
        grid.set_margin_bottom(92);
        grid.set_margin_start(44);
        grid.set_margin_end(44);

        let scrolled = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::Never)
            .vscrollbar_policy(gtk::PolicyType::Automatic)
            .vexpand(true)
            .hexpand(true)
            .build();
        scrolled.add_css_class("library-scroll");
        scrolled.set_child(Some(&grid));
        overlay.set_child(Some(&scrolled));

        let empty_state = gtk::Box::new(gtk::Orientation::Vertical, 8);
        empty_state.add_css_class("library-empty-state");
        empty_state.set_halign(gtk::Align::Center);
        empty_state.set_valign(gtk::Align::Center);
        empty_state.set_size_request(520, -1);
        empty_state.set_visible(false);
        overlay.add_overlay(&empty_state);

        let topbar = gtk::Box::new(gtk::Orientation::Horizontal, 10);
        topbar.add_css_class("library-topbar");
        topbar.set_halign(gtk::Align::Fill);
        topbar.set_valign(gtk::Align::Start);
        topbar.set_margin_top(16);
        topbar.set_margin_start(18);
        topbar.set_margin_end(18);

        let back_button = gtk::Button::from_icon_name("go-previous-symbolic");
        back_button.add_css_class("library-icon");
        back_button.set_tooltip_text(Some("Back to all books"));
        back_button.set_visible(false);
        topbar.append(&back_button);

        let brand = gtk::Label::new(Some("GRIMOIR"));
        brand.add_css_class("library-brand");
        brand.set_xalign(0.0);
        topbar.append(&brand);

        let search = gtk::SearchEntry::new();
        search.add_css_class("library-search");
        search.set_placeholder_text(Some("Search the shelves…"));
        search.set_hexpand(true);
        search.set_max_width_chars(30);
        search.set_width_request(210);
        topbar.append(&search);

        let filters = gtk::Box::new(gtk::Orientation::Horizontal, 2);
        filters.add_css_class("library-filters");
        let continue_button = filter_chip("Continue");
        let all_button = filter_chip("All");
        let favorites_button = filter_chip("Favorites");
        let recent_button = filter_chip("Recent");
        filters.append(&continue_button);
        filters.append(&all_button);
        filters.append(&favorites_button);
        filters.append(&recent_button);
        topbar.append(&filters);

        let series_nav = gtk::Box::new(gtk::Orientation::Vertical, 2);
        let folder_nav = gtk::Box::new(gtk::Orientation::Vertical, 2);
        let collection_nav = gtk::Box::new(gtk::Orientation::Vertical, 2);

        let browse_button = gtk::MenuButton::new();
        browse_button.set_always_show_arrow(false);
        browse_button.set_tooltip_text(Some("Series and folders"));
        let browse_label = gtk::Box::new(gtk::Orientation::Horizontal, 5);
        browse_label.append(&gtk::Label::new(Some("Browse")));
        browse_label.append(&gtk::Image::from_icon_name("pan-down-symbolic"));
        browse_button.set_child(Some(&browse_label));
        let browse_popover = gtk::Popover::new();
        browse_popover.add_css_class("reader-popover");
        browse_popover.add_css_class("library-menu");
        let browse_box = gtk::Box::new(gtk::Orientation::Vertical, 4);
        browse_box.set_margin_top(8);
        browse_box.set_margin_bottom(8);
        browse_box.set_margin_start(8);
        browse_box.set_margin_end(8);
        let series_label = gtk::Label::new(Some("SERIES"));
        series_label.add_css_class("library-menu-heading");
        series_label.set_xalign(0.0);
        browse_box.append(&series_label);
        browse_box.append(&series_nav);
        let collection_label = gtk::Label::new(Some("COLLECTIONS"));
        collection_label.add_css_class("library-menu-heading");
        collection_label.set_xalign(0.0);
        collection_label.set_margin_top(8);
        browse_box.append(&collection_label);
        browse_box.append(&collection_nav);
        let new_collection = gtk::Button::with_label("New collection…");
        new_collection.add_css_class("library-menu-item");
        new_collection.set_halign(gtk::Align::Fill);
        browse_box.append(&new_collection);

        let folder_label = gtk::Label::new(Some("FOLDERS"));
        folder_label.add_css_class("library-menu-heading");
        folder_label.set_xalign(0.0);
        folder_label.set_margin_top(8);
        browse_box.append(&folder_label);
        browse_box.append(&folder_nav);
        browse_popover.set_child(Some(&browse_box));
        browse_button.set_popover(Some(&browse_popover));
        topbar.append(&browse_button);

        let new_collection_top = gtk::Button::from_icon_name("list-add-symbolic");
        new_collection_top.add_css_class("library-icon");
        new_collection_top.set_tooltip_text(Some("New collection"));
        topbar.append(&new_collection_top);

        let stats = gtk::Button::from_icon_name("view-list-symbolic");
        stats.add_css_class("library-icon");
        stats.set_tooltip_text(Some("Reading statistics"));
        topbar.append(&stats);

        let settings = gtk::Button::from_icon_name("emblem-system-symbolic");
        settings.add_css_class("library-icon");
        settings.set_tooltip_text(Some("Settings"));
        topbar.append(&settings);

        let rescan = gtk::Button::from_icon_name("view-refresh-symbolic");
        rescan.add_css_class("library-icon");
        rescan.set_tooltip_text(Some("Rescan library"));
        topbar.append(&rescan);

        let add_folder = gtk::Button::from_icon_name("folder-new-symbolic");
        add_folder.add_css_class("library-icon");
        add_folder.set_tooltip_text(Some("Add a folder to the library"));
        topbar.append(&add_folder);
        overlay.add_overlay(&topbar);

        let bottom = gtk::Box::new(gtk::Orientation::Horizontal, 12);
        bottom.add_css_class("library-bottombar");
        bottom.set_halign(gtk::Align::Fill);
        bottom.set_valign(gtk::Align::End);
        bottom.set_margin_bottom(14);
        bottom.set_margin_start(22);
        bottom.set_margin_end(22);

        let status_label = gtk::Label::new(Some(""));
        status_label.add_css_class("library-status");
        status_label.set_wrap(false);
        status_label.set_xalign(0.0);
        status_label.set_hexpand(true);
        bottom.append(&status_label);

        let count_label = gtk::Label::new(Some("No books"));
        count_label.add_css_class("library-count");
        bottom.append(&count_label);
        overlay.add_overlay(&bottom);

        let selection_bar = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        selection_bar.add_css_class("selection-bar");
        selection_bar.set_halign(gtk::Align::Center);
        selection_bar.set_valign(gtk::Align::End);
        selection_bar.set_margin_bottom(54);
        selection_bar.set_visible(false);
        let selection_count = gtk::Label::new(Some("0 selected"));
        selection_count.add_css_class("selection-count");
        selection_bar.append(&selection_count);
        let selection_favorite = gtk::Button::with_label("Favorite");
        selection_favorite.add_css_class("selection-action");
        selection_bar.append(&selection_favorite);
        let selection_collection = gtk::Button::with_label("Add to collection");
        selection_collection.add_css_class("selection-action");
        selection_bar.append(&selection_collection);
        let selection_remove = gtk::Button::with_label("Remove");
        selection_remove.add_css_class("selection-action");
        selection_remove.add_css_class("danger");
        selection_bar.append(&selection_remove);
        let selection_cancel = gtk::Button::with_label("Cancel");
        selection_cancel.add_css_class("selection-action");
        selection_bar.append(&selection_cancel);
        overlay.add_overlay(&selection_bar);

        window.set_child(Some(&overlay));

        let state = Rc::new(LibraryState {
            window: window.clone(),
            app: app.clone(),
            library,
            books: RefCell::new(Vec::new()),
            section: RefCell::new(Section::All),
            query: RefCell::new(String::new()),
            store: store.clone(),
            grid: grid.clone(),
            empty_state: empty_state.clone(),
            count_label: count_label.clone(),
            status_label: status_label.clone(),
            series_nav: series_nav.clone(),
            folder_nav: folder_nav.clone(),
            collection_nav: collection_nav.clone(),
            back_button: back_button.clone(),
            opening_book: Cell::new(false),
            selection_mode: Cell::new(false),
            selected: RefCell::new(HashSet::new()),
            selection_bar: selection_bar.clone(),
            selection_count: selection_count.clone(),
            selection_favorite: selection_favorite.clone(),
            selection_collection: selection_collection.clone(),
            selection_remove: selection_remove.clone(),
            nav_buttons: RefCell::new(Vec::new()),
            scanning: Cell::new(false),
            cover_cache: RefCell::new(HashMap::new()),
            last_signature: RefCell::new(String::new()),
            nav_signature: RefCell::new(String::new()),
            watch_handle: RefCell::new(None),
            css_provider: provider.clone(),
            theme_path,
            theme_mtime: Cell::new(theme_mtime),
            theme_monitor: RefCell::new(None),
            theme_source: Cell::new(None),
            active_reader: RefCell::new(None),
        });

        factory.connect_setup(|_, item| {
            let Some(item) = item.downcast_ref::<gtk::ListItem>() else {
                return;
            };
            let host = gtk::Box::new(gtk::Orientation::Vertical, 0);
            // The host keeps the same size whether the card is bound or not.
            // Otherwise unbind shrinks the item, grid relayouts, and every row
            // is bound again in a loop.
            host.set_size_request(244, 398);
            item.set_child(Some(&host));
        });
        factory.connect_bind({
            let state = state.clone();
            move |_, item| {
                let Some(item) = item.downcast_ref::<gtk::ListItem>() else {
                    return;
                };
                let Some(object) = item.item().and_downcast::<glib::BoxedAnyObject>() else {
                    return;
                };
                let catalog_item = object.borrow::<CatalogItem>();
                let index = item.position() as usize;
                let Some(host) = item.child().and_downcast::<gtk::Box>() else {
                    return;
                };
                while let Some(child) = host.first_child() {
                    host.remove(&child);
                }
                let card = match &*catalog_item {
                    CatalogItem::Book(book) => {
                        book_card(&state, book, index).upcast::<gtk::Widget>()
                    }
                    CatalogItem::Collection(collection) => {
                        collection_card(&state, collection).upcast::<gtk::Widget>()
                    }
                };
                host.append(&card);
            }
        });
        factory.connect_unbind(|_, item| {
            if let Some(item) = item.downcast_ref::<gtk::ListItem>()
                && let Some(host) = item.child().and_downcast::<gtk::Box>()
            {
                while let Some(child) = host.first_child() {
                    host.remove(&child);
                }
            }
        });

        state.nav_buttons.borrow_mut().extend([
            (Section::Continue, continue_button.clone()),
            (Section::All, all_button.clone()),
            (Section::Favorites, favorites_button.clone()),
            (Section::Recent, recent_button.clone()),
        ]);
        connect_nav(&state, &continue_button, Section::Continue);
        connect_nav(&state, &all_button, Section::All);
        connect_nav(&state, &favorites_button, Section::Favorites);
        connect_nav(&state, &recent_button, Section::Recent);
        update_nav_active(&state);

        {
            let state = state.clone();
            search.connect_search_changed(move |entry| {
                *state.query.borrow_mut() = entry.text().to_string();
                refresh(&state);
            });
        }

        {
            let state = state.clone();
            add_folder.connect_clicked(move |_| choose_folder(&state));
        }

        {
            let state = state.clone();
            rescan.connect_clicked(move |_| start_scan(&state));
        }

        {
            let state = state.clone();
            stats.connect_clicked(move |_| show_stats(&state));
        }

        {
            let state = state.clone();
            settings.connect_clicked(move |_| show_settings(&state));
        }

        {
            let state = state.clone();
            let browse_popover = browse_popover.clone();
            new_collection.connect_clicked(move |_| {
                browse_popover.popdown();
                show_text_prompt(
                    &state,
                    "New collection",
                    "Collection name",
                    "Create",
                    |state, name| match state.library.create_collection(name) {
                        Ok(_) => {
                            state.nav_signature.borrow_mut().clear();
                            refresh_nav(state);
                        }
                        Err(error) => state.status_label.set_text(&format!("{error:#}")),
                    },
                );
            });
        }

        {
            let state = state.clone();
            new_collection_top.connect_clicked(move |_| {
                prompt_new_collection(&state, None);
            });
        }

        {
            let state = state.clone();
            back_button.connect_clicked(move |_| {
                *state.section.borrow_mut() = Section::All;
                state.query.borrow_mut().clear();
                state.last_signature.borrow_mut().clear();
                update_nav_active(&state);
                refresh(&state);
            });
        }

        {
            let state = state.clone();
            selection_favorite.connect_clicked(move |_| {
                let ids: Vec<i64> = state.selected.borrow().iter().copied().collect();
                for id in &ids {
                    if let Err(error) = state.library.set_favorite(*id, true) {
                        state.status_label.set_text(&format!("{error:#}"));
                        return;
                    }
                }
                state.last_signature.borrow_mut().clear();
                state.status_label.set_text("Marked as favorite");
                refresh(&state);
            });
        }

        {
            let state = state.clone();
            selection_collection.connect_clicked(move |_| {
                let ids: Vec<i64> = state.selected.borrow().iter().copied().collect();
                if ids.is_empty() {
                    return;
                }
                show_collection_picker(&state, ids);
            });
        }

        {
            let state = state.clone();
            selection_remove.connect_clicked(move |_| {
                let ids: Vec<i64> = state.selected.borrow().iter().copied().collect();
                if ids.is_empty() {
                    return;
                }
                confirm_remove_books(&state, ids);
            });
        }

        {
            let state = state.clone();
            selection_cancel.connect_clicked(move |_| exit_selection(&state));
        }

        {
            let state = Rc::downgrade(&state);
            let focus = gtk::EventControllerFocus::new();
            focus.connect_enter(move |_| {
                if let Some(state) = state.upgrade() {
                    glib::idle_add_local_once(move || refresh(&state));
                }
            });
            window.add_controller(focus);
        }

        refresh(&state);
        restart_watch(&state);
        start_scan(&state);
        watch_theme(&state);

        if let Some(path) = std::env::var_os("GRIMOIR_AUTO_OPEN") {
            let state = state.clone();
            glib::timeout_add_local_once(Duration::from_millis(700), move || {
                let path = PathBuf::from(path);
                if let Ok(Some(book)) = state.library.book_by_path(&path) {
                    open_book(&state, &book);
                }
            });
        }

        Ok(Self {
            window,
            _state: state,
        })
    }

    pub fn present(&self) {
        self.window.present();
        self._state.grid.grab_focus();
    }
}

fn filter_chip(label: &str) -> gtk::Button {
    let button = gtk::Button::with_label(label);
    button.add_css_class("library-chip");
    button
}

fn nav_button(label: &str) -> gtk::Button {
    let button = gtk::Button::with_label(label);
    button.add_css_class("library-menu-item");
    button.set_halign(gtk::Align::Fill);
    button.set_hexpand(true);
    button
}

fn connect_nav(state: &Rc<LibraryState>, button: &gtk::Button, section: Section) {
    let state = state.clone();
    button.connect_clicked(move |_| {
        *state.section.borrow_mut() = section.clone();
        update_nav_active(&state);
        refresh(&state);
    });
}

fn update_nav_active(state: &Rc<LibraryState>) {
    let current = state.section.borrow().clone();
    for (section, button) in state.nav_buttons.borrow().iter() {
        if *section == current {
            button.add_css_class("active");
        } else {
            button.remove_css_class("active");
        }
    }
}

fn format_duration(seconds: i64) -> String {
    let hours = seconds / 3600;
    let minutes = (seconds % 3600) / 60;
    if hours > 0 {
        format!("{hours}h {minutes:02}m")
    } else {
        format!("{minutes}m")
    }
}

fn show_stats(state: &Rc<LibraryState>) {
    let stats = match state.library.library_stats() {
        Ok(stats) => stats,
        Err(error) => {
            state
                .status_label
                .set_text(&format!("Could not load statistics: {error}"));
            return;
        }
    };
    let detail = format!(
        "{} books · {} reading · {} finished\n{} sessions · {} · {} pages",
        stats.total_books,
        stats.reading,
        stats.finished,
        stats.sessions,
        format_duration(stats.seconds),
        stats.pages
    );
    let dialog = gtk::MessageDialog::builder()
        .modal(true)
        .transient_for(&state.window)
        .text("Reading statistics")
        .secondary_text(detail)
        .build();
    dialog.connect_response(|dialog, _| dialog.close());
    dialog.present();
}

fn show_settings(state: &Rc<LibraryState>) {
    let window = gtk::Window::builder()
        .title("Settings")
        .modal(true)
        .decorated(false)
        .transient_for(&state.window)
        .default_width(560)
        .default_height(460)
        .build();
    window.add_css_class("book-dialog");

    let body = gtk::Box::new(gtk::Orientation::Vertical, 10);
    body.set_margin_top(46);
    body.set_margin_bottom(22);
    body.set_margin_start(24);
    body.set_margin_end(24);

    let heading = gtk::Label::new(Some("Settings"));
    heading.add_css_class("reader-title");
    heading.set_xalign(0.0);
    body.append(&heading);

    let roots = state.library.roots().unwrap_or_default();
    let roots_title = gtk::Label::new(Some("Library folders"));
    roots_title.set_xalign(0.0);
    roots_title.add_css_class("reader-muted");
    body.append(&roots_title);
    if roots.is_empty() {
        let empty = gtk::Label::new(Some("No folders added yet."));
        empty.set_xalign(0.0);
        body.append(&empty);
    } else {
        for root in &roots {
            let label = gtk::Label::new(Some(&root.display().to_string()));
            label.set_xalign(0.0);
            label.set_ellipsize(gtk::pango::EllipsizeMode::Middle);
            body.append(&label);
        }
    }
    let add = gtk::Button::with_label("Add folder…");
    add.set_halign(gtk::Align::Start);
    add.add_css_class("library-chip");
    {
        let state = state.clone();
        let window = window.clone();
        add.connect_clicked(move |_| {
            window.close();
            choose_folder(&state);
        });
    }
    body.append(&add);

    let vault_title = gtk::Label::new(Some("Obsidian vault"));
    vault_title.set_xalign(0.0);
    vault_title.add_css_class("reader-muted");
    body.append(&vault_title);
    let vault_value = state
        .library
        .setting("obsidian_vault")
        .ok()
        .flatten()
        .unwrap_or_else(|| "Not set".to_owned());
    let vault_label = gtk::Label::new(Some(&vault_value));
    vault_label.set_xalign(0.0);
    vault_label.set_ellipsize(gtk::pango::EllipsizeMode::Middle);
    body.append(&vault_label);
    let choose_vault = gtk::Button::with_label("Choose vault…");
    choose_vault.set_halign(gtk::Align::Start);
    choose_vault.add_css_class("library-chip");
    {
        let state = state.clone();
        let vault_label = vault_label.clone();
        choose_vault.connect_clicked(move |_| {
            let dialog = gtk::FileChooserNative::new(
                Some("Choose Obsidian vault"),
                Some(&state.window),
                gtk::FileChooserAction::SelectFolder,
                Some("Choose"),
                Some("Cancel"),
            );
            let state = state.clone();
            let vault_label = vault_label.clone();
            dialog.connect_response(move |dialog, response| {
                if response == gtk::ResponseType::Accept
                    && let Some(file) = dialog.file()
                    && let Some(path) = file.path()
                {
                    let _ = state
                        .library
                        .set_setting("obsidian_vault", &path.display().to_string());
                    vault_label.set_text(&path.display().to_string());
                }
                dialog.hide();
            });
            dialog.show();
        });
    }
    body.append(&choose_vault);

    let about = gtk::Label::new(Some("Grimoir 0.1.0 · GPL-3.0-or-later"));
    about.set_xalign(0.0);
    about.add_css_class("reader-muted");
    about.set_margin_top(10);
    body.append(&about);

    let close = gtk::Button::with_label("Close");
    close.set_halign(gtk::Align::End);
    close.set_margin_top(12);
    {
        let window = window.clone();
        close.connect_clicked(move |_| window.close());
    }
    body.append(&close);

    window.set_child(Some(&body));
    window.present();
}

fn choose_folder(state: &Rc<LibraryState>) {
    let dialog = gtk::FileChooserNative::new(
        Some("Add library folder"),
        Some(&state.window),
        gtk::FileChooserAction::SelectFolder,
        Some("Add"),
        Some("Cancel"),
    );
    let state = state.clone();
    dialog.connect_response(move |dialog, response| {
        if response == gtk::ResponseType::Accept
            && let Some(file) = dialog.file()
            && let Some(path) = file.path()
        {
            if !path.is_dir() {
                state
                    .status_label
                    .set_text(&format!("Not a folder: {}", path.display()));
            } else {
                match state.library.add_root(&path) {
                    Ok(_) => {
                        state
                            .status_label
                            .set_text(&format!("Added {}", path.display()));
                        refresh(&state);
                        restart_watch(&state);
                        start_scan(&state);
                    }
                    Err(error) => state.status_label.set_text(&format!("{error:#}")),
                }
            }
        }
        dialog.hide();
    });
    dialog.show();
}

fn refresh(state: &Rc<LibraryState>) {
    let books = match state.library.all_books() {
        Ok(books) => books,
        Err(error) => {
            state.status_label.set_text(&format!("{error:#}"));
            return;
        }
    };

    let section = state.section.borrow().clone();
    let query = state.query.borrow().to_lowercase();
    let collection_books = match &section {
        Section::Collection(id, _) => Some(
            state
                .library
                .book_ids_in_collection(*id)
                .unwrap_or_default()
                .into_iter()
                .collect::<HashSet<_>>(),
        ),
        _ => None,
    };

    let collections = state.library.collections().unwrap_or_default();
    let in_all = matches!(section, Section::All);
    let mut collected_ids = HashSet::new();
    let mut collection_cards = Vec::new();
    if in_all {
        for collection in &collections {
            let ids = state
                .library
                .book_ids_in_collection(collection.id)
                .unwrap_or_default();
            collected_ids.extend(ids.iter().copied());
            let members = ids
                .iter()
                .filter_map(|id| books.iter().find(|book| book.id == *id))
                .collect::<Vec<_>>();
            let matches = query.is_empty()
                || collection.name.to_lowercase().contains(&query)
                || members.iter().any(|book| book_matches(book, &query));
            if !matches {
                continue;
            }
            let covers = members
                .iter()
                .filter_map(|book| book.cover_path.clone())
                .take(3)
                .collect::<Vec<_>>();
            collection_cards.push(CollectionCard {
                id: collection.id,
                name: collection.name.clone(),
                count: members.len(),
                covers,
            });
        }
    }

    let mut visible = books
        .iter()
        .filter(|book| section_matches(book, &section, collection_books.as_ref()))
        .filter(|book| !(in_all && collected_ids.contains(&book.id)))
        .filter(|book| query.is_empty() || book_matches(book, &query))
        .cloned()
        .collect::<Vec<_>>();

    sort_books(&mut visible, &section);

    let count_text = if in_all && !collections.is_empty() {
        format!(
            "{} book{} · {} collection{}",
            visible.len(),
            if visible.len() == 1 { "" } else { "s" },
            collection_cards.len(),
            if collection_cards.len() == 1 { "" } else { "s" }
        )
    } else {
        match visible.len() {
            1 => "1 book".to_owned(),
            count => format!("{count} books"),
        }
    };
    state.count_label.set_text(&count_text);
    state
        .back_button
        .set_visible(matches!(section, Section::Collection(..)));

    let roots = state
        .library
        .roots()
        .unwrap_or_default()
        .iter()
        .map(|root| root.display().to_string())
        .collect::<Vec<_>>()
        .join("|");
    let cards = visible
        .iter()
        .map(|book| {
            format!(
                "{}:{}:{}:{}:{}",
                book.id,
                book.title,
                book.progress_page(),
                book.favorite,
                book.cover_path
                    .as_deref()
                    .map(|path| path.display().to_string())
                    .unwrap_or_default()
            )
        })
        .collect::<Vec<_>>()
        .join("|");
    let folders = collection_cards
        .iter()
        .map(|card| {
            format!(
                "{}:{}:{}:{}",
                card.id,
                card.name,
                card.count,
                card.covers
                    .iter()
                    .map(|path| path.display().to_string())
                    .collect::<Vec<_>>()
                    .join(",")
            )
        })
        .collect::<Vec<_>>()
        .join("|");
    let signature = format!("{roots}::{cards}::{folders}");

    if *state.last_signature.borrow() == signature {
        state.books.replace(books);
        refresh_nav(state);
        return;
    }
    *state.last_signature.borrow_mut() = signature;

    state.store.remove_all();
    for collection in &collection_cards {
        state
            .store
            .append(&glib::BoxedAnyObject::new(CatalogItem::Collection(
                collection.clone(),
            )));
    }
    for book in &visible {
        state
            .store
            .append(&glib::BoxedAnyObject::new(CatalogItem::Book(Box::new(
                book.clone(),
            ))));
    }

    while let Some(child) = state.empty_state.first_child() {
        state.empty_state.remove(&child);
    }
    let is_empty = visible.is_empty() && collection_cards.is_empty();
    state.empty_state.set_visible(is_empty);
    if is_empty {
        let has_roots = !state.library.roots().unwrap_or_default().is_empty();
        let title = gtk::Label::new(Some(if has_roots {
            "No books match this view."
        } else {
            "Your shelves are empty."
        }));
        title.add_css_class("library-empty");
        state.empty_state.append(&title);

        if !has_roots {
            let hint = gtk::Label::new(Some(
                "Point Grimoir at a folder of PDFs to build your library.",
            ));
            hint.add_css_class("library-empty-muted");
            hint.set_wrap(true);
            hint.set_justify(gtk::Justification::Center);
            state.empty_state.append(&hint);

            let add = gtk::Button::with_label("Add a folder");
            add.add_css_class("library-empty-action");
            add.set_halign(gtk::Align::Center);
            add.set_margin_top(8);
            add.connect_clicked({
                let state = state.clone();
                move |_| choose_folder(&state)
            });
            state.empty_state.append(&add);
        }
    }

    state.books.replace(books);
    refresh_nav(state);
}

fn section_matches(
    book: &Book,
    section: &Section,
    collection_books: Option<&HashSet<i64>>,
) -> bool {
    match section {
        Section::Continue => book.progress_percent > 0.0 && book.progress_percent < 0.995,
        Section::All => true,
        Section::Favorites => book.favorite,
        Section::Recent => book.last_opened_at.is_some(),
        Section::Series(series) => book.series.as_deref() == Some(series.as_str()),
        Section::Folder(path) => book.path.starts_with(path),
        Section::Collection(_, _) => collection_books.is_some_and(|ids| ids.contains(&book.id)),
    }
}

fn book_matches(book: &Book, query: &str) -> bool {
    book.title.to_lowercase().contains(query)
        || book
            .authors
            .as_deref()
            .is_some_and(|authors| authors.to_lowercase().contains(query))
        || book
            .series
            .as_deref()
            .is_some_and(|series| series.to_lowercase().contains(query))
        || book.path.to_string_lossy().to_lowercase().contains(query)
}

fn sort_books(books: &mut [Book], section: &Section) {
    match section {
        Section::Continue | Section::Recent => {
            books.sort_by_key(|book| std::cmp::Reverse(book.last_opened_at));
        }
        Section::Series(_) => {
            books.sort_by(|a, b| {
                a.series_index
                    .partial_cmp(&b.series_index)
                    .unwrap_or(std::cmp::Ordering::Equal)
                    .then_with(|| a.title.cmp(&b.title))
            });
        }
        _ => {
            books.sort_by(|a, b| a.title.to_lowercase().cmp(&b.title.to_lowercase()));
        }
    }
}

fn refresh_nav(state: &Rc<LibraryState>) {
    let mut series = state
        .books
        .borrow()
        .iter()
        .filter_map(|book| book.series.clone())
        .collect::<Vec<_>>();
    series.sort();
    series.dedup();
    let roots = state.library.roots().unwrap_or_default();
    let collections = state.library.collections().unwrap_or_default();
    let signature = format!(
        "{}::{}::{}",
        series.join("\u{1f}"),
        roots
            .iter()
            .map(|root| root.display().to_string())
            .collect::<Vec<_>>()
            .join("\u{1f}"),
        collections
            .iter()
            .map(|collection| format!("{}:{}", collection.id, collection.name))
            .collect::<Vec<_>>()
            .join("\u{1f}")
    );
    if *state.nav_signature.borrow() == signature {
        update_nav_active(state);
        return;
    }
    *state.nav_signature.borrow_mut() = signature;

    state.nav_buttons.borrow_mut().retain(|(section, _)| {
        matches!(
            section,
            Section::Continue | Section::All | Section::Favorites | Section::Recent
        )
    });
    while let Some(child) = state.series_nav.first_child() {
        state.series_nav.remove(&child);
    }
    for name in series {
        let button = nav_button(&name);
        connect_nav(state, &button, Section::Series(name.clone()));
        state
            .nav_buttons
            .borrow_mut()
            .push((Section::Series(name), button.clone()));
        state.series_nav.append(&button);
    }

    while let Some(child) = state.collection_nav.first_child() {
        state.collection_nav.remove(&child);
    }
    for Collection { id, name } in collections {
        let row = gtk::Box::new(gtk::Orientation::Horizontal, 2);
        let button = nav_button(&name);
        connect_nav(state, &button, Section::Collection(id, name.clone()));
        state
            .nav_buttons
            .borrow_mut()
            .push((Section::Collection(id, name), button.clone()));
        row.append(&button);

        let delete = gtk::Button::from_icon_name("user-trash-symbolic");
        delete.add_css_class("library-icon");
        delete.set_tooltip_text(Some("Delete collection"));
        delete.connect_clicked({
            let state = state.clone();
            move |_| {
                if let Err(error) = state.library.delete_collection(id) {
                    state.status_label.set_text(&format!("{error:#}"));
                    return;
                }
                if matches!(&*state.section.borrow(), Section::Collection(current, _) if *current == id) {
                    *state.section.borrow_mut() = Section::All;
                }
                state.nav_signature.borrow_mut().clear();
                state.last_signature.borrow_mut().clear();
                refresh(&state);
            }
        });
        row.append(&delete);
        state.collection_nav.append(&row);
    }

    while let Some(child) = state.folder_nav.first_child() {
        state.folder_nav.remove(&child);
    }
    for root in state.library.roots().unwrap_or_default() {
        let name = root
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_else(|| root.display().to_string());
        let row = gtk::Box::new(gtk::Orientation::Horizontal, 2);
        let button = nav_button(&name);
        button.set_tooltip_text(Some(&root.display().to_string()));
        connect_nav(state, &button, Section::Folder(root.clone()));
        state
            .nav_buttons
            .borrow_mut()
            .push((Section::Folder(root.clone()), button.clone()));
        row.append(&button);

        let remove = gtk::Button::from_icon_name("user-trash-symbolic");
        remove.add_css_class("library-icon");
        remove.set_tooltip_text(Some("Remove this folder from the library"));
        remove.connect_clicked({
            let state = state.clone();
            let root = root.clone();
            move |_| match state.library.forget_root(&root) {
                Ok(_) => {
                    state.cover_cache.borrow_mut().clear();
                    state.last_signature.borrow_mut().clear();
                    state.nav_signature.borrow_mut().clear();
                    restart_watch(&state);
                    start_scan(&state);
                    refresh(&state);
                }
                Err(error) => state.status_label.set_text(&format!("{error:#}")),
            }
        });
        row.append(&remove);
        state.folder_nav.append(&row);
    }
    update_nav_active(state);
}

fn book_card(state: &Rc<LibraryState>, book: &Book, index: usize) -> gtk::Overlay {
    let card = gtk::Button::new();
    card.add_css_class("showcase-card");
    card.set_size_request(224, 372);
    card.set_halign(gtk::Align::Center);
    card.set_overflow(gtk::Overflow::Visible);
    card.set_margin_start(10);
    card.set_margin_end(10);
    card.set_margin_top(10);
    card.set_margin_bottom(16);

    let content = gtk::Box::new(gtk::Orientation::Vertical, 7);
    content.set_halign(gtk::Align::Center);

    let stage = gtk::AspectFrame::new(0.5, 0.5, 172.0 / 258.0, false);
    stage.add_css_class("cover-stage");
    stage.set_size_request(172, 258);
    stage.set_halign(gtk::Align::Center);
    stage.set_valign(gtk::Align::Start);
    stage.set_hexpand(false);
    stage.set_vexpand(false);
    stage.set_overflow(gtk::Overflow::Visible);

    let cover = if let Some(cover_path) = &book.cover_path {
        let picture = gtk::Picture::new();
        picture.add_css_class("showcase-cover");
        picture.set_content_fit(gtk::ContentFit::Cover);
        picture.set_can_shrink(true);
        picture.set_size_request(172, 258);
        picture.set_halign(gtk::Align::Fill);
        picture.set_valign(gtk::Align::Fill);

        // Decode covers only when the card is actually mapped. GtkGridView may
        // bind every item once to measure the grid; doing image I/O there stalls
        // startup for seconds on a large library.
        let state_for_cover = state.clone();
        let cover_path = cover_path.clone();
        let book_id = book.id;
        picture.connect_map(move |picture| {
            if picture.paintable().is_some() {
                return;
            }
            if let Some(texture) = state_for_cover.cover_cache.borrow().get(&book_id).cloned() {
                picture.set_paintable(Some(&texture));
                return;
            }
            if let Ok(texture) = gdk::Texture::from_filename(&cover_path) {
                let mut cache = state_for_cover.cover_cache.borrow_mut();
                if cache.len() >= COVER_CACHE_LIMIT
                    && let Some(oldest) = cache.keys().next().copied()
                {
                    cache.remove(&oldest);
                }
                cache.insert(book_id, texture.clone());
                drop(cache);
                picture.set_paintable(Some(&texture));
            }
        });
        let cached = state.cover_cache.borrow().get(&book.id).cloned();
        if let Some(texture) = cached {
            picture.set_paintable(Some(&texture));
        }
        picture.upcast::<gtk::Widget>()
    } else {
        let placeholder = gtk::Label::new(Some("No cover"));
        placeholder.add_css_class("showcase-cover");
        placeholder.add_css_class("library-empty");
        placeholder.set_size_request(172, 258);
        placeholder.upcast::<gtk::Widget>()
    };
    cover.add_css_class(if index.is_multiple_of(2) {
        "tilt-left"
    } else {
        "tilt-right"
    });
    stage.set_child(Some(&cover));
    content.append(&stage);

    let shelf = gtk::Box::new(gtk::Orientation::Vertical, 0);
    shelf.add_css_class("cover-shelf");
    shelf.set_size_request(150, 12);
    content.append(&shelf);

    let title_text = if book.favorite {
        format!("★ {}", book.title)
    } else {
        book.title.clone()
    };
    let title = gtk::Label::new(Some(&title_text));
    title.add_css_class("showcase-title");
    title.set_wrap(true);
    title.set_lines(2);
    title.set_ellipsize(gtk::pango::EllipsizeMode::End);
    title.set_justify(gtk::Justification::Center);
    title.set_xalign(0.5);
    title.set_max_width_chars(24);
    content.append(&title);

    let author = gtk::Label::new(Some(&book.author_label()));
    author.add_css_class("showcase-muted");
    author.set_ellipsize(gtk::pango::EllipsizeMode::End);
    author.set_xalign(0.5);
    author.set_max_width_chars(24);
    content.append(&author);

    if let Some(series) = book.series_label() {
        let label = gtk::Label::new(Some(&series));
        label.add_css_class("showcase-muted");
        label.set_ellipsize(gtk::pango::EllipsizeMode::End);
        label.set_xalign(0.5);
        content.append(&label);
    }

    if book.progress_percent > 0.0 {
        let progress = gtk::ProgressBar::new();
        progress.add_css_class("showcase-progress");
        progress.set_fraction(book.progress_percent.clamp(0.0, 1.0));
        progress.set_size_request(120, 2);
        content.append(&progress);
    }

    card.set_child(Some(&content));

    let click_state = state.clone();
    let click_book = book.clone();
    card.connect_clicked(move |_| {
        if click_state.selection_mode.get() {
            toggle_selected(&click_state, click_book.id);
        } else {
            open_book(&click_state, &click_book);
        }
    });

    let overlay = gtk::Overlay::new();
    overlay.set_child(Some(&card));

    let actions = gtk::Box::new(gtk::Orientation::Horizontal, 2);
    actions.add_css_class("card-actions");
    actions.set_visible(!state.selection_mode.get());
    actions.set_halign(gtk::Align::End);
    actions.set_valign(gtk::Align::Start);
    actions.set_margin_top(5);
    actions.set_margin_end(5);

    let star = gtk::ToggleButton::new();
    star.add_css_class("card-action");
    star.add_css_class("favorite-star");
    star.set_active(book.favorite);
    star.set_icon_name(if book.favorite {
        "starred-symbolic"
    } else {
        "non-starred-symbolic"
    });
    star.set_tooltip_text(Some("Favorite"));
    star.connect_toggled({
        let state = state.clone();
        let book = book.clone();
        move |star| {
            let favorite = star.is_active();
            star.set_icon_name(if favorite {
                "starred-symbolic"
            } else {
                "non-starred-symbolic"
            });
            if let Err(error) = state.library.set_favorite(book.id, favorite) {
                state.status_label.set_text(&format!("{error:#}"));
                return;
            }
            state.last_signature.borrow_mut().clear();
            let state = state.clone();
            glib::idle_add_local_once(move || refresh(&state));
        }
    });
    actions.append(&star);

    let menu = gtk::MenuButton::new();
    menu.add_css_class("card-menu-button");
    menu.set_icon_name("view-more-symbolic");
    menu.set_tooltip_text(Some("Book actions"));
    let popover = gtk::Popover::new();
    popover.add_css_class("reader-popover");
    popover.add_css_class("card-menu");
    popover.set_has_arrow(false);
    popover.set_position(gtk::PositionType::Bottom);
    let menu_box = gtk::Box::new(gtk::Orientation::Vertical, 2);
    menu_box.set_margin_top(6);
    menu_box.set_margin_bottom(6);
    menu_box.set_margin_start(6);
    menu_box.set_margin_end(6);
    menu_box.set_width_request(160);
    let details = gtk::Button::with_label("Edit details…");
    details.add_css_class("library-menu-item");
    details.set_halign(gtk::Align::Fill);
    if let Some(label) = details.child().and_downcast::<gtk::Label>() {
        label.set_xalign(0.0);
    }
    details.connect_clicked({
        let state = state.clone();
        let book = book.clone();
        let popover = popover.clone();
        move |_| {
            popover.popdown();
            show_book_details(&state, &book);
        }
    });
    let add_to_collection = gtk::Button::with_label("Add to collection…");
    add_to_collection.add_css_class("library-menu-item");
    add_to_collection.set_halign(gtk::Align::Fill);
    if let Some(label) = add_to_collection.child().and_downcast::<gtk::Label>() {
        label.set_xalign(0.0);
    }
    add_to_collection.connect_clicked({
        let state = state.clone();
        let book_id = book.id;
        let popover = popover.clone();
        move |_| {
            popover.popdown();
            show_collection_picker(&state, vec![book_id]);
        }
    });
    menu_box.append(&add_to_collection);

    let select_book = gtk::Button::with_label("Select book");
    select_book.add_css_class("library-menu-item");
    select_book.set_halign(gtk::Align::Fill);
    if let Some(label) = select_book.child().and_downcast::<gtk::Label>() {
        label.set_xalign(0.0);
    }
    select_book.connect_clicked({
        let state = state.clone();
        let book_id = book.id;
        let popover = popover.clone();
        move |_| {
            popover.popdown();
            enter_selection(&state, book_id);
        }
    });
    menu_box.append(&select_book);
    menu_box.append(&details);
    popover.set_child(Some(&menu_box));
    menu.set_popover(Some(&popover));
    actions.append(&menu);

    overlay.add_overlay(&actions);

    if state.selection_mode.get() {
        let check = gtk::CheckButton::new();
        check.add_css_class("select-check");
        check.set_active(state.selected.borrow().contains(&book.id));
        check.set_halign(gtk::Align::Start);
        check.set_valign(gtk::Align::Start);
        check.set_margin_top(8);
        check.set_margin_start(8);
        check.set_tooltip_text(Some("Select book"));
        check.connect_toggled({
            let state = state.clone();
            let book_id = book.id;
            move |check| {
                if check.is_active() {
                    state.selected.borrow_mut().insert(book_id);
                } else {
                    state.selected.borrow_mut().remove(&book_id);
                }
                update_selection_bar(&state);
            }
        });
        overlay.add_overlay(&check);
    }

    overlay
}

fn prompt_new_collection(state: &Rc<LibraryState>, after_ids: Option<Vec<i64>>) {
    show_text_prompt(
        state,
        "New collection",
        "Collection name",
        "Create",
        move |state, name| match state.library.create_collection(name) {
            Ok(collection) => {
                if let Some(ids) = &after_ids
                    && let Err(error) = state.library.add_books_to_collection(ids, collection.id)
                {
                    state.status_label.set_text(&format!("{error:#}"));
                    return;
                }
                state.nav_signature.borrow_mut().clear();
                state.last_signature.borrow_mut().clear();
                if after_ids.is_some() {
                    state.selection_mode.set(false);
                    state.selected.borrow_mut().clear();
                }
                refresh_nav(state);
                refresh(state);
                update_selection_bar(state);
                state.status_label.set_text("Collection created");
            }
            Err(error) => state.status_label.set_text(&format!("{error:#}")),
        },
    );
}

fn show_collection_picker(state: &Rc<LibraryState>, book_ids: Vec<i64>) {
    let collections = state.library.collections().unwrap_or_default();
    if collections.is_empty() {
        prompt_new_collection(state, Some(book_ids));
        return;
    }

    let window = gtk::Window::builder()
        .title("Add to collection")
        .modal(true)
        .decorated(false)
        .transient_for(&state.window)
        .default_width(380)
        .default_height(420)
        .build();
    window.add_css_class("book-dialog");
    let body = gtk::Box::new(gtk::Orientation::Vertical, 6);
    body.set_margin_top(46);
    body.set_margin_bottom(18);
    body.set_margin_start(20);
    body.set_margin_end(20);
    let heading = gtk::Label::new(Some(if book_ids.len() == 1 {
        "Add to collection"
    } else {
        "Add selected books to collection"
    }));
    heading.add_css_class("reader-title");
    heading.set_xalign(0.0);
    heading.set_margin_bottom(8);
    body.append(&heading);

    for collection in collections {
        let button = gtk::Button::with_label(&collection.name);
        button.add_css_class("library-menu-item");
        button.set_halign(gtk::Align::Fill);
        if let Some(label) = button.child().and_downcast::<gtk::Label>() {
            label.set_xalign(0.0);
        }
        button.connect_clicked({
            let state = state.clone();
            let ids = book_ids.clone();
            let window = window.clone();
            move |_| {
                if let Err(error) = state.library.add_books_to_collection(&ids, collection.id) {
                    state.status_label.set_text(&format!("{error:#}"));
                    return;
                }
                window.close();
                state.selection_mode.set(false);
                state.selected.borrow_mut().clear();
                state.last_signature.borrow_mut().clear();
                refresh(&state);
                update_selection_bar(&state);
                state.status_label.set_text("Added to collection");
            }
        });
        body.append(&button);
    }

    let cancel = gtk::Button::with_label("Cancel");
    cancel.add_css_class("library-menu-item");
    cancel.set_margin_top(8);
    cancel.connect_clicked({
        let window = window.clone();
        move |_| window.close()
    });
    body.append(&cancel);
    window.set_child(Some(&body));
    window.present();
}

fn enter_selection(state: &Rc<LibraryState>, book_id: i64) {
    state.selection_mode.set(true);
    state.selected.borrow_mut().clear();
    state.selected.borrow_mut().insert(book_id);
    state.last_signature.borrow_mut().clear();
    refresh(state);
    update_selection_bar(state);
}

fn toggle_selected(state: &Rc<LibraryState>, book_id: i64) {
    if !state.selected.borrow_mut().remove(&book_id) {
        state.selected.borrow_mut().insert(book_id);
    }
    update_selection_bar(state);
}

fn exit_selection(state: &Rc<LibraryState>) {
    state.selection_mode.set(false);
    state.selected.borrow_mut().clear();
    state.last_signature.borrow_mut().clear();
    refresh(state);
    update_selection_bar(state);
}

fn update_selection_bar(state: &Rc<LibraryState>) {
    let active = state.selection_mode.get();
    let count = state.selected.borrow().len();
    state.selection_bar.set_visible(active);
    state.selection_count.set_text(&format!(
        "{count} book{} selected",
        if count == 1 { "" } else { "s" }
    ));
    state.selection_favorite.set_sensitive(count > 0);
    state.selection_collection.set_sensitive(count > 0);
    state.selection_remove.set_sensitive(count > 0);
}

fn confirm_remove_books(state: &Rc<LibraryState>, ids: Vec<i64>) {
    let dialog = gtk::MessageDialog::builder()
        .modal(true)
        .transient_for(&state.window)
        .text(if ids.len() == 1 {
            "Remove this book from the library?"
        } else {
            "Remove these books from the library?"
        })
        .secondary_text("The PDF/CBZ/EPUB files stay on disk. Only the library entry is removed.")
        .build();
    dialog.add_button("Cancel", gtk::ResponseType::Cancel);
    dialog.add_button("Remove", gtk::ResponseType::Yes);
    dialog.connect_response({
        let state = state.clone();
        move |dialog, response| {
            if response == gtk::ResponseType::Yes {
                if let Err(error) = state.library.remove_books(&ids) {
                    state.status_label.set_text(&format!("{error:#}"));
                } else {
                    state.selected.borrow_mut().clear();
                    state.selection_mode.set(false);
                    state.last_signature.borrow_mut().clear();
                    refresh(&state);
                    update_selection_bar(&state);
                    state.status_label.set_text("Removed from library");
                }
            }
            dialog.close();
        }
    });
    dialog.present();
}

fn collection_card(state: &Rc<LibraryState>, collection: &CollectionCard) -> gtk::Button {
    let card = gtk::Button::new();
    card.add_css_class("showcase-card");
    card.add_css_class("collection-card");
    card.set_size_request(224, 372);
    card.set_halign(gtk::Align::Center);
    card.set_overflow(gtk::Overflow::Visible);
    card.set_margin_start(10);
    card.set_margin_end(10);
    card.set_margin_top(10);
    card.set_margin_bottom(16);

    let content = gtk::Box::new(gtk::Orientation::Vertical, 7);
    content.set_halign(gtk::Align::Center);

    let stage = gtk::Overlay::new();
    stage.add_css_class("collection-stage");
    stage.set_size_request(172, 258);
    stage.set_halign(gtk::Align::Center);
    stage.set_valign(gtk::Align::Start);

    let preview = gtk::Fixed::new();
    preview.set_hexpand(true);
    preview.set_vexpand(true);
    if collection.covers.is_empty() {
        let empty = gtk::Label::new(Some("Empty collection"));
        empty.add_css_class("library-empty-muted");
        empty.set_halign(gtk::Align::Center);
        empty.set_valign(gtk::Align::Center);
        preview.put(&empty, 18.0, 110.0);
    } else {
        for (index, path) in collection.covers.iter().take(3).enumerate() {
            let picture = gtk::Picture::new();
            picture.add_css_class("collection-cover");
            picture.set_content_fit(gtk::ContentFit::Cover);
            picture.set_can_shrink(true);
            picture.set_size_request(92, 132);
            if let Ok(texture) = gdk::Texture::from_filename(path) {
                picture.set_paintable(Some(&texture));
            }
            preview.put(
                &picture,
                6.0 + index as f64 * 31.0,
                44.0 - index as f64 * 8.0,
            );
        }
    }
    stage.set_child(Some(&preview));

    let tab = gtk::Box::new(gtk::Orientation::Horizontal, 4);
    tab.add_css_class("collection-tab");
    tab.set_halign(gtk::Align::Start);
    tab.set_valign(gtk::Align::Start);
    tab.append(&gtk::Image::from_icon_name("folder-symbolic"));
    tab.append(&gtk::Label::new(Some("COLLECTION")));
    stage.add_overlay(&tab);

    let count = gtk::Label::new(Some(&format!(
        "{} book{}",
        collection.count,
        if collection.count == 1 { "" } else { "s" }
    )));
    count.add_css_class("collection-count");
    count.set_halign(gtk::Align::End);
    count.set_valign(gtk::Align::End);
    count.set_margin_end(10);
    count.set_margin_bottom(8);
    stage.add_overlay(&count);

    content.append(&stage);

    let title = gtk::Label::new(Some(&collection.name));
    title.add_css_class("showcase-title");
    title.set_ellipsize(gtk::pango::EllipsizeMode::End);
    title.set_justify(gtk::Justification::Center);
    title.set_lines(2);
    title.set_xalign(0.5);
    title.set_max_width_chars(24);
    content.append(&title);

    let muted = gtk::Label::new(Some("Collection"));
    muted.add_css_class("showcase-muted");
    muted.set_xalign(0.5);
    content.append(&muted);
    card.set_child(Some(&content));

    card.connect_clicked({
        let state = state.clone();
        let id = collection.id;
        let name = collection.name.clone();
        let card = card.clone();
        move |_| {
            card.add_css_class("collection-opening");
            let state = state.clone();
            let name = name.clone();
            let card = card.clone();
            glib::timeout_add_local_once(std::time::Duration::from_millis(140), move || {
                *state.section.borrow_mut() = Section::Collection(id, name);
                state.query.borrow_mut().clear();
                state.last_signature.borrow_mut().clear();
                update_nav_active(&state);
                refresh(&state);
                card.remove_css_class("collection-opening");
            });
        }
    });

    card
}

fn open_book(state: &Rc<LibraryState>, book: &Book) {
    if state.opening_book.get() {
        return;
    }
    if let Some(reader) = state.active_reader.borrow().as_ref() {
        reader.window().present();
        return;
    }
    state.opening_book.set(true);
    state.status_label.set_text("Opening…");

    let state = state.clone();
    let book = book.clone();
    glib::timeout_add_local_once(Duration::from_millis(250), move || {
        let opened = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            match book.format.as_str() {
                "cbz" => CbzReaderWindow::open(
                    &state.app,
                    &book.path,
                    Some(state.library.clone()),
                    Some(book.id),
                )
                .map(ActiveReader::Cbz),
                "epub" => EpubReaderWindow::open(
                    &state.app,
                    &book.path,
                    Some(state.library.clone()),
                    Some(book.id),
                )
                .map(ActiveReader::Epub),
                _ => ReaderWindow::open(
                    &state.app,
                    &book.path,
                    Some(state.library.clone()),
                    Some(book.id),
                )
                .map(ActiveReader::Pdf),
            }
        }));
        match opened {
            Ok(Ok(reader)) => {
                state.opening_book.set(false);
                let window = reader.window().clone();
                let weak_state = Rc::downgrade(&state);
                window.connect_destroy(move |_| {
                    if let Some(state) = weak_state.upgrade() {
                        state.active_reader.borrow_mut().take();
                    }
                });
                *state.active_reader.borrow_mut() = Some(reader);
                window.present();
                window.fullscreen();
                state.status_label.set_text("");
            }
            Ok(Err(error)) => {
                state.opening_book.set(false);
                state.status_label.set_text(&format!("{error:#}"));
            }
            Err(payload) => {
                state.opening_book.set(false);
                let message = payload
                    .downcast_ref::<&str>()
                    .copied()
                    .or_else(|| payload.downcast_ref::<String>().map(String::as_str))
                    .unwrap_or("unknown error");
                state
                    .status_label
                    .set_text(&format!("Failed to open book: {message}"));
                eprintln!("reader panic: {message}");
            }
        }
    });
}

fn show_book_details(state: &Rc<LibraryState>, book: &Book) {
    let window = gtk::Window::builder()
        .title("Book details")
        .modal(true)
        .decorated(false)
        .transient_for(&state.window)
        .default_width(540)
        .default_height(720)
        .build();
    window.add_css_class("book-dialog");

    let body = gtk::Box::new(gtk::Orientation::Vertical, 10);
    body.set_margin_top(46);
    body.set_margin_bottom(20);
    body.set_margin_start(22);
    body.set_margin_end(22);

    let heading = gtk::Label::new(Some("Book details"));
    heading.add_css_class("reader-title");
    heading.set_xalign(0.0);
    heading.set_margin_bottom(4);
    body.append(&heading);

    let title = gtk::Entry::new();
    title.set_text(&book.title);
    title.set_placeholder_text(Some("Title"));
    body.append(&labeled("Title", &title));

    let authors = gtk::Entry::new();
    authors.set_text(book.authors.as_deref().unwrap_or(""));
    authors.set_placeholder_text(Some("Author"));
    body.append(&labeled("Author", &authors));

    let series = gtk::Entry::new();
    series.set_text(book.series.as_deref().unwrap_or(""));
    series.set_placeholder_text(Some("Series"));
    body.append(&labeled("Series", &series));

    let series_index = gtk::SpinButton::with_range(0.0, 999.0, 1.0);
    series_index.set_digits(1);
    series_index.set_numeric(true);
    if let Some(index) = book.series_index {
        series_index.set_value(index);
    }
    body.append(&labeled("Series number (0 = none)", &series_index));

    let status = gtk::ComboBoxText::new();
    for (id, label) in [
        ("unread", "Unread"),
        ("reading", "Reading"),
        ("finished", "Finished"),
    ] {
        status.append(Some(id), label);
    }
    status.set_active_id(Some(&book.reading_status));
    body.append(&labeled("Reading status", &status));

    let collections_label = gtk::Label::new(Some("Collections"));
    collections_label.set_xalign(0.0);
    collections_label.add_css_class("reader-muted");
    body.append(&collections_label);

    let collections_box = gtk::Box::new(gtk::Orientation::Vertical, 4);
    let mut collection_checks = Vec::new();
    let selected = state
        .library
        .collections_for_book(book.id)
        .unwrap_or_default();
    let all_collections = state.library.collections().unwrap_or_default();
    for collection in &all_collections {
        let check = gtk::CheckButton::with_label(&collection.name);
        check.set_active(selected.contains(&collection.id));
        collections_box.append(&check);
        collection_checks.push((collection.id, check));
    }
    if all_collections.is_empty() {
        let empty = gtk::Label::new(Some("No collections yet."));
        empty.add_css_class("library-status");
        empty.set_xalign(0.0);
        collections_box.append(&empty);
    }
    body.append(&collections_box);

    let new_collection = gtk::Button::with_label("New collection…");
    new_collection.add_css_class("library-menu-item");
    new_collection.set_halign(gtk::Align::Start);
    new_collection.connect_clicked({
        let state = state.clone();
        move |_| {
            show_text_prompt(
                &state,
                "New collection",
                "Collection name",
                "Create",
                |state, name| match state.library.create_collection(name) {
                    Ok(_) => {
                        state.nav_signature.borrow_mut().clear();
                        state
                            .status_label
                            .set_text("Collection created. Reopen details to assign it.");
                    }
                    Err(error) => state.status_label.set_text(&format!("{error:#}")),
                },
            );
        }
    });
    body.append(&new_collection);

    let buttons = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    buttons.set_halign(gtk::Align::End);
    buttons.set_margin_top(8);
    let cancel = gtk::Button::with_label("Cancel");
    let save = gtk::Button::with_label("Save");
    save.add_css_class("library-empty-action");
    buttons.append(&cancel);
    buttons.append(&save);
    body.append(&buttons);

    window.set_child(Some(&body));
    cancel.connect_clicked({
        let window = window.clone();
        move |_| window.close()
    });
    save.connect_clicked({
        let state = state.clone();
        let book = book.clone();
        let window = window.clone();
        move |_| {
            let title_text = title.text().trim().to_owned();
            let authors_text = authors.text().trim().to_owned();
            let series_text = series.text().trim().to_owned();
            let index_value = series_index.value();
            let reading_status = status
                .active_id()
                .map(|id| id.to_string())
                .unwrap_or_else(|| "unread".to_owned());
            let selected_ids = collection_checks
                .iter()
                .filter(|(_, check)| check.is_active())
                .map(|(id, _)| *id)
                .collect::<Vec<_>>();
            if let Err(error) = state.library.update_book_metadata(
                book.id,
                &title_text,
                (!authors_text.is_empty()).then_some(authors_text.as_str()),
                (!series_text.is_empty()).then_some(series_text.as_str()),
                (index_value > 0.0).then_some(index_value),
                &reading_status,
            ) {
                state.status_label.set_text(&format!("{error:#}"));
                return;
            }
            if let Err(error) = state.library.set_book_collections(book.id, &selected_ids) {
                state.status_label.set_text(&format!("{error:#}"));
                return;
            }
            state.last_signature.borrow_mut().clear();
            state.nav_signature.borrow_mut().clear();
            refresh(&state);
            window.close();
        }
    });
    window.present();
}

fn labeled<W: IsA<gtk::Widget>>(text: &str, widget: &W) -> gtk::Box {
    let box_ = gtk::Box::new(gtk::Orientation::Vertical, 3);
    let label = gtk::Label::new(Some(text));
    label.add_css_class("reader-muted");
    label.set_xalign(0.0);
    box_.append(&label);
    box_.append(widget);
    box_
}

fn show_text_prompt<F>(
    state: &Rc<LibraryState>,
    title: &str,
    placeholder: &str,
    action: &str,
    on_accept: F,
) where
    F: Fn(&Rc<LibraryState>, &str) + 'static,
{
    let window = gtk::Window::builder()
        .title(title)
        .modal(true)
        .decorated(false)
        .transient_for(&state.window)
        .default_width(380)
        .default_height(170)
        .build();
    window.add_css_class("book-dialog");
    let body = gtk::Box::new(gtk::Orientation::Vertical, 10);
    body.set_margin_top(46);
    body.set_margin_bottom(18);
    body.set_margin_start(18);
    body.set_margin_end(18);
    let entry = gtk::Entry::new();
    entry.set_placeholder_text(Some(placeholder));
    body.append(&entry);
    let buttons = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    buttons.set_halign(gtk::Align::End);
    let cancel = gtk::Button::with_label("Cancel");
    let accept = gtk::Button::with_label(action);
    accept.add_css_class("library-empty-action");
    buttons.append(&cancel);
    buttons.append(&accept);
    body.append(&buttons);
    window.set_child(Some(&body));

    cancel.connect_clicked({
        let window = window.clone();
        move |_| window.close()
    });
    accept.connect_clicked({
        let state = state.clone();
        let window = window.clone();
        let entry = entry.clone();
        move |_| {
            let value = entry.text().trim().to_owned();
            if !value.is_empty() {
                on_accept(&state, &value);
                refresh_nav(&state);
            }
            window.close();
        }
    });
    entry.connect_activate({
        let accept = accept.clone();
        move |_| accept.emit_clicked()
    });
    let escape = gtk::EventControllerKey::new();
    escape.connect_key_pressed({
        let window = window.clone();
        move |_, key, _, _| {
            if key == gdk::Key::Escape {
                window.close();
                glib::Propagation::Stop
            } else {
                glib::Propagation::Proceed
            }
        }
    });
    window.add_controller(escape);
    window.present();
    let entry = entry.clone();
    glib::idle_add_local_once(move || {
        entry.grab_focus();
    });
}

fn start_scan(state: &Rc<LibraryState>) {
    if state.scanning.replace(true) {
        return;
    }
    let roots = state.library.roots().unwrap_or_default();
    if roots.is_empty() {
        state.scanning.set(false);
        state
            .status_label
            .set_text("Add a folder to start building your library.");
        return;
    }

    state.status_label.set_text("Scanning library…");
    let (tx, rx) = async_channel::unbounded();
    let _ = scan::spawn(state.library.clone(), roots, tx);
    let weak = Rc::downgrade(state);
    glib::spawn_future_local(async move {
        while let Ok(event) = rx.recv().await {
            let Some(state) = weak.upgrade() else {
                break;
            };
            match event {
                ScanEvent::Started { total } => {
                    state
                        .status_label
                        .set_text(&format!("Scanning 0 / {total}…"));
                }
                ScanEvent::Progress { done, total } => {
                    state
                        .status_label
                        .set_text(&format!("Scanning {done} / {total}…"));
                }
                ScanEvent::Finished {
                    total,
                    added,
                    updated,
                    removed,
                    failed,
                    errors,
                } => {
                    state.scanning.set(false);
                    state.status_label.set_text(&format!(
                        "{total} files · {added} added · {updated} updated · {removed} removed · {failed} failed"
                    ));
                    if let Some(error) = errors.first() {
                        eprintln!("library scan: {error}");
                    }
                    // Covers can be regenerated under the same path, so drop the
                    // decoded textures and force the grid to rebuild from disk.
                    state.cover_cache.borrow_mut().clear();
                    state.last_signature.borrow_mut().clear();
                    refresh(&state);
                }
            }
        }
    });
}

fn restart_watch(state: &Rc<LibraryState>) {
    *state.watch_handle.borrow_mut() = None;
    let roots = state
        .library
        .roots()
        .unwrap_or_default()
        .into_iter()
        .filter(|root| root.exists())
        .collect::<Vec<_>>();
    if roots.is_empty() {
        return;
    }

    let (tx, rx) = async_channel::unbounded();
    match watch::watch(&roots, tx) {
        Ok(handle) => {
            *state.watch_handle.borrow_mut() = Some(handle);
            let weak = Rc::downgrade(state);
            glib::spawn_future_local(async move {
                while let Ok(WatchEvent::Changed) = rx.recv().await {
                    if let Some(state) = weak.upgrade() {
                        start_scan(&state);
                    }
                }
            });
        }
        Err(error) => state
            .status_label
            .set_text(&format!("Watch error: {error:#}")),
    }
}

fn load_theme() -> (Theme, PathBuf) {
    let config = config_dir().join("grimoir/theme.json");
    if let Some(path) = std::env::var_os("GRIMOIR_THEME") {
        let path = PathBuf::from(path);
        return (Theme::load(&path).unwrap_or_default(), path);
    }
    let hypr = std::env::var_os("GRIMOIR_HYPR_THEME")
        .map(PathBuf::from)
        .unwrap_or_else(|| home_dir().join(".local/state/hypr/theme.json"));
    for path in [&config, &hypr] {
        if let Ok(theme) = Theme::load(path) {
            return (theme, path.clone());
        }
    }
    (Theme::default(), config)
}

fn watch_theme(state: &Rc<LibraryState>) {
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
            let weak = Rc::downgrade(state);
            let target = state.theme_path.clone();
            monitor.connect_changed(move |_, file, _other, _event| {
                if file.path().as_deref() == Some(target.as_path())
                    && let Some(state) = weak.upgrade()
                {
                    schedule_theme_reload(&state);
                }
            });
            *state.theme_monitor.borrow_mut() = Some(monitor);
        }
        Err(error) => eprintln!(
            "failed to watch theme directory {}: {error}",
            parent.display()
        ),
    }
}

fn schedule_theme_reload(state: &Rc<LibraryState>) {
    if let Some(source) = state.theme_source.take() {
        source.remove();
    }
    let pending = state.clone();
    let source = glib::timeout_add_local_once(Duration::from_millis(140), move || {
        pending.theme_source.set(None);
        reload_theme(&pending);
    });
    state.theme_source.set(Some(source));
}

fn reload_theme(state: &Rc<LibraryState>) {
    if !state.theme_path.exists() {
        return;
    }
    let modified = std::fs::metadata(&state.theme_path)
        .ok()
        .and_then(|metadata| metadata.modified().ok());
    if modified.is_some() && modified == state.theme_mtime.get() {
        return;
    }
    match Theme::load(&state.theme_path) {
        Ok(theme) => {
            state
                .css_provider
                .load_from_data(&theme.css(gtk::check_version(4, 24, 0).is_some()));
            state.theme_mtime.set(modified);
        }
        Err(error) => eprintln!("library theme reload failed: {error:#}"),
    }
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
