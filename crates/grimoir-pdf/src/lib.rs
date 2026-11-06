//! PDF rendering for Grimoir, backed by PDFium.

pub mod glass;
pub mod worker;

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

use anyhow::{Result, anyhow, bail};
use pdfium_render::prelude::*;

/// A rendered page in straight (non-premultiplied) RGBA order.
#[derive(Debug, Clone)]
pub struct RawPage {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
}

/// A normalized rectangle (0..1) on a page.
#[derive(Debug, Clone, Copy)]
pub struct SearchRect {
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
}

/// A text search result, with the page and the highlighted bounds.
#[derive(Debug, Clone)]
pub struct SearchHit {
    pub page: usize,
    pub snippet: String,
    pub rects: Vec<SearchRect>,
}

/// An open PDF document.
///
/// The document is intentionally not `Send`: PDFium documents are used from
/// the thread that owns the reader, and the UI currently renders on the main
/// thread. The rendering worker will move this behind a dedicated thread in a
/// later phase.
pub struct PdfBook {
    _pdfium: &'static Pdfium,
    document: PdfDocument<'static>,
    path: PathBuf,
}

impl PdfBook {
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref();
        if !path.exists() {
            bail!("file does not exist: {}", path.display());
        }

        let _guard = pdfium_guard();
        let pdfium = pdfium()?;
        let document = pdfium
            .load_pdf_from_file(path, None)
            .map_err(|e| anyhow!("failed to open PDF {}: {e}", path.display()))?;

        if document.pages().is_empty() {
            bail!("PDF has no pages: {}", path.display());
        }

        Ok(Self {
            _pdfium: pdfium,
            document,
            path: path.to_path_buf(),
        })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn page_count(&self) -> usize {
        let _guard = pdfium_guard();
        self.document.pages().len() as usize
    }

    pub fn title(&self) -> Option<String> {
        self.metadata(PdfDocumentMetadataTagType::Title)
    }

    pub fn author(&self) -> Option<String> {
        self.metadata(PdfDocumentMetadataTagType::Author)
    }

    fn metadata(&self, tag: PdfDocumentMetadataTagType) -> Option<String> {
        let _guard = pdfium_guard();
        let value = self
            .document
            .metadata()
            .get(tag)?
            .value()
            .trim()
            .to_string();
        (!value.is_empty()).then_some(value)
    }

    /// Page dimensions in PDF points, in document order.
    pub fn page_sizes(&self) -> Vec<(f32, f32)> {
        let _guard = pdfium_guard();
        let pages = self.document.pages();
        (0..pages.len())
            .map(|index| match pages.get(index) {
                Ok(page) => (page.width().value, page.height().value),
                Err(_) => (0.0, 0.0),
            })
            .collect()
    }

    /// Searches every page's text layer. Image-only scans yield no results.
    pub fn search(&self, query: &str, max_results: usize) -> Vec<SearchHit> {
        let query = query.trim();
        if query.is_empty() || max_results == 0 {
            return Vec::new();
        }

        let _guard = pdfium_guard();
        let pages = self.document.pages();
        let mut hits = Vec::new();
        for index in 0..pages.len() {
            let Ok(page) = pages.get(index) else {
                continue;
            };
            let page_width = page.width().value.max(1.0);
            let page_height = page.height().value.max(1.0);
            let Ok(text) = page.text() else {
                continue;
            };
            let Ok(search) = text.search(query, &PdfSearchOptions::new()) else {
                continue;
            };

            for segments in search.iter(PdfSearchDirection::SearchForward) {
                let mut snippet = String::new();
                let mut rects = Vec::new();
                for segment in segments.iter() {
                    let bounds = segment.bounds();
                    let left = bounds.left().value;
                    let bottom = bounds.bottom().value;
                    let right = bounds.right().value;
                    let top = bounds.top().value;
                    rects.push(SearchRect {
                        x: (left / page_width).clamp(0.0, 1.0),
                        y: (1.0 - top / page_height).clamp(0.0, 1.0),
                        width: ((right - left) / page_width).clamp(0.0, 1.0),
                        height: ((top - bottom) / page_height).clamp(0.0, 1.0),
                    });
                    if !snippet.is_empty() {
                        snippet.push(' ');
                    }
                    snippet.push_str(segment.text().trim());
                }

                let snippet = snippet.trim().to_owned();
                if snippet.is_empty() {
                    continue;
                }
                let snippet = if snippet.chars().count() > 180 {
                    let mut short: String = snippet.chars().take(177).collect();
                    short.push('…');
                    short
                } else {
                    snippet
                };
                hits.push(SearchHit {
                    page: index as usize,
                    snippet,
                    rects,
                });
                if hits.len() >= max_results {
                    return hits;
                }
            }
        }
        hits
    }

    /// Flattens the PDF outline into a depth-first list with page targets.
    pub fn table_of_contents(&self) -> Vec<worker::TocEntry> {
        let _guard = pdfium_guard();
        let mut entries = Vec::new();
        let mut visited = HashSet::new();
        walk_bookmarks(
            self.document.bookmarks().root(),
            0,
            &mut entries,
            &mut visited,
        );
        entries
    }

    /// Renders a page so it fits inside `max_width` x `max_height`, preserving
    /// the page aspect ratio.
    pub fn render_page(&self, index: usize, max_width: u32, max_height: u32) -> Result<RawPage> {
        let _guard = pdfium_guard();
        let page_index = i32::try_from(index)
            .map_err(|_| anyhow!("page index {index} does not fit in PDFium's page index"))?;
        let page = self
            .document
            .pages()
            .get(page_index)
            .map_err(|e| anyhow!("failed to get page {}: {e}", index + 1))?;

        let width = max_width.max(1) as i32;
        let height = max_height.max(1) as i32;

        let config = PdfRenderConfig::new()
            .set_target_width(width)
            .set_maximum_width(width)
            .set_maximum_height(height);

        let bitmap = page
            .render_with_config(&config)
            .map_err(|e| anyhow!("failed to render page {}: {e}", index + 1))?;

        let width = bitmap.width().max(0) as u32;
        let height = bitmap.height().max(0) as u32;
        let rgba = bitmap.as_rgba_bytes();

        let expected = width as usize * height as usize * 4;
        if rgba.len() != expected {
            bail!(
                "PDFium returned {} bytes for a {}x{} RGBA page (expected {})",
                rgba.len(),
                width,
                height,
                expected
            );
        }

        Ok(RawPage {
            width,
            height,
            rgba,
        })
    }
}

fn walk_bookmarks<'a>(
    first: Option<PdfBookmark<'a>>,
    depth: usize,
    entries: &mut Vec<worker::TocEntry>,
    visited: &mut HashSet<PdfBookmark<'a>>,
) {
    let mut current = first;
    while let Some(bookmark) = current {
        if !visited.insert(bookmark.clone()) {
            return;
        }
        if let Some(title) = bookmark.title() {
            let page = bookmark
                .destination()
                .and_then(|destination| destination.page_index().ok())
                .and_then(|page| usize::try_from(page).ok());
            entries.push(worker::TocEntry { title, page, depth });
        }
        walk_bookmarks(bookmark.first_child(), depth + 1, entries, visited);
        current = bookmark.next_sibling();
    }
}

static PDFIUM: OnceLock<Result<&'static Pdfium, String>> = OnceLock::new();
static PDFIUM_OPERATION: Mutex<()> = Mutex::new(());

fn pdfium_guard() -> std::sync::MutexGuard<'static, ()> {
    PDFIUM_OPERATION
        .lock()
        .unwrap_or_else(|error| error.into_inner())
}

/// Initializes PDFium once. Safe to call from anywhere; subsequent calls are
/// cheap.
pub fn init() -> Result<()> {
    pdfium().map(|_| ())
}

fn pdfium() -> Result<&'static Pdfium> {
    PDFIUM
        .get_or_init(|| {
            let bindings = bind_pdfium_library().map_err(|e| format!("{e:#}"))?;
            Ok(Box::leak(Box::new(Pdfium::new(bindings))))
        })
        .clone()
        .map_err(|e| anyhow!(e))
}

fn bind_pdfium_library() -> Result<Box<dyn PdfiumLibraryBindings>> {
    let mut candidates: Vec<PathBuf> = Vec::new();

    if let Some(path) = std::env::var_os("GRIMOIR_PDFIUM_LIB") {
        candidates.push(PathBuf::from(path));
    }

    if let Some(dir) = std::env::var_os("GRIMOIR_PDFIUM_DIR") {
        let dir = PathBuf::from(dir);
        candidates.push(Pdfium::pdfium_platform_library_name_at_path(&dir));
    }

    if let Ok(exe) = std::env::current_exe()
        && let Some(dir) = exe.parent()
    {
        // Portable layouts: lib next to the binary (tarball), or in a
        // per-user lib directory (the install script).
        candidates.push(Pdfium::pdfium_platform_library_name_at_path(dir));
        candidates.push(Pdfium::pdfium_platform_library_name_at_path(
            &dir.join("lib"),
        ));
        candidates.push(Pdfium::pdfium_platform_library_name_at_path(
            &dir.join("../lib/grimoir"),
        ));
        candidates.push(Pdfium::pdfium_platform_library_name_at_path(
            &dir.join("../lib"),
        ));
        // A release built inside the repository can still find the dev tree
        // without baking an absolute path into the installed binary.
        candidates.push(Pdfium::pdfium_platform_library_name_at_path(
            &dir.join("../../vendor/pdfium/lib"),
        ));
    }

    if let Some(data_home) = std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".local/share")))
    {
        candidates.push(Pdfium::pdfium_platform_library_name_at_path(
            &data_home.join("grimoir/lib"),
        ));
    }

    let relative_vendor = PathBuf::from("vendor/pdfium/lib");
    candidates.push(Pdfium::pdfium_platform_library_name_at_path(
        &relative_vendor,
    ));

    #[cfg(debug_assertions)]
    {
        let manifest_vendor =
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../vendor/pdfium/lib");
        candidates.push(Pdfium::pdfium_platform_library_name_at_path(
            &manifest_vendor,
        ));
    }

    let mut errors = Vec::new();

    for candidate in candidates {
        if !candidate.exists() {
            continue;
        }
        match Pdfium::bind_to_library(&candidate) {
            Ok(bindings) => return Ok(bindings),
            Err(error) => errors.push(format!("{}: {error}", candidate.display())),
        }
    }

    match Pdfium::bind_to_system_library() {
        Ok(bindings) => Ok(bindings),
        Err(error) => {
            errors.push(format!("system library: {error}"));
            Err(anyhow!(
                "could not load PDFium. Run scripts/fetch-pdfium.sh or set GRIMOIR_PDFIUM_LIB.\n{}",
                errors.join("\n")
            ))
        }
    }
}
