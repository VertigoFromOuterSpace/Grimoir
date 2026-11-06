//! Background library scanning and cover extraction.

use std::collections::HashSet;
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, anyhow};
use grimoir_pdf::PdfBook;
use walkdir::WalkDir;

use crate::parse;
use crate::{Book, Library, ScannedBook};

#[derive(Debug, Clone)]
pub enum ScanEvent {
    Started {
        total: usize,
    },
    Progress {
        done: usize,
        total: usize,
    },
    Finished {
        total: usize,
        added: usize,
        updated: usize,
        removed: usize,
        failed: usize,
        errors: Vec<String>,
    },
}

pub fn spawn(
    library: Arc<Library>,
    roots: Vec<PathBuf>,
    events: async_channel::Sender<ScanEvent>,
) -> std::thread::JoinHandle<()> {
    std::thread::Builder::new()
        .name("grimoir-library-scan".to_owned())
        .spawn(move || {
            let result = scan(&library, &roots, &events);
            if let Err(error) = result {
                let _ = events.send_blocking(ScanEvent::Finished {
                    total: 0,
                    added: 0,
                    updated: 0,
                    removed: 0,
                    failed: 0,
                    errors: vec![format!("{error:#}")],
                });
            }
        })
        .expect("failed to spawn library scanner")
}

fn scan(
    library: &Library,
    roots: &[PathBuf],
    events: &async_channel::Sender<ScanEvent>,
) -> Result<()> {
    let (files, walk_errors) = collect_books(roots);
    let total = files.len();
    if events.send_blocking(ScanEvent::Started { total }).is_err() {
        return Ok(());
    }

    let mut seen = Vec::with_capacity(total);
    let mut added = 0;
    let mut updated = 0;
    let mut failed = 0;
    let mut errors = walk_errors;

    for (done, path) in files.iter().enumerate() {
        // A scan may still be running when the user removes a root. Never
        // reinsert a book whose root is no longer configured.
        let active_roots = library.roots().unwrap_or_default();
        if !active_roots.iter().any(|root| path.starts_with(root)) {
            continue;
        }
        seen.push(path.clone());
        match file_signature(path) {
            Ok((file_size, file_mtime)) => {
                let fresh = library
                    .is_fresh(path, file_size, file_mtime)
                    .unwrap_or(false);
                if !fresh {
                    let existed = library.book_by_path(path).ok().flatten().is_some();
                    match scan_book(library, path, file_size, file_mtime) {
                        Ok(_) => {
                            if existed {
                                updated += 1;
                            } else {
                                added += 1;
                            }
                        }
                        Err(error) => {
                            failed += 1;
                            push_error(&mut errors, format!("{}: {error:#}", path.display()));
                        }
                    }
                }
            }
            Err(error) => {
                failed += 1;
                push_error(&mut errors, format!("{}: {error:#}", path.display()));
            }
        }

        let _ = events.send_blocking(ScanEvent::Progress {
            done: done + 1,
            total,
        });
    }

    let existing_roots: Vec<PathBuf> = roots.iter().filter(|root| root.exists()).cloned().collect();
    let removed = library
        .prune_missing(&existing_roots, &seen)
        .unwrap_or_default();

    let _ = events.send_blocking(ScanEvent::Finished {
        total,
        added,
        updated,
        removed,
        failed,
        errors,
    });
    Ok(())
}

fn scan_book(library: &Library, path: &Path, file_size: i64, file_mtime: i64) -> Result<Book> {
    if is_cbz(path) {
        return scan_cbz(library, path, file_size, file_mtime);
    }
    if is_epub(path) {
        return scan_epub(library, path, file_size, file_mtime);
    }
    scan_pdf(library, path, file_size, file_mtime)
}

fn scan_pdf(library: &Library, path: &Path, file_size: i64, file_mtime: i64) -> Result<Book> {
    let pdf = PdfBook::open(path)?;
    let page_count = pdf.page_count();
    let (series, series_index) = parse::parse_series(path);
    let fallback_title = parse::title_from_path(path);
    let title = pdf
        .title()
        .filter(|title| is_useful_title(title))
        .unwrap_or(fallback_title);
    let authors = pdf.author().map(|author| author.trim().to_owned());

    let book = library.upsert_scanned(&ScannedBook {
        path: path.to_path_buf(),
        format: "pdf".to_owned(),
        title,
        authors,
        series,
        series_index,
        page_count: Some(page_count),
        file_size,
        file_mtime,
    })?;

    let cover_exists = book.cover_path.as_ref().is_some_and(|cover| cover.exists());
    if !cover_exists && page_count > 0 {
        let cover_path = library.cover_dir().join(format!("{}.png", book.id));
        match pdf.render_page(0, 320, 480) {
            Ok(page) => save_cover(&cover_path, &page.rgba, page.width, page.height)?,
            Err(error) => {
                eprintln!("cover render failed for {}: {error:#}", path.display());
            }
        }
        if cover_path.exists() {
            library.set_cover(book.id, Some(&cover_path))?;
            return library
                .book_by_id(book.id)?
                .ok_or_else(|| anyhow!("book disappeared after cover generation"));
        }
    }

    Ok(book)
}

fn scan_cbz(library: &Library, path: &Path, file_size: i64, file_mtime: i64) -> Result<Book> {
    let mut archive = zip::ZipArchive::new(fs::File::open(path)?)
        .with_context(|| format!("failed to open CBZ {}", path.display()))?;
    let mut pages = Vec::new();
    for index in 0..archive.len() {
        let entry = archive.by_index(index)?;
        if !entry.is_dir() && is_image(entry.name()) {
            pages.push(entry.name().to_owned());
        }
    }
    pages.sort_by_key(|name| natural_page_key(name));
    let page_count = pages.len();

    let (series, series_index) = parse::parse_series(path);
    let title = parse::title_from_path(path);
    let book = library.upsert_scanned(&ScannedBook {
        path: path.to_path_buf(),
        format: "cbz".to_owned(),
        title,
        authors: None,
        series,
        series_index,
        page_count: Some(page_count),
        file_size,
        file_mtime,
    })?;

    let cover_exists = book.cover_path.as_ref().is_some_and(|cover| cover.exists());
    if !cover_exists && let Some(first) = pages.first() {
        let mut bytes = Vec::new();
        archive.by_name(first)?.read_to_end(&mut bytes)?;
        match image::load_from_memory(&bytes) {
            Ok(image) => {
                let cover = image.thumbnail(320, 480).to_rgba8();
                let cover_path = library.cover_dir().join(format!("{}.png", book.id));
                save_cover(&cover_path, cover.as_raw(), cover.width(), cover.height())?;
                library.set_cover(book.id, Some(&cover_path))?;
                return library
                    .book_by_id(book.id)?
                    .ok_or_else(|| anyhow!("book disappeared after cover generation"));
            }
            Err(error) => eprintln!("CBZ cover decode failed for {}: {error:#}", path.display()),
        }
    }

    Ok(book)
}

fn scan_epub(library: &Library, path: &Path, file_size: i64, file_mtime: i64) -> Result<Book> {
    let mut archive = zip::ZipArchive::new(fs::File::open(path)?)
        .with_context(|| format!("failed to open EPUB {}", path.display()))?;
    let mut pages = Vec::new();
    let mut cover_name = None;
    for index in 0..archive.len() {
        let entry = archive.by_index(index)?;
        if entry.is_dir() {
            continue;
        }
        if is_text_page(entry.name()) {
            pages.push(entry.name().to_owned());
        } else if is_image(entry.name())
            && (cover_name.is_none() || entry.name().to_ascii_lowercase().contains("cover"))
        {
            cover_name = Some(entry.name().to_owned());
        }
    }
    pages.sort_by_key(|name| natural_page_key(name));
    let page_count = pages.len().max(1);

    let (series, series_index) = parse::parse_series(path);
    let title = parse::title_from_path(path);
    let book = library.upsert_scanned(&ScannedBook {
        path: path.to_path_buf(),
        format: "epub".to_owned(),
        title,
        authors: None,
        series,
        series_index,
        page_count: Some(page_count),
        file_size,
        file_mtime,
    })?;

    let cover_exists = book.cover_path.as_ref().is_some_and(|cover| cover.exists());
    if !cover_exists && let Some(name) = cover_name {
        let mut bytes = Vec::new();
        archive.by_name(&name)?.read_to_end(&mut bytes)?;
        if let Ok(image) = image::load_from_memory(&bytes) {
            let cover = image.thumbnail(320, 480).to_rgba8();
            let cover_path = library.cover_dir().join(format!("{}.png", book.id));
            save_cover(&cover_path, cover.as_raw(), cover.width(), cover.height())?;
            library.set_cover(book.id, Some(&cover_path))?;
            return library
                .book_by_id(book.id)?
                .ok_or_else(|| anyhow!("book disappeared after cover generation"));
        }
    }

    Ok(book)
}

fn save_cover(path: &Path, rgba: &[u8], width: u32, height: u32) -> Result<()> {
    image::save_buffer_with_format(
        path,
        rgba,
        width,
        height,
        image::ColorType::Rgba8,
        image::ImageFormat::Png,
    )
    .with_context(|| format!("failed to write {}", path.display()))
}

fn collect_books(roots: &[PathBuf]) -> (Vec<PathBuf>, Vec<String>) {
    let mut files = HashSet::new();
    let mut errors = Vec::new();
    for root in roots {
        if !root.exists() {
            errors.push(format!("library root does not exist: {}", root.display()));
            continue;
        }
        let walker = WalkDir::new(root)
            .max_depth(12)
            .follow_links(false)
            .into_iter()
            .filter_entry(|entry| !ignored(entry.path()));
        for entry in walker {
            match entry {
                Ok(entry) if entry.file_type().is_file() && is_supported(entry.path()) => {
                    files.insert(entry.into_path());
                }
                Ok(_) => {}
                Err(error) => push_error(&mut errors, error.to_string()),
            }
        }
    }
    let mut files: Vec<PathBuf> = files.into_iter().collect();
    files.sort();
    (files, errors)
}

fn file_signature(path: &Path) -> Result<(i64, i64)> {
    let metadata = fs::metadata(path)?;
    let mtime = metadata
        .modified()
        .ok()
        .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
        .map(|duration| duration.as_secs() as i64)
        .unwrap_or_else(|| {
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs() as i64
        });
    Ok((metadata.len() as i64, mtime))
}

fn is_supported(path: &Path) -> bool {
    extension_is(path, "pdf") || is_cbz(path) || is_epub(path)
}

fn is_cbz(path: &Path) -> bool {
    extension_is(path, "cbz")
}

fn is_epub(path: &Path) -> bool {
    extension_is(path, "epub")
}

fn is_text_page(name: &str) -> bool {
    ["xhtml", "html", "htm"].iter().any(|extension| {
        name.to_ascii_lowercase()
            .ends_with(&format!(".{extension}"))
    })
}

fn extension_is(path: &Path, wanted: &str) -> bool {
    path.extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| extension.eq_ignore_ascii_case(wanted))
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

fn ignored(path: &Path) -> bool {
    let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
        return true;
    };
    name.starts_with('.')
        || name.starts_with("MEGABASTERD_CHUNKS_")
        || name.contains(".mctemp")
        || name.ends_with(".part")
        || name.ends_with(".crdownload")
        || name.ends_with(".tmp")
        || name == "__MACOSX"
}

fn is_useful_title(title: &str) -> bool {
    let title = title.trim();
    !title.is_empty()
        && !title.eq_ignore_ascii_case("untitled")
        && !title.eq_ignore_ascii_case("unknown")
}

fn push_error(errors: &mut Vec<String>, error: String) {
    if errors.len() < 50 {
        errors.push(error);
    }
}
