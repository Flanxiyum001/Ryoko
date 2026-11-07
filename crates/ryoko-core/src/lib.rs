//! Project Ryoko orchestration core.
//!
//! Responsibilities:
//! - the [`AgentRuntime`] lifecycle contract (`identity`, `execute_task`, `rollback`);
//! - a dependency-aware, concurrent [`Orchestrator`] that isolates each task in a
//!   [`WorkspaceLease`] and validates its changes through [`ChangeValidator`]
//!   before any internal pull request is opened;
//! - a thread-safe [`SharedMemory`] blackboard shared by every agent;
//! - provider-neutral model access through [`ryoko_models::ModelRouter`].

pub mod agent;
pub mod error;
pub mod event;
pub mod iacp;
pub mod memory;
pub mod model;
pub mod orchestrator;
pub mod validation;
pub mod workspace;

pub use agent::{AgentRuntime, TaskContext};
pub use error::{CoreError, CoreResult};
pub use event::{EventSink, OrchestratorEvent};
pub use iacp::{
    AuditEntry, IacpEnvelope, IacpError, IacpPayload, IacpPolicy, IacpRouter, IacpService,
    RoutedMessage,
};
pub use memory::SharedMemory;
pub use model::{AgentIdentity, AgentRole, ProjectContext, ProposedChange, Task, TaskResult};
pub use orchestrator::{
    OrchestrationReport, Orchestrator, OrchestratorConfig, SkippedTask, TaskFailure, TaskSuccess,
};
pub use validation::{AllowAllValidator, ChangeValidator, StagedChanges, ValidationReport};
pub use workspace::{InMemoryWorkspaceProvider, PullRequest, WorkspaceLease, WorkspaceProvider};
