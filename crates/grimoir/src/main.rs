mod canvas;
mod cbz_reader;
mod epub_reader;
mod library;
mod obsidian;
mod reader;

use std::cell::RefCell;
use std::path::PathBuf;
use std::sync::Arc;

use grimoir_library::Library;
use gtk::gio;
use gtk::glib;
use gtk::prelude::*;

const APP_ID: &str = "io.github.vertigofromouterspace.Grimoir";

thread_local! {
    static LIBRARY_WINDOW: RefCell<Option<library::LibraryWindow>> = const { RefCell::new(None) };
}

fn main() -> glib::ExitCode {
    std::panic::set_hook(Box::new(|info| {
        use std::io::Write;
        let message = format!(
            "{info}\nbacktrace:\n{}\n",
            std::backtrace::Backtrace::force_capture()
        );
        eprintln!("{message}");
        if let Ok(mut file) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(std::env::temp_dir().join("grimoir-panic.log"))
        {
            let _ = file.write_all(message.as_bytes());
        }
    }));

    let app = gtk::Application::builder()
        .application_id(APP_ID)
        .flags(gio::ApplicationFlags::HANDLES_OPEN)
        .build();

    app.connect_activate(|app| {
        open_library(app);
    });

    app.connect_open(|app, files, _hint| {
        let Some(file) = files.first() else {
            open_library(app);
            return;
        };
        let Some(path) = file.path() else {
            reader::show_error(app, "Grimoir can only open local files.");
            return;
        };
        open_path(app, path);
    });

    app.run()
}

fn open_library(app: &gtk::Application) {
    if LIBRARY_WINDOW.with(|slot| {
        if let Some(window) = slot.borrow().as_ref() {
            window.present();
            true
        } else {
            false
        }
    }) {
        return;
    }

    match Library::open_default() {
        Ok(library) => match library::LibraryWindow::open(app, Arc::new(library)) {
            Ok(window) => {
                window.present();
                LIBRARY_WINDOW.with(|slot| *slot.borrow_mut() = Some(window));
            }
            Err(error) => {
                eprintln!("{error:#}");
                reader::show_error(app, &format!("{error:#}"));
            }
        },
        Err(error) => {
            eprintln!("{error:#}");
            reader::show_error(app, &format!("{error:#}"));
        }
    }
}

fn open_path(app: &gtk::Application, path: PathBuf) {
    let canonical = std::fs::canonicalize(&path).unwrap_or_else(|_| path.clone());
    let (library, book_id) = match Library::open_default() {
        Ok(library) => {
            let library = Arc::new(library);
            let book_id = library
                .book_by_path(&canonical)
                .ok()
                .flatten()
                .map(|book| book.id);
            (Some(library), book_id)
        }
        Err(_) => (None, None),
    };

    let extension = path
        .extension()
        .and_then(|extension| extension.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase();
    let opened = match extension.as_str() {
        "cbz" => cbz_reader::CbzReaderWindow::open(app, &path, library, book_id)
            .map(|window| window.window().clone()),
        "epub" => epub_reader::EpubReaderWindow::open(app, &path, library, book_id)
            .map(|window| window.window().clone()),
        _ => reader::ReaderWindow::open(app, &path, library, book_id)
            .map(|window| window.window().clone()),
    };
    match opened {
        Ok(window) => {
            window.present();
            window.fullscreen();
        }
        Err(error) => {
            eprintln!("{error:#}");
            reader::show_error(app, &format!("{error:#}"));
        }
    }
}
