//! SQLite-backed persistent storage for Ryoko's durable state.
//!
//! This module provides:
//! - Durable task persistence with execution state recovery
//! - Event journal with versioned, replayable events
//! - Project memory persistence with provenance
//! - Approval decision persistence with audit metadata
//!
//! All state is stored in SQLite databases under `.ryoko/state/` for crash recovery.

mod schema;
mod repository;
mod task_store;
mod event_journal;
mod memory_store;
mod approval_store;
mod checkpoint;
mod error;

pub use error::StorageError;
pub use schema::DatabaseSchema;
pub use repository::{Repository, RepositoryConnection};
pub use task_store::{TaskStore, TaskRecord, TaskStatus, AttemptRecord, ExecutionState};
pub use event_journal::{EventJournal, JournalEvent, EventId, EventVersion};
pub use memory_store::{MemoryStore, MemoryEntry, MemoryCategory};
pub use approval_store::{ApprovalStore, ApprovalRecord, ApprovalDecision, ApprovalMetadata};
pub use checkpoint::{Checkpoint, CheckpointBuilder, CheckpointReader};

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use async_trait::async_trait;
use rusqlite::{Connection, OpenFlags, params};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use thiserror::Error;
use tokio::sync::RwLock;
use uuid::Uuid;

/// The current schema version for migration tracking.
pub const SCHEMA_VERSION: &str = "1.0.0";

/// Maximum size for event payloads (10 MB).
pub const MAX_EVENT_SIZE: usize = 10 * 1024 * 1024;

/// Default location for state databases.
pub fn default_state_path() -> PathBuf {
    dirs::cache_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("ryoko")
        .join("state")
}

/// Creates a new storage repository at the given project path.
pub fn create_storage(project_path: &Path) -> Result<Repository, StorageError> {
    let state_dir = project_path.join(".ryoko").join("state");
    std::fs::create_dir_all(&state_dir)
        .map_err(|e| StorageError::Io(e.to_string()))?;
    Repository::open_at(&state_dir, "state")
}

/// Opens an existing storage repository, or creates a new one if it doesn't exist.
pub fn open_storage(project_path: &Path) -> Result<Repository, StorageError> {
    let state_dir = project_path.join(".ryoko").join("state");
    if !state_dir.exists() {
        std::fs::create_dir_all(&state_dir)
            .map_err(|e| StorageError::Io(e.to_string()))?;
    }
    Repository::open_at(&state_dir, "state")
}