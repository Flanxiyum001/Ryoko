use uuid::Uuid;

use crate::model::AgentRole;

pub type CoreResult<T> = Result<T, CoreError>;

#[derive(Debug, thiserror::Error)]
pub enum CoreError {
    #[error("no agent is registered for role `{role}`")]
    MissingAgent { role: AgentRole },

    #[error("an agent named `{agent}` is already registered")]
    DuplicateAgent { agent: String },

    #[error("task {0} appears more than once in the plan")]
    DuplicateTask(Uuid),

    #[error("task {task} depends on unknown task {dependency}")]
    UnknownDependency { task: Uuid, dependency: Uuid },

    #[error("task dependency cycle detected involving {tasks:?}")]
    DependencyCycle { tasks: Vec<Uuid> },

    #[error("task {task} failed: {message}")]
    TaskFailure { task: Uuid, message: String },

    #[error("validation rejected changes for task {task}: {reason}")]
    ValidationRejected { task: Uuid, reason: String },

    #[error("workspace isolation error: {0}")]
    Workspace(String),

    #[error("sandbox error: {0}")]
    Sandbox(String),

    #[error("internal orchestration error: {0}")]
    Internal(String),
}
