//! Persistent, conservative task state and append-only event journal.
//!
//! Recovery is read-only: callers must explicitly re-authorize side effects
//! before resuming an interrupted task.

use std::path::Path;

use chrono::{DateTime, SecondsFormat, Utc};
use rusqlite::{Connection, OpenFlags, OptionalExtension, params};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use uuid::Uuid;

pub const MAX_EVENT_BYTES: usize = 1024 * 1024;

#[derive(Debug, thiserror::Error)]
pub enum StorageError {
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
    #[error("database error: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("serialization error: {0}")]
    Json(#[from] serde_json::Error),
    #[error("task {0} not found")]
    TaskNotFound(Uuid),
    #[error("stale revision or conflicting event ID")]
    Conflict,
    #[error("invalid task state transition")]
    InvalidTransition,
    #[error("invalid data: {0}")]
    InvalidData(String),
    #[error("event payload exceeds 1 MiB")]
    EventTooLarge,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskState {
    Queued,
    Running,
    AwaitingApproval,
    Completed,
    Failed,
    Cancelled,
}

impl TaskState {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Queued => "queued",
            Self::Running => "running",
            Self::AwaitingApproval => "awaiting_approval",
            Self::Completed => "completed",
            Self::Failed => "failed",
            Self::Cancelled => "cancelled",
        }
    }

    fn parse(value: &str) -> Result<Self, StorageError> {
        match value {
            "queued" => Ok(Self::Queued),
            "running" => Ok(Self::Running),
            "awaiting_approval" => Ok(Self::AwaitingApproval),
            "completed" => Ok(Self::Completed),
            "failed" => Ok(Self::Failed),
            "cancelled" => Ok(Self::Cancelled),
            other => Err(StorageError::InvalidData(format!(
                "unknown task state: {other}"
            ))),
        }
    }

    pub fn may_transition(self, next: Self) -> bool {
        matches!(
            (self, next),
            (Self::Queued, Self::Running)
                | (Self::Queued, Self::Cancelled)
                | (Self::Running, Self::AwaitingApproval)
                | (Self::Running, Self::Completed)
                | (Self::Running, Self::Failed)
                | (Self::Running, Self::Cancelled)
                | (Self::AwaitingApproval, Self::Running)
                | (Self::AwaitingApproval, Self::Cancelled)
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskRecord {
    pub id: Uuid,
    pub goal: String,
    pub state: TaskState,
    pub revision: u64,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct JournalEvent {
    pub id: Uuid,
    pub task_id: Option<Uuid>,
    pub kind: String,
    pub payload: Value,
    pub created_at: DateTime<Utc>,
}

pub struct DurableStore {
    connection: Connection,
}

impl DurableStore {
    pub fn open(path: &Path) -> Result<Self, StorageError> {
        if let Some(parent) = path.parent() {
            if parent.exists() && std::fs::symlink_metadata(parent)?.file_type().is_symlink() {
                return Err(StorageError::InvalidData(
                    "storage directory must not be a symlink".into(),
                ));
            }
            std::fs::create_dir_all(parent)?;
        }
        if path
            .symlink_metadata()
            .is_ok_and(|metadata| metadata.file_type().is_symlink())
        {
            return Err(StorageError::InvalidData(
                "storage database must not be a symlink".into(),
            ));
        }
        // Normalize ancestor symlinks (notably /var -> /private/var on
        // macOS) before SQLite's NOFOLLOW open. The explicit parent check
        // above still rejects a symlink at the storage directory itself,
        // and NOFOLLOW still forbids a symlink at the database file.
        let parent = path
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
            .unwrap_or_else(|| Path::new("."));
        let canonical_parent = std::fs::canonicalize(parent)?;
        let filename = path
            .file_name()
            .ok_or_else(|| StorageError::InvalidData("storage filename is missing".into()))?;
        let database_path = canonical_parent.join(filename);
        let connection = Connection::open_with_flags(
            &database_path,
            OpenFlags::SQLITE_OPEN_READ_WRITE
                | OpenFlags::SQLITE_OPEN_CREATE
                | OpenFlags::SQLITE_OPEN_NOFOLLOW,
        )?;
        connection.execute_batch(
            "PRAGMA journal_mode = WAL;
             PRAGMA synchronous = FULL;
             CREATE TABLE IF NOT EXISTS tasks (
                 id TEXT PRIMARY KEY,
                 goal TEXT NOT NULL,
                 state TEXT NOT NULL,
                 revision INTEGER NOT NULL,
                 updated_ms INTEGER NOT NULL
             );
             CREATE TABLE IF NOT EXISTS events (
                 sequence INTEGER PRIMARY KEY AUTOINCREMENT,
                 event_id TEXT NOT NULL UNIQUE,
                 task_id TEXT,
                 kind TEXT NOT NULL,
                 payload TEXT NOT NULL,
                 created_at TEXT NOT NULL
             );
             CREATE INDEX IF NOT EXISTS events_by_task ON events(task_id, sequence);",
        )?;
        Ok(Self { connection })
    }

    pub fn create_task(&mut self, id: Uuid, goal: &str) -> Result<TaskRecord, StorageError> {
        if goal.trim().is_empty() || goal.len() > 16_384 {
            return Err(StorageError::InvalidData(
                "task goal must be 1..16384 bytes".into(),
            ));
        }
        let now = Utc::now();
        self.connection.execute(
            "INSERT INTO tasks(id, goal, state, revision, updated_ms)
             VALUES (?1, ?2, ?3, 0, ?4)",
            params![
                id.to_string(),
                goal,
                TaskState::Queued.as_str(),
                now.timestamp_millis()
            ],
        )?;
        Ok(TaskRecord {
            id,
            goal: goal.to_owned(),
            state: TaskState::Queued,
            revision: 0,
            updated_at: now,
        })
    }

    pub fn task(&self, id: Uuid) -> Result<Option<TaskRecord>, StorageError> {
        let row = self
            .connection
            .query_row(
                "SELECT goal, state, revision, updated_ms FROM tasks WHERE id = ?1",
                params![id.to_string()],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, i64>(2)?,
                        row.get::<_, i64>(3)?,
                    ))
                },
            )
            .optional()?;
        row.map(|(goal, raw_state, revision, ms)| {
            let state = TaskState::parse(&raw_state)?;
            let revision = u64::try_from(revision)
                .map_err(|error| StorageError::InvalidData(error.to_string()))?;
            let updated_at = DateTime::from_timestamp_millis(ms)
                .ok_or_else(|| StorageError::InvalidData("invalid task timestamp".into()))?;
            Ok(TaskRecord {
                id,
                goal,
                state,
                revision,
                updated_at,
            })
        })
        .transpose()
    }

    /// Compare-and-swap task transition. A stale version cannot override a
    /// newer approval/cancellation. Terminal states are never restarted here.
    pub fn transition(
        &mut self,
        id: Uuid,
        expected_revision: u64,
        next: TaskState,
    ) -> Result<TaskRecord, StorageError> {
        let current = self.task(id)?.ok_or(StorageError::TaskNotFound(id))?;
        if current.revision != expected_revision {
            return Err(StorageError::Conflict);
        }
        if !current.state.may_transition(next) {
            return Err(StorageError::InvalidTransition);
        }
        let now = Utc::now();
        let updated = self.connection.execute(
            "UPDATE tasks SET state = ?1, revision = revision + 1, updated_ms = ?2
             WHERE id = ?3 AND revision = ?4 AND state = ?5",
            params![
                next.as_str(),
                now.timestamp_millis(),
                id.to_string(),
                expected_revision as i64,
                current.state.as_str(),
            ],
        )?;
        if updated != 1 {
            return Err(StorageError::Conflict);
        }
        self.task(id)?.ok_or(StorageError::TaskNotFound(id))
    }

    /// Recoverable tasks require explicit operator reconfirmation; this method
    /// never executes their side effects.
    pub fn interrupted_tasks(&self) -> Result<Vec<TaskRecord>, StorageError> {
        let mut statement = self.connection.prepare(
            "SELECT id FROM tasks WHERE state IN ('running', 'awaiting_approval')
             ORDER BY updated_ms, id",
        )?;
        let rows = statement.query_map([], |row| row.get::<_, String>(0))?;
        let ids = rows.collect::<Result<Vec<_>, _>>()?;
        let mut tasks = Vec::new();
        for raw in ids {
            let id = Uuid::parse_str(&raw)
                .map_err(|error| StorageError::InvalidData(error.to_string()))?;
            if let Some(task) = self.task(id)? {
                tasks.push(task);
            }
        }
        Ok(tasks)
    }

    /// Append-only, idempotent event insert. A repeated ID with different data
    /// is a conflict instead of silently replacing the original event.
    pub fn append(&mut self, event: &JournalEvent) -> Result<bool, StorageError> {
        if event.kind.is_empty()
            || event.kind.len() > 128
            || !event
                .kind
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
        {
            return Err(StorageError::InvalidData("invalid event kind".into()));
        }
        let payload = serde_json::to_string(&event.payload)?;
        if payload.len() > MAX_EVENT_BYTES {
            return Err(StorageError::EventTooLarge);
        }
        let task_id = event.task_id.map(|value| value.to_string());
        let existing = self
            .connection
            .query_row(
                "SELECT task_id, kind, payload, created_at FROM events WHERE event_id = ?1",
                params![event.id.to_string()],
                |row| {
                    Ok((
                        row.get::<_, Option<String>>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, String>(3)?,
                    ))
                },
            )
            .optional()?;
        if let Some((prior_task, prior_kind, prior_payload, prior_created)) = existing {
            if prior_task == task_id
                && prior_kind == event.kind
                && prior_payload == payload
                && prior_created == event.created_at.to_rfc3339_opts(SecondsFormat::Nanos, true)
            {
                return Ok(false);
            }
            return Err(StorageError::Conflict);
        }
        self.connection.execute(
            "INSERT INTO events(event_id, task_id, kind, payload, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![
                event.id.to_string(),
                task_id,
                event.kind,
                payload,
                event.created_at.to_rfc3339_opts(SecondsFormat::Nanos, true),
            ],
        )?;
        Ok(true)
    }

    /// Replay in monotonic database insertion order, not wall-clock time.
    pub fn events_since(
        &self,
        after_sequence: i64,
        limit: usize,
    ) -> Result<Vec<(i64, JournalEvent)>, StorageError> {
        let mut statement = self.connection.prepare(
            "SELECT sequence, event_id, task_id, kind, payload, created_at
             FROM events WHERE sequence > ?1 ORDER BY sequence ASC LIMIT ?2",
        )?;
        let rows = statement.query_map(
            params![after_sequence, limit.clamp(1, 1000) as i64],
            |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, Option<String>>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, String>(4)?,
                    row.get::<_, String>(5)?,
                ))
            },
        )?;
        let mut events = Vec::new();
        for row in rows {
            let (sequence, id, task_id, kind, payload, created_at) = row?;
            events.push((
                sequence,
                JournalEvent {
                    id: Uuid::parse_str(&id)
                        .map_err(|error| StorageError::InvalidData(error.to_string()))?,
                    task_id: task_id
                        .map(|value| Uuid::parse_str(&value))
                        .transpose()
                        .map_err(|error| StorageError::InvalidData(error.to_string()))?,
                    kind,
                    payload: serde_json::from_str(&payload)?,
                    created_at: DateTime::parse_from_rfc3339(&created_at)
                        .map_err(|error| StorageError::InvalidData(error.to_string()))?
                        .with_timezone(&Utc),
                },
            ));
        }
        Ok(events)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reopens_tasks_and_replays_events_in_insertion_order() {
        let temp = tempfile::TempDir::new().unwrap();
        let db = temp.path().join("state.sqlite3");
        let id = Uuid::new_v4();
        let event = JournalEvent {
            id: Uuid::new_v4(),
            task_id: Some(id),
            kind: "task_started".into(),
            payload: serde_json::json!({"attempt": 1}),
            created_at: Utc::now(),
        };
        {
            let mut store = DurableStore::open(&db).unwrap();
            store.create_task(id, "Build a Rust service").unwrap();
            let task = store.transition(id, 0, TaskState::Running).unwrap();
            assert_eq!(task.revision, 1);
            assert!(store.append(&event).unwrap());
            assert!(!store.append(&event).unwrap());
        }
        let store = DurableStore::open(&db).unwrap();
        assert_eq!(store.interrupted_tasks().unwrap().len(), 1);
        assert_eq!(store.task(id).unwrap().unwrap().state, TaskState::Running);
        assert_eq!(store.events_since(0, 10).unwrap()[0].1, event);
        let first = store.events_since(0, 10).unwrap()[0].0;
        assert!(store.events_since(first, 10).unwrap().is_empty());
    }

    #[test]
    fn rejects_stale_revision_terminal_restarts_and_conflicting_event_ids() {
        let temp = tempfile::TempDir::new().unwrap();
        let mut store = DurableStore::open(&temp.path().join("state.sqlite3")).unwrap();
        let id = Uuid::new_v4();
        store.create_task(id, "Test recovery").unwrap();
        store.transition(id, 0, TaskState::Running).unwrap();
        assert!(matches!(
            store.transition(id, 0, TaskState::Completed),
            Err(StorageError::Conflict)
        ));
        store.transition(id, 1, TaskState::Completed).unwrap();
        assert!(matches!(
            store.transition(id, 2, TaskState::Running),
            Err(StorageError::InvalidTransition)
        ));
        let mut event = JournalEvent {
            id: Uuid::new_v4(),
            task_id: Some(id),
            kind: "task_completed".into(),
            payload: serde_json::json!({"success": true}),
            created_at: Utc::now(),
        };
        store.append(&event).unwrap();
        event.payload = serde_json::json!({"success": false});
        assert!(matches!(store.append(&event), Err(StorageError::Conflict)));
        assert!(store.interrupted_tasks().unwrap().is_empty());
    }

    #[cfg(unix)]
    #[test]
    fn resolves_symlinked_ancestors_but_rejects_symlinked_storage_directories() {
        let temp = tempfile::TempDir::new().unwrap();
        let actual = temp.path().join("actual");
        std::fs::create_dir_all(&actual).unwrap();
        let alias = temp.path().join("alias");
        std::os::unix::fs::symlink(&actual, &alias).unwrap();

        // The storage directory itself is real; only an ancestor is an
        // alias (as /var is on macOS). SQLite NOFOLLOW must still work.
        let db = alias.join("state").join("tasks.sqlite3");
        let store = DurableStore::open(&db).unwrap();
        drop(store);
        assert!(actual.join("state/tasks.sqlite3").is_file());

        // A symlink directly at the storage directory must remain refused.
        assert!(DurableStore::open(&alias.join("tasks.sqlite3")).is_err());
    }

    #[test]
    fn rejects_oversized_events_and_symlinked_database_files() {
        let temp = tempfile::TempDir::new().unwrap();
        let db = temp.path().join("state.sqlite3");
        let mut store = DurableStore::open(&db).unwrap();
        let event = JournalEvent {
            id: Uuid::new_v4(),
            task_id: None,
            kind: "tool_output".into(),
            payload: Value::String("x".repeat(MAX_EVENT_BYTES + 1)),
            created_at: Utc::now(),
        };
        assert!(matches!(
            store.append(&event),
            Err(StorageError::EventTooLarge)
        ));
        #[cfg(unix)]
        {
            let link = temp.path().join("linked.sqlite3");
            std::os::unix::fs::symlink(&db, &link).unwrap();
            assert!(DurableStore::open(&link).is_err());
        }
    }
}
