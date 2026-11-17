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
        /// IDs of tasks that must finish before this one is ready.
        #[serde(default)]
        dependencies: Vec<Uuid>,
    },
    TaskStarted {
        task_id: Uuid,
        role: AgentRole,
        agent: String,
        attempt: u32,
    },
    /// Coarse live phase for the Mission Control status badge. This is
    /// advisory UI state; task success still depends on validation.
    TaskPhase {
        task_id: Uuid,
        phase: AgentPhase,
    },
    TaskProgress {
        task_id: Uuid,
        message: String,
    },
    /// Cumulative usage for this task. Unconfigured model pricing can make
    /// ledger_cost_usd incomplete: never label it verified billed cost.
    TaskUsage {
        task_id: Uuid,
        requests: u64,
        input_tokens: u64,
        output_tokens: u64,
        ledger_cost_usd: f64,
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
        stage: TaskFailureStage,
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

/// Coarse phase reported while an agent is running. These labels are
/// informational and never imply that validation passed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentPhase {
    Thinking,
    Coding,
    Testing,
    Waiting,
}

/// Where a failed attempt stopped, so the operator gets a specific explanation
/// instead of only a raw error string.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskFailureStage {
    Workspace,
    Agent,
    Staging,
    Validation,
    Publish,
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn phase_and_failure_stage_are_machine_readable() {
        let phase = serde_json::to_value(OrchestratorEvent::TaskPhase {
            task_id: Uuid::nil(),
            phase: AgentPhase::Coding,
        })
        .unwrap();
        assert_eq!(phase["type"], "task_phase");
        assert_eq!(phase["phase"], "coding");
        assert_eq!(
            serde_json::to_string(&TaskFailureStage::Validation).unwrap(),
            "\"validation\""
        );
    }
}
