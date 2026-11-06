//! Debounced filesystem watcher for library roots.

use std::path::PathBuf;
use std::time::Duration;

use anyhow::{Context, Result};
use notify::{RecommendedWatcher, RecursiveMode, Watcher};

#[derive(Debug, Clone, Copy)]
pub enum WatchEvent {
    Changed,
}

pub struct WatchHandle {
    _watcher: RecommendedWatcher,
    _thread: std::thread::JoinHandle<()>,
}

pub fn watch(roots: &[PathBuf], events: async_channel::Sender<WatchEvent>) -> Result<WatchHandle> {
    let (raw_tx, raw_rx) = std::sync::mpsc::channel();
    let mut watcher = notify::recommended_watcher(move |event: notify::Result<notify::Event>| {
        if let Ok(event) = event {
            // Scanning opens every PDF to read metadata and render covers.
            // Opening files emits access events; treating those as changes
            // would immediately schedule another scan, forever.
            if !matches!(event.kind, notify::EventKind::Access(_)) {
                let _ = raw_tx.send(());
            }
        }
    })
    .context("failed to create filesystem watcher")?;

    for root in roots.iter().filter(|root| root.exists()) {
        watcher
            .watch(root, RecursiveMode::Recursive)
            .with_context(|| format!("failed to watch {}", root.display()))?;
    }

    let thread = std::thread::Builder::new()
        .name("grimoir-library-watch".to_owned())
        .spawn(move || {
            while raw_rx.recv().is_ok() {
                loop {
                    match raw_rx.recv_timeout(Duration::from_millis(700)) {
                        Ok(()) => {}
                        Err(std::sync::mpsc::RecvTimeoutError::Timeout) => break,
                        Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => return,
                    }
                }
                if events.send_blocking(WatchEvent::Changed).is_err() {
                    return;
                }
            }
        })
        .context("failed to spawn filesystem watcher thread")?;

    Ok(WatchHandle {
        _watcher: watcher,
        _thread: thread,
    })
}
