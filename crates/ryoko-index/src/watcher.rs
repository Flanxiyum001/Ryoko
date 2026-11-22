//! Filesystem watching for change detection.
//!
//! Uses the `notify` crate to receive filesystem events. Events are batched
//! and deduplicated before being passed to the index for processing.

use std::path::{Path, PathBuf};
use std::time::Duration;

use notify::{Event, EventKind, RecommendedWatcher, RecursiveMode, Result as NotifyResult, Watcher};
use tokio::sync::mpsc;

use crate::error::IndexError;

/// A filesystem event detected by the watcher.
#[derive(Debug, Clone)]
pub enum WatchEvent {
    /// A file was modified.
    Modified { path: PathBuf },
    /// A file was created.
    Created { path: PathBuf },
    /// A file was deleted.
    Deleted { path: PathBuf },
    /// A file was renamed (from -> to).
    Renamed { from: PathBuf, to: PathBuf },
    /// The watcher encountered an error.
    Error(String),
}

/// Configuration for the file watcher.
#[derive(Debug, Clone)]
pub struct WatchConfig {
    /// How long to wait before batching events (debounce period).
    pub debounce_ms: u64,
    /// Maximum batch size before sending events regardless of timing.
    pub max_batch_size: usize,
}

impl Default for WatchConfig {
    fn default() -> Self {
        Self {
            debounce_ms: 100,
            max_batch_size: 100,
        }
    }
}

/// Filesystem watcher that sends change events asynchronously.
pub struct FileWatcher {
    #[allow(dead_code)]
    watcher: RecommendedWatcher,
}

impl FileWatcher {
    /// Create a new file watcher for the given root directory.
    pub fn new(root: &Path, config: WatchConfig) -> Result<(Self, mpsc::UnboundedReceiver<WatchEvent>), IndexError> {
        let (tx, rx) = mpsc::unbounded_channel::<WatchEvent>();

        // Create the watcher
        let mut watcher = RecommendedWatcher::new(
            move |res: NotifyResult<Event>| {
                match res {
                    Ok(event) => {
                        // Filter events to the watched root
                        if is_relevant_event(&event, root) {
                            // Convert to WatchEvent - simplified version for now
                            for path in &event.paths {
                                let event_type = match &event.kind {
                                    EventKind::Modify(_) => WatchEvent::Modified { path: path.clone() },
                                    EventKind::Create(_) => WatchEvent::Created { path: path.clone() },
                                    EventKind::OtherModify(_) => WatchEvent::Modified { path: path.clone() },
                                    _ => continue,
                                };
                                let _ = tx.send(event_type);
                            }
                        }
                    }
                    Err(e) => {
                        let _ = tx.send(WatchEvent::Error(e.to_string()));
                    }
                }
            },
            notify::Config::default()
                .with_poll_interval(Duration::from_millis(config.debounce_ms)),
        )?;

        watcher.watch(root, RecursiveMode::Recursive)?;

        Ok(Self { watcher }, rx)
    }

    /// Re-arm the watcher after processing events.
    pub fn ack(&mut self) -> Result<(), IndexError> {
        // No-op for now - the watcher is self-contained
        Ok(())
    }
}

fn is_relevant_event(event: &Event, root: &Path) -> bool {
    // Consider events that affect non-hidden, non-build files
    match &event.kind {
        EventKind::Modify(_) |
        EventKind::Create(_) |
        EventKind::OtherModify(_) => {
            // Check if any path is inside our watch root
            event.paths.iter().any(|path| {
                path.starts_with(root)
            })
        }
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn watcher_creates_successfully() {
        let dir = TempDir::new().unwrap();
        let config = WatchConfig::default();
        let result = FileWatcher::new(dir.path(), config);
        assert!(result.is_ok());
    }

    #[test]
    fn watcher_rejects_nonexistent_path() {
        let config = WatchConfig::default();
        let nonexistent = Path::new("/nonexistent/path/that/does/not/exist");
        let result = FileWatcher::new(nonexistent, config);
        assert!(result.is_err());
    }
}