//! Storage error types.

use thiserror::Error;

/// Errors that can occur in storage operations.
#[derive(Debug, Error)]
pub enum StorageError {
    #[error("I/O error: {0}")]
    Io(String),

    #[error("SQLite error: {0}")]
    Sqlite(#[from] rusqlite::Error),

    #[error("Invalid event version: {0}")]
    InvalidEventVersion(String),

    #[error("Event payload too large: {0} bytes (max {MAX_EVENT_SIZE})", max = crate::MAX_EVENT_SIZE)]
    EventPayloadTooLarge(u64),

    #[error("Task not found: {0}")]
    TaskNotFound(uuid::Uuid),

    #[error("Checkpoint corruption: {0}")]
    CheckpointCorruption(String),

    #[error("Migration failed: {0}")]
    MigrationFailed(String),
}