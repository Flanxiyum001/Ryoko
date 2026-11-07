use serde::Serialize;
use tokio::sync::broadcast;
use uuid::Uuid;

use crate::model::AgentRole;

/// Lifecycle events emitted by the orchestrator. The Tauri shell forwards these
/// to the React UI; tests subscribe directly.
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum OrchestratorEvent {
    RunStarted {
        run_id: Uuid,
        task_count: usize,
    },
    TaskScheduled {
        task_id: Uuid,
        role: AgentRole,
        description: String,
    },
    TaskStarted {
        task_id: Uuid,
        role: AgentRole,
        agent: String,
        attempt: u32,
    },
    TaskProgress {
        task_id: Uuid,
        message: String,
    },
    ValidationStarted {
        task_id: Uuid,
        file_count: usize,
    },
    ValidationFinished {
        task_id: Uuid,
        passed: bool,
        summary: String,
        logs: Vec<String>,
        duration_ms: u64,
    },
    PullRequestOpened {
        task_id: Uuid,
        pr_id: String,
        branch: String,
        files: Vec<String>,
        summary: String,
    },
    TaskSucceeded {
        task_id: Uuid,
        role: AgentRole,
        output: String,
        attempts: u32,
    },
    TaskFailed {
        task_id: Uuid,
        role: AgentRole,
        error: String,
        attempts: u32,
        retrying: bool,
    },
    TaskSkipped {
        task_id: Uuid,
        role: AgentRole,
        reason: String,
    },
    RunFinished {
        run_id: Uuid,
        succeeded: usize,
        failed: usize,
        skipped: usize,
        duration_ms: u64,
    },
}

/// Cloneable handle to a bounded broadcast channel. Events are best-effort:
/// slow or absent subscribers never block orchestration.
#[derive(Debug, Clone)]
pub struct EventSink {
    tx: broadcast::Sender<OrchestratorEvent>,
}

impl Default for EventSink {
    fn default() -> Self {
        Self::new(1_024)
    }
}

impl EventSink {
    pub fn new(capacity: usize) -> Self {
        let (tx, _) = broadcast::channel(capacity.max(16));
        Self { tx }
    }

    pub fn emit(&self, event: OrchestratorEvent) {
        let _ = self.tx.send(event);
    }

    pub fn subscribe(&self) -> broadcast::Receiver<OrchestratorEvent> {
        self.tx.subscribe()
    }

    pub fn receiver_count(&self) -> usize {
        self.tx.receiver_count()
    }
}
