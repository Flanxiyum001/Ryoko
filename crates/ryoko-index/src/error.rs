//! Error types for the index crate.

use thiserror::Error;

/// Errors that can occur during indexing operations.
#[derive(Debug, thiserror::Error)]
pub enum IndexError {
    #[error("SQL error: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
    #[error("invalid path: {0}")]
    InvalidPath(String),
    #[error("filesystem watcher error: {0}")]
    Watcher(String),
    #[error("file parse error: {0}")]
    Parse(String),
    #[error("malformed source file: {0}")]
    MalformedSource(String),
    #[error("notify error: {0}")]
    Notify(#[from] notify::Error),
}