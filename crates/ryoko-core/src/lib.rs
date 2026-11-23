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
pub mod agents;
pub mod connectors;
pub mod context;
pub mod diagnostic;
pub mod encoding;
pub mod error;
pub mod event;
pub mod iacp;
pub mod index;
pub mod mcp;
pub mod memory;
pub mod model;
pub mod orchestrator;
pub mod project_memory;
pub mod review;
pub mod spec_link;
pub mod summary;
pub mod symbol_edit;
pub mod team;
pub mod validation;
pub mod vision;
pub mod workspace;

pub use agent::{AgentRuntime, TaskContext};
pub use agents::{
    CODEGEN_SYSTEM_PROMPT, CONTEXT_METADATA, ModelAgent, RETRY_FEEDBACK_METADATA,
    StructuredProposal, parse_structured_proposal, task_branch_key, task_summary_key,
};
pub use connectors::{
    ConnectorConfig, ConnectorError, ConnectorReadiness, ConnectorRegistry, ConnectorStatus,
    ConnectorTool, ConnectorTransport, builtin_preset, builtin_presets,
    readiness as connector_readiness, resolve_required_env,
};
pub use context::{
    CallEdge, ContextError, ContextExtractor, ContextSnippet, FileContext, Language, SourceFile,
    Symbol, extract_files,
};
pub use diagnostic::{DiagnosticLevel, TaskDiagnostic, TaskDiagnosticSink};
pub use error::{CoreError, CoreResult};
pub use event::{EventSink, OrchestratorEvent};
pub use iacp::{
    AuditEntry, IacpEnvelope, IacpError, IacpPayload, IacpPolicy, IacpRouter, IacpService,
    RoutedMessage,
};
pub use index::{CallerHit, IndexError, SymbolHit, WorkspaceIndex, goal_terms};
pub use mcp::{
    McpApprovalAsker, McpApprovalDecision, McpApprovalGate, McpApprovalScope, McpClient, McpError,
    McpTool, McpToolPolicy, McpToolService, McpToolbox, McpTransport, MultiMcpToolbox,
    NamespacedApprovalGate, SessionApprovalGate, namespaced_tool_name,
};
pub use memory::SharedMemory;
pub use model::{AgentIdentity, AgentRole, ProjectContext, ProposedChange, Task, TaskResult};
pub use orchestrator::{
    HumanReview, OrchestrationReport, Orchestrator, OrchestratorConfig, RepairCycle, SkippedTask,
    TaskFailure, TaskSuccess, ReviewOutcome,
};
pub use project_memory::{MemoryEntry, MemoryKind, ProjectMemory};
pub use review::{
    CODE_REVIEW_ROLE, INDEPENDENT_REVIEW_METADATA, REVIEW_TARGET_METADATA, ReviewConfig,
    ReviewFinding, ReviewRecord, ReviewSeverity, ReviewVerdict, SECURITY_REVIEW_ROLE,
    trusted_validation_evidence, validation_evidence_key,
};
pub use summary::{
    ChangeImpact, ChangeType, CodeReviewSummary, FileSummary, ImpactLevel, analyze_change_impact,
    generate_review_summary, render_markdown_summary,
};
pub use symbol_edit::{SymbolEdit, materialize_symbol_edits};
pub use team::{
    DynamicPlan, GoalSignals, VERIFICATION_METADATA, dynamic_plan, dynamic_plan_with_reviews,
};
pub use validation::{AllowAllValidator, ChangeValidator, StagedChanges, ValidationReport};
pub use workspace::{InMemoryWorkspaceProvider, PullRequest, WorkspaceLease, WorkspaceProvider};
