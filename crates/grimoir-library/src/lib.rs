//! Local library storage, scanning and file watching for Grimoir.

pub mod parse;
pub mod scan;
pub mod watch;

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, anyhow};
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};

const SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS books (
    id INTEGER PRIMARY KEY,
    path TEXT UNIQUE NOT NULL,
    format TEXT NOT NULL,
    title TEXT NOT NULL,
    subtitle TEXT,
    authors TEXT,
    series TEXT,
    series_index REAL,
    language TEXT,
    publisher TEXT,
    published TEXT,
    description TEXT,
    isbn TEXT,
    page_count INTEGER,
    cover_path TEXT,
    file_size INTEGER NOT NULL DEFAULT 0,
    file_mtime INTEGER NOT NULL DEFAULT 0,
    file_hash TEXT,
    added_at INTEGER NOT NULL,
    last_opened_at INTEGER,
    favorite INTEGER NOT NULL DEFAULT 0,
    reading_status TEXT NOT NULL DEFAULT 'unread'
);

CREATE TABLE IF NOT EXISTS progress (
    book_id INTEGER PRIMARY KEY REFERENCES books(id) ON DELETE CASCADE,
    locator TEXT,
    page INTEGER,
    percent REAL NOT NULL DEFAULT 0,
    updated_at INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS collections (
    id INTEGER PRIMARY KEY,
    name TEXT NOT NULL,
    parent_id INTEGER REFERENCES collections(id) ON DELETE CASCADE,
    kind TEXT NOT NULL,
    path TEXT,
    sort_order INTEGER NOT NULL DEFAULT 0
);

CREATE TABLE IF NOT EXISTS book_collections (
    book_id INTEGER NOT NULL REFERENCES books(id) ON DELETE CASCADE,
    collection_id INTEGER NOT NULL REFERENCES collections(id) ON DELETE CASCADE,
    PRIMARY KEY (book_id, collection_id)
);

CREATE TABLE IF NOT EXISTS annotations (
    id INTEGER PRIMARY KEY,
    book_id INTEGER NOT NULL REFERENCES books(id) ON DELETE CASCADE,
    kind TEXT NOT NULL,
    page INTEGER,
    locator TEXT,
    color TEXT,
    text TEXT,
    note TEXT,
    rects_json TEXT,
    created_at INTEGER NOT NULL,
    updated_at INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS obsidian_targets (
    book_id INTEGER PRIMARY KEY REFERENCES books(id) ON DELETE CASCADE,
    note_path TEXT NOT NULL,
    note_title TEXT,
    updated_at INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS reading_sessions (
    id INTEGER PRIMARY KEY,
    book_id INTEGER NOT NULL REFERENCES books(id) ON DELETE CASCADE,
    started_at INTEGER NOT NULL,
    ended_at INTEGER,
    pages_read INTEGER NOT NULL DEFAULT 0,
    duration_seconds INTEGER NOT NULL DEFAULT 0
);

CREATE TABLE IF NOT EXISTS settings (
    key TEXT PRIMARY KEY,
    value TEXT NOT NULL
);

CREATE INDEX IF NOT EXISTS books_title_idx ON books(title COLLATE NOCASE);
CREATE INDEX IF NOT EXISTS books_series_idx ON books(series COLLATE NOCASE);
CREATE INDEX IF NOT EXISTS books_last_opened_idx ON books(last_opened_at DESC);
CREATE INDEX IF NOT EXISTS progress_updated_idx ON progress(updated_at DESC);
"#;

const BOOK_SELECT: &str = r#"
SELECT b.id, b.path, b.format, b.title, b.authors, b.series, b.series_index,
       b.page_count, b.cover_path, b.file_size, b.file_mtime, b.added_at,
       b.last_opened_at, b.favorite, b.reading_status, p.page, p.percent
FROM books b
LEFT JOIN progress p ON p.book_id = b.id
"#;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Book {
    pub id: i64,
    pub path: PathBuf,
    pub format: String,
    pub title: String,
    pub authors: Option<String>,
    pub series: Option<String>,
    pub series_index: Option<f64>,
    pub page_count: Option<usize>,
    pub cover_path: Option<PathBuf>,
    pub file_size: i64,
    pub file_mtime: i64,
    pub added_at: i64,
    pub last_opened_at: Option<i64>,
    pub favorite: bool,
    pub reading_status: String,
    pub progress_page: Option<usize>,
    pub progress_percent: f64,
}

impl Book {
    pub fn author_label(&self) -> String {
        self.authors
            .clone()
            .unwrap_or_else(|| "Unknown author".to_owned())
    }

    pub fn series_label(&self) -> Option<String> {
        let series = self.series.as_ref()?;
        Some(match self.series_index {
            Some(index) if index.fract() == 0.0 => format!("{series} #{}", index as i64),
            Some(index) => format!("{series} #{index:.1}"),
            None => series.clone(),
        })
    }

    pub fn progress_page(&self) -> usize {
        self.progress_page.unwrap_or(0)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Collection {
    pub id: i64,
    pub name: String,
}

/// Per-book reading preferences. Stored as JSON in the settings table so the
/// schema can evolve without a migration for every new toggle.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct ReaderPrefs {
    pub double_page: bool,
    pub rtl: bool,
    pub scroll: bool,
    pub motion: bool,
    /// 0 = page, 1 = width, 2 = height, 3 = original.
    pub fit: u8,
    pub zoom: f32,
    pub glass: bool,
}

impl Default for ReaderPrefs {
    fn default() -> Self {
        Self {
            double_page: false,
            rtl: false,
            scroll: false,
            motion: true,
            fit: 0,
            zoom: 1.0,
            glass: true,
        }
    }
}

#[derive(Debug, Clone)]
pub struct Annotation {
    pub id: i64,
    pub book_id: i64,
    pub kind: String,
    pub page: usize,
    pub color: Option<String>,
    pub text: Option<String>,
    pub note: Option<String>,
    pub rects_json: Option<String>,
    pub created_at: i64,
    pub updated_at: i64,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Progress {
    pub page: usize,
    pub percent: f64,
}

#[derive(Debug, Clone, Copy, Default)]
pub struct LibraryStats {
    pub total_books: i64,
    pub reading: i64,
    pub finished: i64,
    pub sessions: i64,
    pub seconds: i64,
    pub pages: i64,
}

#[derive(Debug, Clone)]
pub struct ScannedBook {
    pub path: PathBuf,
    pub format: String,
    pub title: String,
    pub authors: Option<String>,
    pub series: Option<String>,
    pub series_index: Option<f64>,
    pub page_count: Option<usize>,
    pub file_size: i64,
    pub file_mtime: i64,
}

pub struct Library {
    db: Mutex<Connection>,
    db_path: PathBuf,
    cover_dir: PathBuf,
}

impl Library {
    pub fn open_default() -> Result<Self> {
        let data = env_dir("XDG_DATA_HOME", ".local/share").join("grimoir");
        let cache = env_dir("XDG_CACHE_HOME", ".cache").join("grimoir");
        Self::open(&data, &cache)
    }

    pub fn open(data_dir: impl AsRef<Path>, cache_dir: impl AsRef<Path>) -> Result<Self> {
        let data_dir = data_dir.as_ref();
        let cache_dir = cache_dir.as_ref();
        fs::create_dir_all(data_dir)
            .with_context(|| format!("failed to create {}", data_dir.display()))?;
        let cover_dir = cache_dir.join("covers");
        fs::create_dir_all(&cover_dir)
            .with_context(|| format!("failed to create {}", cover_dir.display()))?;

        let db_path = data_dir.join("library.db");
        let conn = Connection::open(&db_path)
            .with_context(|| format!("failed to open {}", db_path.display()))?;
        conn.execute_batch(
            "PRAGMA journal_mode=WAL; PRAGMA synchronous=NORMAL; PRAGMA foreign_keys=ON;",
        )?;
        migrate(&conn)?;

        Ok(Self {
            db: Mutex::new(conn),
            db_path,
            cover_dir,
        })
    }

    pub fn db_path(&self) -> &Path {
        &self.db_path
    }

    pub fn cover_dir(&self) -> &Path {
        &self.cover_dir
    }

    fn conn(&self) -> std::sync::MutexGuard<'_, Connection> {
        self.db.lock().expect("library database mutex poisoned")
    }

    pub fn all_books(&self) -> Result<Vec<Book>> {
        let conn = self.conn();
        let mut stmt = conn.prepare(&format!("{BOOK_SELECT} ORDER BY b.title COLLATE NOCASE"))?;
        let rows = stmt.query_map([], book_from_row)?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    pub fn book_by_path(&self, path: &Path) -> Result<Option<Book>> {
        let conn = self.conn();
        let mut stmt = conn.prepare(&format!("{BOOK_SELECT} WHERE b.path = ?1"))?;
        Ok(stmt
            .query_row([path.to_string_lossy().as_ref()], book_from_row)
            .optional()?)
    }

    pub fn book_by_id(&self, id: i64) -> Result<Option<Book>> {
        let conn = self.conn();
        let mut stmt = conn.prepare(&format!("{BOOK_SELECT} WHERE b.id = ?1"))?;
        Ok(stmt.query_row([id], book_from_row).optional()?)
    }

    pub fn is_fresh(&self, path: &Path, file_size: i64, file_mtime: i64) -> Result<bool> {
        let Some(book) = self.book_by_path(path)? else {
            return Ok(false);
        };
        let cover_ok = book.cover_path.as_ref().is_some_and(|cover| cover.exists());
        Ok(book.file_size == file_size && book.file_mtime == file_mtime && cover_ok)
    }

    pub fn upsert_scanned(&self, book: &ScannedBook) -> Result<Book> {
        let now = unix_now();
        {
            let conn = self.conn();
            conn.execute(
                r#"
                INSERT INTO books
                    (path, format, title, authors, series, series_index, page_count,
                     file_size, file_mtime, added_at, reading_status)
                VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, 'unread')
                ON CONFLICT(path) DO UPDATE SET
                    format = excluded.format,
                    title = excluded.title,
                    authors = excluded.authors,
                    series = excluded.series,
                    series_index = excluded.series_index,
                    page_count = excluded.page_count,
                    file_size = excluded.file_size,
                    file_mtime = excluded.file_mtime
                "#,
                params![
                    book.path.to_string_lossy(),
                    book.format,
                    book.title,
                    book.authors,
                    book.series,
                    book.series_index,
                    book.page_count.map(|n| n as i64),
                    book.file_size,
                    book.file_mtime,
                    now,
                ],
            )?;
        }
        self.book_by_path(&book.path)?
            .ok_or_else(|| anyhow!("book disappeared after upsert: {}", book.path.display()))
    }

    pub fn set_cover(&self, book_id: i64, cover: Option<&Path>) -> Result<()> {
        let conn = self.conn();
        conn.execute(
            "UPDATE books SET cover_path = ?1 WHERE id = ?2",
            params![
                cover.map(|path| path.to_string_lossy().into_owned()),
                book_id
            ],
        )?;
        Ok(())
    }

    pub fn mark_opened(&self, book_id: i64) -> Result<()> {
        let conn = self.conn();
        conn.execute(
            "UPDATE books SET last_opened_at = ?1, reading_status = 'reading' WHERE id = ?2",
            params![unix_now(), book_id],
        )?;
        Ok(())
    }

    pub fn save_progress(&self, book_id: i64, page: usize, percent: f64) -> Result<()> {
        let conn = self.conn();
        conn.execute(
            r#"
            INSERT INTO progress (book_id, page, percent, updated_at)
            VALUES (?1, ?2, ?3, ?4)
            ON CONFLICT(book_id) DO UPDATE SET
                page = excluded.page,
                percent = excluded.percent,
                updated_at = excluded.updated_at
            "#,
            params![book_id, page as i64, percent.clamp(0.0, 1.0), unix_now()],
        )?;
        Ok(())
    }

    pub fn progress_for_path(&self, path: &Path) -> Result<Option<Progress>> {
        let conn = self.conn();
        let mut stmt = conn.prepare(
            "SELECT p.page, p.percent FROM progress p JOIN books b ON b.id = p.book_id WHERE b.path = ?1",
        )?;
        Ok(stmt
            .query_row([path.to_string_lossy().as_ref()], |row| {
                Ok(Progress {
                    page: row.get::<_, Option<i64>>(0)?.unwrap_or(0).max(0) as usize,
                    percent: row.get(1)?,
                })
            })
            .optional()?)
    }

    pub fn set_favorite(&self, book_id: i64, favorite: bool) -> Result<()> {
        let conn = self.conn();
        conn.execute(
            "UPDATE books SET favorite = ?1 WHERE id = ?2",
            params![favorite as i64, book_id],
        )?;
        Ok(())
    }

    pub fn add_books_to_collection(&self, book_ids: &[i64], collection_id: i64) -> Result<()> {
        let mut conn = self.conn();
        let tx = conn.transaction()?;
        for book_id in book_ids {
            tx.execute(
                "INSERT OR IGNORE INTO book_collections (book_id, collection_id) VALUES (?1, ?2)",
                params![book_id, collection_id],
            )?;
        }
        tx.commit()?;
        Ok(())
    }

    pub fn remove_books_from_collection(&self, book_ids: &[i64], collection_id: i64) -> Result<()> {
        let mut conn = self.conn();
        let tx = conn.transaction()?;
        for book_id in book_ids {
            tx.execute(
                "DELETE FROM book_collections WHERE book_id = ?1 AND collection_id = ?2",
                params![book_id, collection_id],
            )?;
        }
        tx.commit()?;
        Ok(())
    }

    /// Removes books from the library database only. The source files are
    /// never touched; this is the basis of Grimoir's read-only file policy.
    pub fn remove_books(&self, book_ids: &[i64]) -> Result<()> {
        let mut conn = self.conn();
        let tx = conn.transaction()?;
        let mut covers = Vec::new();
        for book_id in book_ids {
            let cover: Option<String> = tx
                .query_row(
                    "SELECT cover_path FROM books WHERE id = ?1",
                    [book_id],
                    |row| row.get(0),
                )
                .optional()?
                .flatten();
            if let Some(cover) = cover {
                covers.push(cover);
            }
            tx.execute("DELETE FROM books WHERE id = ?1", [book_id])?;
        }
        tx.commit()?;
        for cover in covers {
            let _ = fs::remove_file(cover);
        }
        Ok(())
    }

    pub fn update_metadata(&self, book_id: i64, title: &str, authors: Option<&str>) -> Result<()> {
        let title = title.trim();
        if title.is_empty() {
            return Err(anyhow!("title cannot be empty"));
        }
        let conn = self.conn();
        conn.execute(
            "UPDATE books SET title = ?1, authors = ?2 WHERE id = ?3",
            params![title, authors, book_id],
        )?;
        Ok(())
    }

    pub fn update_book_metadata(
        &self,
        book_id: i64,
        title: &str,
        authors: Option<&str>,
        series: Option<&str>,
        series_index: Option<f64>,
        reading_status: &str,
    ) -> Result<()> {
        let title = title.trim();
        if title.is_empty() {
            return Err(anyhow!("title cannot be empty"));
        }
        let reading_status = match reading_status {
            "unread" | "reading" | "finished" => reading_status,
            _ => return Err(anyhow!("invalid reading status: {reading_status}")),
        };
        let conn = self.conn();
        conn.execute(
            r#"UPDATE books
               SET title = ?1, authors = ?2, series = ?3, series_index = ?4,
                   reading_status = ?5
               WHERE id = ?6"#,
            params![
                title,
                authors,
                series,
                series_index,
                reading_status,
                book_id
            ],
        )?;
        Ok(())
    }

    pub fn set_reading_status(&self, book_id: i64, reading_status: &str) -> Result<()> {
        let conn = self.conn();
        conn.execute(
            "UPDATE books SET reading_status = ?1 WHERE id = ?2",
            params![reading_status, book_id],
        )?;
        Ok(())
    }

    pub fn collections(&self) -> Result<Vec<Collection>> {
        let conn = self.conn();
        let mut stmt =
            conn.prepare("SELECT id, name FROM collections ORDER BY name COLLATE NOCASE")?;
        let rows = stmt.query_map([], |row| {
            Ok(Collection {
                id: row.get(0)?,
                name: row.get(1)?,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    pub fn create_collection(&self, name: &str) -> Result<Collection> {
        let name = name.trim();
        if name.is_empty() {
            return Err(anyhow!("collection name cannot be empty"));
        }
        let conn = self.conn();
        conn.execute(
            "INSERT INTO collections (name, kind) VALUES (?1, 'manual')",
            [name],
        )?;
        Ok(Collection {
            id: conn.last_insert_rowid(),
            name: name.to_owned(),
        })
    }

    pub fn rename_collection(&self, collection_id: i64, name: &str) -> Result<()> {
        let name = name.trim();
        if name.is_empty() {
            return Err(anyhow!("collection name cannot be empty"));
        }
        let conn = self.conn();
        conn.execute(
            "UPDATE collections SET name = ?1 WHERE id = ?2",
            params![name, collection_id],
        )?;
        Ok(())
    }

    pub fn delete_collection(&self, collection_id: i64) -> Result<()> {
        let conn = self.conn();
        conn.execute("DELETE FROM collections WHERE id = ?1", [collection_id])?;
        Ok(())
    }

    pub fn collections_for_book(&self, book_id: i64) -> Result<Vec<i64>> {
        let conn = self.conn();
        let mut stmt =
            conn.prepare("SELECT collection_id FROM book_collections WHERE book_id = ?1")?;
        let rows = stmt.query_map([book_id], |row| row.get(0))?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    pub fn book_ids_in_collection(&self, collection_id: i64) -> Result<Vec<i64>> {
        let conn = self.conn();
        let mut stmt =
            conn.prepare("SELECT book_id FROM book_collections WHERE collection_id = ?1")?;
        let rows = stmt.query_map([collection_id], |row| row.get(0))?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    pub fn set_book_collections(&self, book_id: i64, collection_ids: &[i64]) -> Result<()> {
        let mut conn = self.conn();
        let tx = conn.transaction()?;
        tx.execute("DELETE FROM book_collections WHERE book_id = ?1", [book_id])?;
        for collection_id in collection_ids {
            tx.execute(
                "INSERT OR IGNORE INTO book_collections (book_id, collection_id) VALUES (?1, ?2)",
                params![book_id, collection_id],
            )?;
        }
        tx.commit()?;
        Ok(())
    }

    pub fn load_reader_prefs(&self, book_id: i64) -> Result<ReaderPrefs> {
        let conn = self.conn();
        let raw: Option<String> = conn
            .query_row(
                "SELECT value FROM settings WHERE key = ?1",
                [format!("reader_prefs:{book_id}")],
                |row| row.get(0),
            )
            .optional()?;
        Ok(raw
            .and_then(|raw| serde_json::from_str(&raw).ok())
            .unwrap_or_default())
    }

    pub fn save_reader_prefs(&self, book_id: i64, prefs: &ReaderPrefs) -> Result<()> {
        let encoded = serde_json::to_string(prefs)?;
        let conn = self.conn();
        conn.execute(
            "INSERT INTO settings (key, value) VALUES (?1, ?2)
             ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            params![format!("reader_prefs:{book_id}"), encoded],
        )?;
        Ok(())
    }

    pub fn annotations_for_book(&self, book_id: i64) -> Result<Vec<Annotation>> {
        let conn = self.conn();
        let mut stmt = conn.prepare(
            r#"SELECT id, book_id, kind, page, color, text, note, rects_json,
                      created_at, updated_at
               FROM annotations WHERE book_id = ?1 ORDER BY page, id"#,
        )?;
        let rows = stmt.query_map([book_id], |row| {
            Ok(Annotation {
                id: row.get(0)?,
                book_id: row.get(1)?,
                kind: row.get(2)?,
                page: row.get::<_, Option<i64>>(3)?.unwrap_or(0).max(0) as usize,
                color: row.get(4)?,
                text: row.get(5)?,
                note: row.get(6)?,
                rects_json: row.get(7)?,
                created_at: row.get(8)?,
                updated_at: row.get(9)?,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    pub fn annotations_for_page(&self, book_id: i64, page: usize) -> Result<Vec<Annotation>> {
        Ok(self
            .annotations_for_book(book_id)?
            .into_iter()
            .filter(|annotation| annotation.page == page)
            .collect())
    }

    #[allow(clippy::too_many_arguments)]
    pub fn add_annotation(
        &self,
        book_id: i64,
        kind: &str,
        page: usize,
        color: Option<&str>,
        text: Option<&str>,
        note: Option<&str>,
        rects_json: Option<&str>,
    ) -> Result<i64> {
        let now = unix_now();
        let conn = self.conn();
        conn.execute(
            r#"INSERT INTO annotations
               (book_id, kind, page, color, text, note, rects_json, created_at, updated_at)
               VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?8)"#,
            params![
                book_id,
                kind,
                page as i64,
                color,
                text,
                note,
                rects_json,
                now
            ],
        )?;
        Ok(conn.last_insert_rowid())
    }

    pub fn update_annotation_note(&self, annotation_id: i64, note: &str) -> Result<()> {
        let conn = self.conn();
        conn.execute(
            "UPDATE annotations SET note = ?1, updated_at = ?2 WHERE id = ?3",
            params![note, unix_now(), annotation_id],
        )?;
        Ok(())
    }

    pub fn delete_annotation(&self, annotation_id: i64) -> Result<()> {
        let conn = self.conn();
        conn.execute("DELETE FROM annotations WHERE id = ?1", [annotation_id])?;
        Ok(())
    }

    pub fn start_reading_session(&self, book_id: i64) -> Result<i64> {
        let conn = self.conn();
        conn.execute(
            "INSERT INTO reading_sessions (book_id, started_at) VALUES (?1, ?2)",
            params![book_id, unix_now()],
        )?;
        Ok(conn.last_insert_rowid())
    }

    pub fn finish_reading_session(
        &self,
        session_id: i64,
        pages_read: i64,
        duration_seconds: i64,
    ) -> Result<()> {
        let conn = self.conn();
        conn.execute(
            "UPDATE reading_sessions
             SET ended_at = ?1, pages_read = ?2, duration_seconds = ?3
             WHERE id = ?4",
            params![
                unix_now(),
                pages_read.max(0),
                duration_seconds.max(0),
                session_id
            ],
        )?;
        Ok(())
    }

    /// Read a generic application setting.
    pub fn setting(&self, key: &str) -> Result<Option<String>> {
        let conn = self.conn();
        Ok(conn
            .query_row("SELECT value FROM settings WHERE key = ?1", [key], |row| {
                row.get(0)
            })
            .optional()?)
    }

    /// Write a generic application setting.
    pub fn set_setting(&self, key: &str, value: &str) -> Result<()> {
        let conn = self.conn();
        conn.execute(
            "INSERT INTO settings (key, value) VALUES (?1, ?2)
             ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            params![key, value],
        )?;
        Ok(())
    }

    /// Aggregate reading statistics for the whole library.
    pub fn library_stats(&self) -> Result<LibraryStats> {
        let conn = self.conn();
        let total_books = conn.query_row("SELECT COUNT(*) FROM books", [], |row| row.get(0))?;
        let reading = conn.query_row(
            "SELECT COUNT(*) FROM books WHERE reading_status = 'reading'",
            [],
            |row| row.get(0),
        )?;
        let finished = conn.query_row(
            "SELECT COUNT(*) FROM books WHERE reading_status = 'finished'",
            [],
            |row| row.get(0),
        )?;
        let (sessions, seconds, pages): (i64, i64, i64) = conn.query_row(
            "SELECT COUNT(*), COALESCE(SUM(duration_seconds), 0),
                    COALESCE(SUM(pages_read), 0)
             FROM reading_sessions WHERE ended_at IS NOT NULL",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )?;
        Ok(LibraryStats {
            total_books,
            reading,
            finished,
            sessions,
            seconds,
            pages,
        })
    }

    pub fn reading_stats(&self, book_id: i64) -> Result<(i64, i64)> {
        let conn = self.conn();
        let (seconds, pages): (i64, i64) = conn.query_row(
            "SELECT COALESCE(SUM(duration_seconds), 0), COALESCE(SUM(pages_read), 0)
             FROM reading_sessions WHERE book_id = ?1",
            [book_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?;
        Ok((seconds, pages))
    }

    pub fn roots(&self) -> Result<Vec<PathBuf>> {
        if let Some(raw) = std::env::var_os("GRIMOIR_LIBRARY_ROOTS") {
            return Ok(std::env::split_paths(&raw)
                .filter(|path| !path.as_os_str().is_empty())
                .map(normalize_root)
                .collect());
        }

        let conn = self.conn();
        let raw: Option<String> = conn
            .query_row(
                "SELECT value FROM settings WHERE key = 'roots'",
                [],
                |row| row.get(0),
            )
            .optional()?;
        let Some(raw) = raw else {
            return Ok(Vec::new());
        };
        let paths: Vec<String> = serde_json::from_str(&raw).unwrap_or_default();
        Ok(paths.into_iter().map(PathBuf::from).collect())
    }

    pub fn set_roots(&self, roots: &[PathBuf]) -> Result<()> {
        let mut seen = Vec::new();
        for root in roots {
            let root = normalize_root(root.clone());
            if !seen.contains(&root) {
                seen.push(root);
            }
        }
        let encoded = serde_json::to_string(
            &seen
                .iter()
                .map(|path| path.to_string_lossy().into_owned())
                .collect::<Vec<_>>(),
        )?;
        let conn = self.conn();
        conn.execute(
            "INSERT INTO settings (key, value) VALUES ('roots', ?1)
             ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            [encoded],
        )?;
        Ok(())
    }

    pub fn add_root(&self, root: &Path) -> Result<Vec<PathBuf>> {
        let mut roots = self.roots()?;
        let root = normalize_root(root.to_path_buf());
        if !roots.contains(&root) {
            roots.push(root);
            self.set_roots(&roots)?;
        }
        Ok(roots)
    }

    pub fn remove_root(&self, root: &Path) -> Result<Vec<PathBuf>> {
        let root = normalize_root(root.to_path_buf());
        let roots: Vec<PathBuf> = self
            .roots()?
            .into_iter()
            .filter(|candidate| candidate != &root)
            .collect();
        self.set_roots(&roots)?;
        Ok(roots)
    }

    /// Removes a root and deletes every indexed book below it. The files on
    /// disk are never touched.
    pub fn forget_root(&self, root: &Path) -> Result<Vec<PathBuf>> {
        let root = normalize_root(root.to_path_buf());
        let books = self.all_books()?;
        for book in books.iter().filter(|book| book.path.starts_with(&root)) {
            if let Some(cover) = &book.cover_path {
                let _ = fs::remove_file(cover);
            }
            let conn = self.conn();
            conn.execute("DELETE FROM books WHERE id = ?1", [book.id])?;
        }
        self.remove_root(&root)
    }

    /// Removes books that belong to a scanned root but were not seen in the
    /// completed walk. Cover files are deleted as well.
    pub fn prune_missing(&self, roots: &[PathBuf], seen: &[PathBuf]) -> Result<usize> {
        let books = self.all_books()?;
        let mut removed = 0;
        for book in books {
            let under_root = roots.iter().any(|root| book.path.starts_with(root));
            if !under_root || seen.contains(&book.path) {
                continue;
            }
            if let Some(cover) = &book.cover_path {
                let _ = fs::remove_file(cover);
            }
            let conn = self.conn();
            conn.execute("DELETE FROM books WHERE id = ?1", [book.id])?;
            removed += 1;
        }
        Ok(removed)
    }
}

fn migrate(conn: &Connection) -> Result<()> {
    let version: i64 = conn.query_row("PRAGMA user_version", [], |row| row.get(0))?;
    if version < 1 {
        conn.execute_batch(SCHEMA)?;
        conn.pragma_update(None, "user_version", 1)?;
    }
    Ok(())
}

fn book_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Book> {
    Ok(Book {
        id: row.get(0)?,
        path: PathBuf::from(row.get::<_, String>(1)?),
        format: row.get(2)?,
        title: row.get(3)?,
        authors: row.get(4)?,
        series: row.get(5)?,
        series_index: row.get(6)?,
        page_count: row
            .get::<_, Option<i64>>(7)?
            .map(|value| value.max(0) as usize),
        cover_path: row.get::<_, Option<String>>(8)?.map(PathBuf::from),
        file_size: row.get(9)?,
        file_mtime: row.get(10)?,
        added_at: row.get(11)?,
        last_opened_at: row.get(12)?,
        favorite: row.get::<_, i64>(13)? != 0,
        reading_status: row.get(14)?,
        progress_page: row
            .get::<_, Option<i64>>(15)?
            .map(|value| value.max(0) as usize),
        progress_percent: row.get::<_, Option<f64>>(16)?.unwrap_or(0.0),
    })
}

pub fn unix_now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64
}

fn env_dir(variable: &str, fallback: &str) -> PathBuf {
    std::env::var_os(variable)
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(fallback)))
        .unwrap_or_else(|| PathBuf::from(fallback))
}

fn normalize_root(root: PathBuf) -> PathBuf {
    if let Ok(canonical) = root.canonicalize() {
        return canonical;
    }
    if root.is_absolute() {
        root
    } else {
        std::env::current_dir()
            .map(|cwd| cwd.join(&root))
            .unwrap_or(root)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stores_books_and_progress() {
        let base = std::env::temp_dir().join(format!(
            "grimoir-library-test-{}-{}",
            std::process::id(),
            unix_now()
        ));
        let library = Library::open(&base, base.join("cache")).unwrap();
        let scanned = ScannedBook {
            path: PathBuf::from("/tmp/example.pdf"),
            format: "pdf".to_owned(),
            title: "Example".to_owned(),
            authors: Some("Someone".to_owned()),
            series: Some("Series".to_owned()),
            series_index: Some(2.0),
            page_count: Some(42),
            file_size: 100,
            file_mtime: 200,
        };
        let book = library.upsert_scanned(&scanned).unwrap();
        assert_eq!(book.title, "Example");
        assert_eq!(book.series_label().as_deref(), Some("Series #2"));

        library.save_progress(book.id, 12, 12.0 / 42.0).unwrap();
        let progress = library.progress_for_path(&scanned.path).unwrap().unwrap();
        assert_eq!(progress.page, 12);

        let roots = vec![PathBuf::from("/tmp")];
        library.set_roots(&roots).unwrap();
        assert_eq!(library.roots().unwrap(), roots);

        library.forget_root(Path::new("/tmp")).unwrap();
        assert!(library.roots().unwrap().is_empty());
        assert!(library.all_books().unwrap().is_empty());

        let _ = fs::remove_dir_all(base);
    }
}
