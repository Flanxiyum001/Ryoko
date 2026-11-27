//! Ryoko's native agent harness.
//!
//! A personalized agent harness for Ryoko, incorporating DeepSeek Harness's
//! ("dsh") "everything is a plugin" composability, operating profiles (`code`,
//! `standard`, `headless`, `minimal`), durable execution traces (`SessionJournal`),
//! dynamic system prompt composition, and anti-thrashing loop guards.
//!
//! The harness follows the workspace rules in `AGENTS.md`:
//!
//! * Model output is untrusted. Tool names, arguments and paths are validated
//!   and every result is size-bounded before it re-enters the context.
//! * File edits are **staged**, never written. [`HarnessOutcome::changes`]
//!   holds them for diff review; the host decides whether to apply them.
//! * Shell commands run only after an [`ApprovalGate`] approves each one. The
//!   default gate ([`DenyAllCommands`]) refuses everything.
//! * Agents cannot grant themselves permissions: tools, command approval and
//!   delegation depth are set by the host through [`HarnessConfig`].

pub mod context;
mod harness;
pub mod loop_guard;
pub mod memory;
pub mod plugin;
pub mod profile;
pub mod prompt;
pub mod review;
pub mod session;
pub mod skills;
pub mod swarm;
mod tools;

pub use context::{ContextHit, FastContext};
pub use harness::{
    Harness, HarnessConfig, HarnessError, HarnessEvent, HarnessOutcome, PlanItem, PlanStatus,
    StopCause,
};
pub use loop_guard::LoopGuard;
pub use memory::{MemoryError, ProjectMemory};
pub use plugin::{
    HarnessPlugin, PluginContext, PluginRegistry, SessionMeta, StepDecision, ToolDecision,
};
pub use profile::{HarnessProfile, ProfileKind};
pub use prompt::{PromptSection, SystemPromptComposer};
pub use review::{ReviewFinding, ReviewReport, Severity, Verdict};
pub use session::{SessionEvent, SessionJournal};
pub use skills::Skill;
pub use swarm::{
    GitSwarmWorkspace, Swarm, SwarmConfig, SwarmEvent, SwarmReport, SwarmTaskResult,
    SwarmTaskStatus,
};
pub use tools::{ApprovalGate, CommandRequest, DenyAllCommands, ExternalTool, ProposedChange};
