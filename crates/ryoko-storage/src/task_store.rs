//! Durable task persistence with execution state recovery.

use std::collections::BTreeMap;
use std::time::{SystemTime, UNIX_EPOCH};

use chrono::{DateTime, Utc};
use rusqlite::{params, Connection};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::{Repository, StorageError};

/// Current status of a task in the durable execution lifecycle.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskStatus {
    /// Task is being planned but not yet started.
    Planning,
    /// Task is ready to run or currently running.
    Running,
    /// Task is paused awaiting human approval.
    AwaitingApproval,
    /// Task is being validated (tests, checks, sandbox).
    Validating,
    /// Task failed validation and is being repaired.
    Repairing,
    /// Task execution is paused by operator.
    Paused,
    /// Task completed successfully.
    Completed,
    /// Task failed permanently.
    Failed,
    /// Task was cancelled by operator.
    Cancelled,
}

impl TaskStatus {
    pub fn is_terminal(&self) -> bool {
        matches!(self, Self::Completed | Self::Failed | Self::Cancelled)
    }

    pub fn can_resume(&self) -> bool {
        matches!(self, Self::AwaitingApproval | Self::Paused | Self::Repairing)
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Planning => "planning",
            Self::Running => "running",
            Self::AwaitingApproval => "awaiting_approval",
            Self::Validating => "validating",
            Self::Repairing => "repairing",
            Self::Paused => "paused",
            Self::Completed => "completed",
            Self::Failed => "failed",
            Self::Cancelled => "cancelled",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "planning" => Some(Self::Planning),
            "running" => Some(Self::Running),
            "awaiting_approval" => Some(Self::AwaitingApproval),
            "validating" => Some(Self::Validating),
            "repairing" => Some(Self::Repairing),
            "paused" => Some(Self::Paused),
            "completed" => Some(Self::Completed),
            "failed" => Some(Self::Failed),
            "cancelled" => Some(Self::Cancelled),
            _ => None,
        }
    }
}

/// Record of a task execution attempt.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AttemptRecord {
    pub id: Uuid,
    pub task_id: Uuid,
    pub attempt_number: u32,
    pub started_at: DateTime<Utc>,
    pub completed_at: Option<DateTime<Utc>>,
    pub status: AttemptStatus,
    pub execution_context: serde_json::Value,
    pub tool_outcomes: Vec<ToolOutcome>,
    pub checkpoint: Option<CheckpointSummary>,
}

/// Status of an individual attempt.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AttemptStatus {
    InProgress,
    Succeeded,
    Failed,
    Cancelled,
}

/// Summary of a checkpoint for an attempt.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CheckpointSummary {
    pub context_hash: String,
    pub tool_outcomes_hash: String,
    pub remaining_budget: serde_json::Value,
}

/// Outcome of a tool execution for recovery purposes.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolOutcome {
    pub tool: String,
    pub success: bool,
    pub output: serde_json::Value,
    pub error: Option<String>,
    pub metadata: serde_json::Value,
    pub timestamp: DateTime<Utc>,
    pub side_effects: Vec<SideEffect>,
}

/// Side effect of a tool execution that must not be blindly repeated.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SideEffect {
    pub kind: SideEffectKind,
    pub description: String,
    pub idempotency_key: Option<String>,
    pub reversible: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SideEffectKind {
    FileWrite,
    ProcessSpawn,
    NetworkRequest,
    GitOperation,
    ExternalApiCall,
    Unknown,
}

/// Durable task record with full execution state.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskRecord {
    pub id: Uuid,
    pub description: String,
    pub assigned_to: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub metadata: BTreeMap<String, String>,
    pub status: TaskStatus,
    pub dependencies: Vec<Uuid>,
    pub source_snapshot: Option<SourceSnapshot>,
    pub validated_changes: Option<ValidatedChanges>,
    pub remaining_budget: Option<BudgetSnapshot>,
    pub attempt_count: u32,
}

/// Snapshot of the source code at task start.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SourceSnapshot {
    pub base_commit: String,
    pub files: Vec<FileSnapshot>,
    pub tree_hash: String,
}

/// Snapshot of a single file.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileSnapshot {
    pub path: String,
    pub content_hash: String,
    pub size: usize,
}

/// Validated changes that passed validation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ValidatedChanges {
    pub changes: Vec<ProposedChange>,
    pub validation_report: ValidationSummary,
    pub commit_hash: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProposedChange {
    pub path: String,
    pub content: String,
    pub language: Option<String>,
    pub description: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ValidationSummary {
    pub passed: bool,
    pub summary: String,
    pub logs: Vec<String>,
    pub duration_ms: u64,
}

/// Budget snapshot for remaining resources.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BudgetSnapshot {
    pub tokens_remaining: Option<u64>,
    pub usd_remaining: Option<f64>,
    pub seconds_remaining: Option<u64>,
}

/// Execution state for recovery.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExecutionState {
    pub task_id: Uuid,
    pub current_attempt: Option<AttemptRecord>,
    pub completed_attempts: Vec<AttemptRecord>,
    pub context_snapshot: serde_json::Value,
    pub validated_changes: Option<ValidatedChanges>,
    pub approved_operations: Vec<ApprovedOperation>,
    pub paused_at: Option<DateTime<Utc>>,
}

/// Record of an approved operation for recovery.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApprovedOperation {
    pub tool: String,
    pub arguments: serde_json::Value,
    pub scope: String,
    pub approved_at: DateTime<Utc>,
    pub approved_by: String,
}

/// Store for durable task persistence.
pub struct TaskStore {
    repo: Arc<Repository>,
}

impl TaskStore {
    /// Creates a new task store.
    pub fn new(repo: Arc<Repository>) -> Self {
        Self { repo }
    }

    /// Creates a new task record.
    pub fn create_task(&self, mut task: TaskRecord) -> Result<TaskRecord, StorageError> {
        let now = Utc::now();
        task.created_at = now;
        task.updated_at = now;

        self.repo.with_transaction(|tx| {
            tx.execute(
                "INSERT INTO tasks (id, description, assigned_to, created_at, updated_at, metadata, status, dependencies, attempt_count)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
                params![
                    task.id.to_string(),
                    task.description,
                    task.assigned_to,
                    task.created_at.timestamp(),
                    task.updated_at.timestamp(),
                    serde_json::to_string(&task.metadata)?,
                    task.status.as_str(),
                    serde_json::to_string(&task.dependencies)?,
                    task.attempt_count,
                ],
            ).map_err(StorageError::Sqlite)?;

            if let Some(snapshot) = &task.source_snapshot {
                tx.execute(
                    "UPDATE tasks SET source_snapshot = ?1 WHERE id = ?2",
                    params![serde_json::to_string(snapshot)?, task.id.to_string()],
                ).map_err(StorageError::Sqlite)?;
            }

            if let Some(changes) = &task.validated_changes {
                tx.execute(
                    "UPDATE tasks SET validated_changes = ?1 WHERE id = ?2",
                    params![serde_json::to_string(changes)?, task.id.to_string()],
                ).map_err(StorageError::Sqlite)?;
            }

            if let Some(budget) = &task.remaining_budget {
                tx.execute(
                    "UPDATE tasks SET remaining_budget = ?1 WHERE id = ?2",
                    params![serde_json::to_string(budget)?, task.id.to_string()],
                ).map_err(StorageError::Sqlite)?;
            }

            Ok(())
        })?;

        Ok(task)
    }

    /// Gets a task by ID.
    pub fn get_task(&self, id: Uuid) -> Result<Option<TaskRecord>, StorageError> {
        self.repo.with_read(|conn| {
            let mut stmt = conn.prepare(
                "SELECT id, description, assigned_to, created_at, updated_at, metadata, status,
                        dependencies, source_snapshot, validated_changes, remaining_budget, attempt_count
                 FROM tasks WHERE id = ?1"
            )?;

            let task = stmt.query_row(params![id.to_string()], |row| {
                Ok(TaskRecord {
                    id: Uuid::parse_str(&row.get::<_, String>(0)?).unwrap(),
                    description: row.get(1)?,
                    assigned_to: row.get(2)?,
                    created_at: DateTime::from_timestamp(row.get::<_, i64>(3)?, 0).unwrap_or_default(),
                    updated_at: DateTime::from_timestamp(row.get::<_, i64>(4)?, 0).unwrap_or_default(),
                    metadata: serde_json::from_str(&row.get::<_, String>(5)?).unwrap_or_default(),
                    status: TaskStatus::parse(&row.get::<_, String>(6)?).unwrap_or(TaskStatus::Planning),
                    dependencies: serde_json::from_str(&row.get::<_, String>(7)?).unwrap_or_default(),
                    source_snapshot: row.get::<_, Option<String>>(8)?
                        .and_then(|s| serde_json::from_str(&s).ok()),
                    validated_changes: row.get::<_, Option<String>>(9)?
                        .and_then(|s| serde_json::from_str(&s).ok()),
                    remaining_budget: row.get::<_, Option<String>>(10)?
                        .and_then(|s| serde_json::from_str(&s).ok()),
                    attempt_count: row.get(11)?,
                })
            }).ok();

            Ok(task)
        })
    }

    /// Updates a task's status.
    pub fn update_task_status(&self, id: Uuid, status: TaskStatus) -> Result<(), StorageError> {
        let now = Utc::now();
        self.repo.with_write(|conn| {
            conn.execute(
                "UPDATE tasks SET status = ?1, updated_at = ?2 WHERE id = ?3",
                params![status.as_str(), now.timestamp(), id.to_string()],
            ).map_err(StorageError::Sqlite)?;
            Ok(())
        })
    }

    /// Updates task metadata and snapshots.
    pub fn update_task(&self, task: &TaskRecord) -> Result<(), StorageError> {
        let mut updated = task.clone();
        updated.updated_at = Utc::now();

        self.repo.with_write(|conn| {
            conn.execute(
                "UPDATE tasks SET
                    description = ?1, assigned_to = ?2, updated_at = ?3,
                    metadata = ?4, status = ?5, dependencies = ?6,
                    source_snapshot = ?7, validated_changes = ?8, remaining_budget = ?9, attempt_count = ?10
                 WHERE id = ?11",
                params![
                    updated.description,
                    updated.assigned_to,
                    updated.updated_at.timestamp(),
                    serde_json::to_string(&updated.metadata)?,
                    updated.status.as_str(),
                    serde_json::to_string(&updated.dependencies)?,
                    updated.source_snapshot.as_ref().map(serde_json::to_string).transpose()?,
                    updated.validated_changes.as_ref().map(serde_json::to_string).transpose()?,
                    updated.remaining_budget.as_ref().map(serde_json::to_string).transpose()?,
                    updated.attempt_count,
                    updated.id.to_string(),
                ],
            ).map_err(StorageError::Sqlite)?;
            Ok(())
        })
    }

    /// Records an attempt.
    pub fn record_attempt(&self, attempt: &AttemptRecord) -> Result<(), StorageError> {
        self.repo.with_write(|conn| {
            conn.execute(
                "INSERT INTO attempts (id, task_id, attempt_number, started_at, completed_at, status, execution_context, tool_outcomes, checkpoint)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
                params![
                    attempt.id.to_string(),
                    attempt.task_id.to_string(),
                    attempt.attempt_number,
                    attempt.started_at.timestamp(),
                    attempt.completed_at.map(|dt| dt.timestamp()),
                    serde_json::to_string(&attempt.status)?,
                    serde_json::to_string(&attempt.execution_context)?,
                    serde_json::to_string(&attempt.tool_outcomes)?,
                    attempt.checkpoint.as_ref().map(serde_json::to_string).transpose()?,
                ],
            ).map_err(StorageError::Sqlite)?;
            Ok(())
        })
    }

    /// Gets all attempts for a task.
    pub fn get_attempts(&self, task_id: Uuid) -> Result<Vec<AttemptRecord>, StorageError> {
        self.repo.with_read(|conn| {
            let mut stmt = conn.prepare(
                "SELECT id, task_id, attempt_number, started_at, completed_at, status, execution_context, tool_outcomes, checkpoint
                 FROM attempts WHERE task_id = ?1 ORDER BY attempt_number"
            )?;

            let rows = stmt.query_map(params![task_id.to_string()], |row| {
                Ok(AttemptRecord {
                    id: Uuid::parse_str(&row.get::<_, String>(0)?).unwrap(),
                    task_id: Uuid::parse_str(&row.get::<_, String>(1)?).unwrap(),
                    attempt_number: row.get(2)?,
                    started_at: DateTime::from_timestamp(row.get::<_, i64>(3)?, 0).unwrap_or_default(),
                    completed_at: row.get::<_, Option<i64>>(4)?
                        .map(|ts| DateTime::from_timestamp(ts, 0).unwrap_or_default()),
                    status: serde_json::from_str(&row.get::<_, String>(5)?).unwrap_or(AttemptStatus::InProgress),
                    execution_context: serde_json::from_str(&row.get::<_, String>(6)?).unwrap_or_default(),
                    tool_outcomes: serde_json::from_str(&row.get::<_, String>(7)?).unwrap_or_default(),
                    checkpoint: row.get::<_, Option<String>>(8)?
                        .and_then(|s| serde_json::from_str(&s).ok()),
                })
            })?;

            Ok(rows.filter_map(Result::ok).collect())
        })
    }

    /// Gets all active (non-terminal) tasks.
    pub fn get_active_tasks(&self) -> Result<Vec<TaskRecord>, StorageError> {
        self.repo.with_read(|conn| {
            let mut stmt = conn.prepare(
                "SELECT id, description, assigned_to, created_at, updated_at, metadata, status,
                        dependencies, source_snapshot, validated_changes, remaining_budget, attempt_count
                 FROM tasks WHERE status NOT IN ('completed', 'failed', 'cancelled')
                 ORDER BY updated_at DESC"
            )?;

            let rows = stmt.query_map([], |row| {
                Ok(TaskRecord {
                    id: Uuid::parse_str(&row.get::<_, String>(0)?).unwrap(),
                    description: row.get(1)?,
                    assigned_to: row.get(2)?,
                    created_at: DateTime::from_timestamp(row.get::<_, i64>(3)?, 0).unwrap_or_default(),
                    updated_at: DateTime::from_timestamp(row.get::<_, i64>(4)?, 0).unwrap_or_default(),
                    metadata: serde_json::from_str(&row.get::<_, String>(5)?).unwrap_or_default(),
                    status: TaskStatus::parse(&row.get::<_, String>(6)?).unwrap_or(TaskStatus::Planning),
                    dependencies: serde_json::from_str(&row.get::<_, String>(7)?).unwrap_or_default(),
                    source_snapshot: row.get::<_, Option<String>>(8)?
                        .and_then(|s| serde_json::from_str(&s).ok()),
                    validated_changes: row.get::<_, Option<String>>(9)?
                        .and_then(|s| serde_json::from_str(&s).ok()),
                    remaining_budget: row.get::<_, Option<String>>(10)?
                        .and_then(|s| serde_json::from_str(&s).ok()),
                    attempt_count: row.get(11)?,
                })
            })?;

            Ok(rows.filter_map(Result::ok).collect())
        })
    }

    /// Gets the execution state for recovery.
    pub fn get_execution_state(&self, task_id: Uuid) -> Result<Option<ExecutionState>, StorageError> {
        let task = self.get_task(task_id)?;
        if task.is_none() {
            return Ok(None);
        }

        let task = task.unwrap();
        let attempts = self.get_attempts(task_id)?;
        let current_attempt = attempts.last().cloned();
        let completed_attempts: Vec<_> = attempts.into_iter().take(task.attempt_count.saturating_sub(1) as usize).collect();

        Ok(Some(ExecutionState {
            task_id,
            current_attempt,
            completed_attempts,
            context_snapshot: serde_json::Value::Null,
            validated_changes: task.validated_changes,
            approved_operations: Vec::new(),
            paused_at: if task.status == TaskStatus::Paused { Some(Utc::now()) } else { None },
        }))
    }

    /// Deletes a task and its attempts (for cleanup).
    pub fn delete_task(&self, id: Uuid) -> Result<(), StorageError> {
        self.repo.with_transaction(|tx| {
            tx.execute("DELETE FROM attempts WHERE task_id = ?1", params![id.to_string()])
                .map_err(StorageError::Sqlite)?;
            tx.execute("DELETE FROM tasks WHERE id = ?1", params![id.to_string()])
                .map_err(StorageError::Sqlite)?;
            Ok(())
        })
    }
}