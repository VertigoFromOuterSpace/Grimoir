//! Background PDF rendering and text search.
//!
//! PDFium documents are not `Send`, so the worker thread owns the document for
//! its entire lifetime. The UI only sends page requests and receives rendered
//! buffers.

use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Condvar, Mutex};
use std::thread;

use anyhow::{Result, anyhow};

use crate::{PdfBook, RawPage, SearchHit};

/// Document-level information collected once, on the worker thread.
#[derive(Debug, Clone)]
pub struct PdfInfo {
    pub page_count: usize,
    /// Page dimensions in PDF points, in document order.
    pub page_sizes: Vec<(f32, f32)>,
    pub table_of_contents: Vec<TocEntry>,
}

#[derive(Debug, Clone)]
pub struct TocEntry {
    pub title: String,
    pub page: Option<usize>,
    pub depth: usize,
}

#[derive(Debug)]
pub enum WorkerEvent {
    Rendered {
        index: usize,
        result: Result<RawPage, String>,
    },
    SearchFinished {
        query: String,
        results: Result<Vec<SearchHit>, String>,
    },
}

#[derive(Debug, Clone)]
enum Command {
    Render {
        index: usize,
        max_width: u32,
        max_height: u32,
    },
    Search {
        query: String,
    },
}

#[derive(Default)]
struct QueueState {
    urgent: VecDeque<Command>,
    normal: VecDeque<Command>,
    closed: bool,
}

/// Two-level request queue. Page turns are urgent; background prefetches are
/// normal. That keeps a slow scan from sitting in front of the page the reader
/// is actually waiting for.
struct CommandQueue {
    state: Mutex<QueueState>,
    ready: Condvar,
}

impl CommandQueue {
    fn new() -> Self {
        Self {
            state: Mutex::new(QueueState::default()),
            ready: Condvar::new(),
        }
    }

    fn push_urgent(&self, command: Command) {
        let mut state = self.state.lock().unwrap();
        if let Command::Render { index, .. } = command {
            if let Some(position) = state.normal.iter().position(
                |queued| matches!(queued, Command::Render { index: queued_index, .. } if *queued_index == index),
            ) {
                let promoted = state.normal.remove(position).unwrap();
                state.urgent.push_back(promoted);
            } else if !state.urgent.iter().any(
                |queued| matches!(queued, Command::Render { index: queued_index, .. } if *queued_index == index),
            ) {
                state.urgent.push_back(command);
            }
        } else {
            state.urgent.push_back(command);
        }
        drop(state);
        self.ready.notify_one();
    }

    fn push_normal(&self, command: Command) {
        let Command::Render { index, .. } = command else {
            return;
        };
        let mut state = self.state.lock().unwrap();
        if state.urgent.iter().chain(state.normal.iter()).any(
            |queued| matches!(queued, Command::Render { index: queued_index, .. } if *queued_index == index),
        ) {
            return;
        }
        state.normal.push_back(command);
        drop(state);
        self.ready.notify_one();
    }

    fn next(&self) -> Option<Command> {
        let mut state = self.state.lock().unwrap();
        loop {
            if let Some(command) = state.urgent.pop_front() {
                return Some(command);
            }
            if let Some(command) = state.normal.pop_front() {
                return Some(command);
            }
            if state.closed {
                return None;
            }
            state = self.ready.wait(state).unwrap();
        }
    }

    fn shutdown(&self) {
        let mut state = self.state.lock().unwrap();
        state.closed = true;
        state.urgent.clear();
        state.normal.clear();
        drop(state);
        self.ready.notify_all();
    }
}

pub struct PdfWorker {
    commands: Arc<CommandQueue>,
    path: PathBuf,
}

impl PdfWorker {
    /// Opens the document on a dedicated thread. Blocks only until the
    /// document is open and its page metadata has been read.
    pub fn spawn(
        path: impl AsRef<Path>,
        events: async_channel::Sender<WorkerEvent>,
    ) -> Result<(Self, PdfInfo)> {
        let path = path.as_ref().to_path_buf();
        let thread_path = path.clone();
        let commands = Arc::new(CommandQueue::new());
        let thread_commands = Arc::clone(&commands);
        let (init_tx, init_rx) = std::sync::mpsc::channel();

        thread::Builder::new()
            .name("grimoir-pdf-render".to_owned())
            .spawn(move || match PdfBook::open(&thread_path) {
                Ok(book) => {
                    let info = PdfInfo {
                        page_count: book.page_count(),
                        page_sizes: book.page_sizes(),
                        table_of_contents: book.table_of_contents(),
                    };
                    if init_tx.send(Ok(info)).is_err() {
                        return;
                    }

                    while let Some(command) = thread_commands.next() {
                        match command {
                            Command::Render {
                                index,
                                max_width,
                                max_height,
                            } => {
                                let result = book
                                    .render_page(index, max_width, max_height)
                                    .map_err(|error| format!("{error:#}"));
                                if events
                                    .send_blocking(WorkerEvent::Rendered { index, result })
                                    .is_err()
                                {
                                    break;
                                }
                            }
                            Command::Search { query } => {
                                let results = Ok(book.search(&query, 200));
                                if events
                                    .send_blocking(WorkerEvent::SearchFinished { query, results })
                                    .is_err()
                                {
                                    break;
                                }
                            }
                        }
                    }
                }
                Err(error) => {
                    let _ = init_tx.send(Err(format!("{error:#}")));
                }
            })
            .map_err(|error| anyhow!("failed to start PDF worker: {error}"))?;

        let info = init_rx
            .recv()
            .map_err(|_| anyhow!("PDF worker stopped before opening the document"))?
            .map_err(|error| anyhow!(error))?;

        Ok((Self { commands, path }, info))
    }

    /// Requests a page that is visible or about to become visible.
    pub fn request_page(&self, index: usize, max_width: u32, max_height: u32) {
        self.commands.push_urgent(Command::Render {
            index,
            max_width,
            max_height,
        });
    }

    /// Requests a page for background prefetching. Normal requests yield to
    /// urgent page turns.
    pub fn prefetch_page(&self, index: usize, max_width: u32, max_height: u32) {
        self.commands.push_normal(Command::Render {
            index,
            max_width,
            max_height,
        });
    }

    /// Runs a text search on the worker thread.
    pub fn search(&self, query: impl Into<String>) {
        self.commands.push_urgent(Command::Search {
            query: query.into(),
        });
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Stops the worker and releases the PDF document. Safe to call repeatedly.
    pub fn shutdown(&self) {
        self.commands.shutdown();
    }
}

impl Drop for PdfWorker {
    fn drop(&mut self) {
        self.shutdown();
    }
}
