use std::sync::Arc;

use async_trait::async_trait;

use crate::error::CoreResult;
use crate::event::{EventSink, OrchestratorEvent};
use crate::memory::SharedMemory;
use crate::model::{AgentIdentity, ProjectContext, Task, TaskResult};
use crate::validation::ChangeValidator;
use crate::workspace::WorkspaceLease;
use ryoko_models::ModelRouter;

/// Everything an agent needs for one task. Cloned per execution so concurrent
/// tasks never share mutable context.
#[derive(Clone)]
pub struct TaskContext {
    pub project: ProjectContext,
    pub memory: SharedMemory,
    pub models: Arc<ModelRouter>,
    pub validator: Arc<dyn ChangeValidator>,
    /// Present when the task runs inside an isolated workspace.
    pub workspace: Option<Arc<dyn WorkspaceLease>>,
    pub events: EventSink,
}

impl TaskContext {
    pub fn with_workspace(mut self, workspace: Option<Arc<dyn WorkspaceLease>>) -> Self {
        self.workspace = workspace;
        self
    }

    pub fn emit_progress(&self, task_id: uuid::Uuid, message: impl Into<String>) {
        self.events.emit(OrchestratorEvent::TaskProgress {
            task_id,
            message: message.into(),
        });
    }

    pub fn shared_memory_snapshot(&self) -> std::collections::HashMap<String, String> {
        self.memory.snapshot()
    }
}

/// Lifecycle contract every agent (built-in, scripted or WASM-hosted) implements.
///
/// Implementations must be safe to call concurrently: the orchestrator may run
/// several tasks for the same role at once, each in its own workspace lease.
#[async_trait]
pub trait AgentRuntime: Send + Sync {
    /// Stable identity, including role, display name and capabilities.
    fn identity(&self) -> AgentIdentity;

    /// Called once when the agent joins the orchestrator.
    async fn on_register(&self, _memory: SharedMemory) -> CoreResult<()> {
        Ok(())
    }

    /// Executes one task. All file writes must be returned as
    /// [`TaskResult::changes`](crate::TaskResult); the orchestrator applies them
    /// to the task's workspace only after validation passes.
    async fn execute_task(&self, ctx: TaskContext, task: Task) -> CoreResult<TaskResult>;

    /// Reverts any side effects of a failed or rejected task. The orchestrator
    /// calls this before discarding the workspace lease.
    async fn rollback(&self, _ctx: TaskContext, _task: &Task, _reason: &str) -> CoreResult<()> {
        Ok(())
    }

    /// Called when the orchestrator is shutting down.
    async fn on_shutdown(&self) -> CoreResult<()> {
        Ok(())
    }
}
