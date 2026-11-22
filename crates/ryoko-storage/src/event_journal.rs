//! Versioned event journal with replayable, validated events.

use std::collections::BTreeMap;
use std::time::{SystemTime, UNIX_EPOCH};

use chrono::{DateTime, Utc};
use rusqlite::{params, Connection};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::{Repository, StorageError, MAX_EVENT_SIZE};

/// Unique identifier for a journal event.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct EventId(pub Uuid);

impl EventId {
    pub fn new() -> Self {
        Self(Uuid::new_v4())
    }
}

impl Default for EventId {
    fn default() -> Self {
        Self::new()
    }
}

/// Event version for schema evolution.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct EventVersion(pub u32);

impl EventVersion {
    pub const CURRENT: Self = Self(1);

    pub fn new(v: u32) -> Self {
        Self(v)
    }
}

/// A versioned, replayable journal event.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JournalEvent {
    pub id: EventId,
    pub version: EventVersion,
    pub event_type: String,
    pub timestamp: DateTime<Utc>,
    pub payload: serde_json::Value,
    pub source: EventSource,
    pub validated: bool,
    pub replayable: bool,
    pub replay_context: Option<ReplayContext>,
}

/// Source of an event.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EventSource {
    Orchestrator,
    Agent,
    Validator,
    HumanOperator,
    System,
}

/// Context needed for safe event replay.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReplayContext {
    pub task_id: Uuid,
    pub attempt_number: u32,
    pub context_snapshot: serde_json::Value,
    pub prior_events: Vec<EventId>,
}

/// Store for the event journal.
pub struct EventJournal {
    repo: Arc<Repository>,
}

impl EventJournal {
    /// Creates a new event journal.
    pub fn new(repo: Arc<Repository>) -> Self {
        Self { repo }
    }

    /// Appends an event to the journal.
    pub fn append(&self, event: &JournalEvent) -> Result<EventId, StorageError> {
        let payload_size = serde_json::to_vec(&event.payload)?.len();
        if payload_size > MAX_EVENT_SIZE {
            return Err(StorageError::EventPayloadTooLarge(payload_size as u64));
        }

        self.repo.with_write(|conn| {
            conn.execute(
                "INSERT INTO event_journal (event_id, version, event_type, timestamp, payload, source, validated, replayable)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                params![
                    event.id.0.to_string(),
                    event.version.0,
                    event.event_type,
                    event.timestamp.timestamp(),
                    serde_json::to_string(&event.payload)?,
                    serde_json::to_string(&event.source)?,
                    event.validated,
                    event.replayable,
                ],
            ).map_err(StorageError::Sqlite)?;
            Ok(())
        })?;

        Ok(event.id)
    }

    /// Appends multiple events atomically.
    pub fn append_batch(&self, events: &[JournalEvent]) -> Result<Vec<EventId>, StorageError> {
        let ids: Vec<_> = events.iter().map(|e| e.id).collect();

        self.repo.with_transaction(|tx| {
            for event in events {
                let payload_size = serde_json::to_vec(&event.payload)?.len();
                if payload_size > MAX_EVENT_SIZE {
                    return Err(StorageError::EventPayloadTooLarge(payload_size as u64));
                }

                tx.execute(
                    "INSERT INTO event_journal (event_id, version, event_type, timestamp, payload, source, validated, replayable)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                    params![
                        event.id.0.to_string(),
                        event.version.0,
                        event.event_type,
                        event.timestamp.timestamp(),
                        serde_json::to_string(&event.payload)?,
                        serde_json::to_string(&event.source)?,
                        event.validated,
                        event.replayable,
                    ],
                ).map_err(StorageError::Sqlite)?;
            }
            Ok(())
        })?;

        Ok(ids)
    }

    /// Gets events for a task.
    pub fn get_events_for_task(&self, task_id: Uuid) -> Result<Vec<JournalEvent>, StorageError> {
        self.repo.with_read(|conn| {
            let mut stmt = conn.prepare(
                "SELECT event_id, version, event_type, timestamp, payload, source, validated, replayable
                 FROM event_journal WHERE json_extract(payload, '$.task_id') = ?1
                 ORDER BY timestamp"
            )?;

            let rows = stmt.query_map(params![task_id.to_string()], |row| {
                Ok(JournalEvent {
                    id: EventId(Uuid::parse_str(&row.get::<_, String>(0)?).unwrap()),
                    version: EventVersion(row.get(1)?),
                    event_type: row.get(2)?,
                    timestamp: DateTime::from_timestamp(row.get::<_, i64>(3)?, 0).unwrap_or_default(),
                    payload: serde_json::from_str(&row.get::<_, String>(4)?).unwrap_or_default(),
                    source: serde_json::from_str(&row.get::<_, String>(5)?).unwrap_or(EventSource::System),
                    validated: row.get(6)?,
                    replayable: row.get(7)?,
                    replay_context: None,
                })
            })?;

            Ok(rows.filter_map(Result::ok).collect())
        })
    }

    /// Gets all events in order.
    pub fn get_all_events(&self, limit: Option<usize>) -> Result<Vec<JournalEvent>, StorageError> {
        self.repo.with_read(|conn| {
            let sql = if let Some(limit) = limit {
                format!(
                    "SELECT event_id, version, event_type, timestamp, payload, source, validated, replayable
                     FROM event_journal ORDER BY timestamp LIMIT {}",
                    limit
                )
            } else {
                "SELECT event_id, version, event_type, timestamp, payload, source, validated, replayable
                 FROM event_journal ORDER BY timestamp".to_string()
            };

            let mut stmt = conn.prepare(&sql)?;

            let rows = stmt.query_map([], |row| {
                Ok(JournalEvent {
                    id: EventId(Uuid::parse_str(&row.get::<_, String>(0)?).unwrap()),
                    version: EventVersion(row.get(1)?),
                    event_type: row.get(2)?,
                    timestamp: DateTime::from_timestamp(row.get::<_, i64>(3)?, 0).unwrap_or_default(),
                    payload: serde_json::from_str(&row.get::<_, String>(4)?).unwrap_or_default(),
                    source: serde_json::from_str(&row.get::<_, String>(5)?).unwrap_or(EventSource::System),
                    validated: row.get(6)?,
                    replayable: row.get(7)?,
                    replay_context: None,
                })
            })?;

            Ok(rows.filter_map(Result::ok).collect())
        })
    }

    /// Gets events by type.
    pub fn get_events_by_type(&self, event_type: &str, limit: Option<usize>) -> Result<Vec<JournalEvent>, StorageError> {
        self.repo.with_read(|conn| {
            let sql = if let Some(limit) = limit {
                format!(
                    "SELECT event_id, version, event_type, timestamp, payload, source, validated, replayable
                     FROM event_journal WHERE event_type = ?1 ORDER BY timestamp LIMIT {}",
                    limit
                )
            } else {
                "SELECT event_id, version, event_type, timestamp, payload, source, validated, replayable
                 FROM event_journal WHERE event_type = ?1 ORDER BY timestamp".to_string()
            };

            let mut stmt = conn.prepare(&sql)?;

            let rows = stmt.query_map(params![event_type], |row| {
                Ok(JournalEvent {
                    id: EventId(Uuid::parse_str(&row.get::<_, String>(0)?).unwrap()),
                    version: EventVersion(row.get(1)?),
                    event_type: row.get(2)?,
                    timestamp: DateTime::from_timestamp(row.get::<_, i64>(3)?, 0).unwrap_or_default(),
                    payload: serde_json::from_str(&row.get::<_, String>(4)?).unwrap_or_default(),
                    source: serde_json::from_str(&row.get::<_, String>(5)?).unwrap_or(EventSource::System),
                    validated: row.get(6)?,
                    replayable: row.get(7)?,
                    replay_context: None,
                })
            })?;

            Ok(rows.filter_map(Result::ok).collect())
        })
    }

    /// Marks an event as validated after replay verification.
    pub fn validate_event(&self, event_id: EventId) -> Result<(), StorageError> {
        self.repo.with_write(|conn| {
            conn.execute(
                "UPDATE event_journal SET validated = 1 WHERE event_id = ?1",
                params![event_id.0.to_string()],
            ).map_err(StorageError::Sqlite)?;
            Ok(())
        })
    }

    /// Creates a checkpoint in the event journal.
    pub fn create_checkpoint(&self, checkpoint_id: &str, last_event_id: EventId, position: i64) -> Result<(), StorageError> {
        self.repo.with_write(|conn| {
            conn.execute(
                "INSERT OR REPLACE INTO event_checkpoints (checkpoint_id, last_event_id, position, created_at, schema_version)
                 VALUES (?1, ?2, ?3, ?4, ?5)",
                params![
                    checkpoint_id,
                    last_event_id.0.to_string(),
                    position,
                    SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_secs() as i64,
                    crate::SCHEMA_VERSION,
                ],
            ).map_err(StorageError::Sqlite)?;
            Ok(())
        })
    }

    /// Gets the latest checkpoint.
    pub fn get_latest_checkpoint(&self) -> Result<Option<EventCheckpoint>, StorageError> {
        self.repo.with_read(|conn| {
            let mut stmt = conn.prepare(
                "SELECT checkpoint_id, last_event_id, position, created_at, schema_version
                 FROM event_checkpoints ORDER BY created_at DESC LIMIT 1"
            )?;

            let checkpoint = stmt.query_row([], |row| {
                Ok(EventCheckpoint {
                    checkpoint_id: row.get(0)?,
                    last_event_id: EventId(Uuid::parse_str(&row.get::<_, String>(1)?).unwrap()),
                    position: row.get(2)?,
                    created_at: DateTime::from_timestamp(row.get::<_, i64>(3)?, 0).unwrap_or_default(),
                    schema_version: row.get(4)?,
                })
            }).ok();

            Ok(checkpoint)
        })
    }

    /// Replays events from a checkpoint with validation.
    pub fn replay_from<F>(&self, checkpoint: &EventCheckpoint, mut handler: F) -> Result<ReplayResult, StorageError>
    where
        F: FnMut(&JournalEvent) -> Result<(), StorageError>,
    {
        let events = self.get_events_after(checkpoint.position)?;
        let mut replayed = 0;
        let mut failed = 0;
        let mut skipped = 0;

        for event in events {
            if !event.replayable {
                skipped += 1;
                continue;
            }

            if !event.validated {
                // Re-validate before replay
                match self.validate_event_content(&event) {
                    Ok(valid) => {
                        if !valid {
                            failed += 1;
                            continue;
                        }
                    }
                    Err(_) => {
                        failed += 1;
                        continue;
                    }
                }
            }

            match handler(&event) {
                Ok(_) => replayed += 1,
                Err(_) => failed += 1,
            }
        }

        Ok(ReplayResult { replayed, failed, skipped })
    }

    /// Gets events after a given position.
    fn get_events_after(&self, position: i64) -> Result<Vec<JournalEvent>, StorageError> {
        self.repo.with_read(|conn| {
            let mut stmt = conn.prepare(
                "SELECT event_id, version, event_type, timestamp, payload, source, validated, replayable
                 FROM event_journal WHERE rowid > ?1 ORDER BY timestamp"
            )?;

            let rows = stmt.query_map(params![position], |row| {
                Ok(JournalEvent {
                    id: EventId(Uuid::parse_str(&row.get::<_, String>(0)?).unwrap()),
                    version: EventVersion(row.get(1)?),
                    event_type: row.get(2)?,
                    timestamp: DateTime::from_timestamp(row.get::<_, i64>(3)?, 0).unwrap_or_default(),
                    payload: serde_json::from_str(&row.get::<_, String>(4)?).unwrap_or_default(),
                    source: serde_json::from_str(&row.get::<_, String>(5)?).unwrap_or(EventSource::System),
                    validated: row.get(6)?,
                    replayable: row.get(7)?,
                    replay_context: None,
                })
            })?;

            Ok(rows.filter_map(Result::ok).collect())
        })
    }

    /// Validates an event's content for safe replay.
    fn validate_event_content(&self, event: &JournalEvent) -> Result<bool, StorageError> {
        // Basic validation: check required fields, size limits, etc.
        if event.payload.is_null() {
            return Ok(false);
        }

        // Check version compatibility
        if event.version.0 > EventVersion::CURRENT.0 {
            return Ok(false);
        }

        // Additional validation based on event type
        match event.event_type.as_str() {
            "task_started" | "task_completed" | "task_failed" => {
                // Ensure task_id is present
                Ok(event.payload.get("task_id").is_some())
            }
            "tool_executed" => {
                // Ensure tool name and outcome are present
                Ok(event.payload.get("tool").is_some() && event.payload.get("outcome").is_some())
            }
            "approval_granted" | "approval_denied" => {
                // Ensure tool and scope are present
                Ok(event.payload.get("tool").is_some() && event.payload.get("scope").is_some())
            }
            _ => Ok(true),
        }
    }
}

/// Event checkpoint for replay resumption.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EventCheckpoint {
    pub checkpoint_id: String,
    pub last_event_id: EventId,
    pub position: i64,
    pub created_at: DateTime<Utc>,
    pub schema_version: String,
}

/// Result of a replay operation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReplayResult {
    pub replayed: usize,
    pub failed: usize,
    pub skipped: usize,
}

impl ReplayResult {
    pub fn success(&self) -> bool {
        self.failed == 0
    }
}

/// Standard event types for the journal.
pub mod event_types {
    pub const TASK_STARTED: &str = "task_started";
    pub const TASK_COMPLETED: &str = "task_completed";
    pub const TASK_FAILED: &str = "task_failed";
    pub const TASK_SKIPPED: &str = "task_skipped";
    pub const TASK_PAUSED: &str = "task_paused";
    pub const TASK_RESUMED: &str = "task_resumed";
    pub const TASK_CANCELLED: &str = "task_cancelled";
    pub const TASK_APPROVAL_REQUESTED: &str = "task_approval_requested";
    pub const TASK_APPROVAL_GRANTED: &str = "task_approval_granted";
    pub const TASK_APPROVAL_DENIED: &str = "task_approval_denied";
    pub const TOOL_EXECUTED: &str = "tool_executed";
    pub const VALIDATION_STARTED: &str = "validation_started";
    pub const VALIDATION_PASSED: &str = "validation_passed";
    pub const VALIDATION_FAILED: &str = "validation_failed";
    pub const REPAIR_STARTED: &str = "repair_started";
    pub const REPAIR_COMPLETED: &str = "repair_completed";
    pub const CHECKPOINT_CREATED: &str = "checkpoint_created";
    pub const MODEL_REQUEST: &str = "model_request";
    pub const MODEL_RESPONSE: &str = "model_response";
    pub const MODEL_ERROR: &str = "model_error";
}