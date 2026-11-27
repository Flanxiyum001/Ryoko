//! Ryoko's native agent harness.
//!
//! A harness turns one model into a working agent: it runs a bounded
//! think → call tools → observe loop, keeps a plan, remembers project facts
//! between sessions, loads reusable skills on demand and can hand a focused
//! sub-task to a fresh sub-agent.
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
pub mod memory;
pub mod review;
pub mod skills;
pub mod swarm;
mod tools;

pub use context::{ContextHit, FastContext};
pub use harness::{
    Harness, HarnessConfig, HarnessError, HarnessEvent, HarnessOutcome, PlanItem, PlanStatus,
    StopCause,
};
pub use memory::{MemoryError, ProjectMemory};
pub use review::{ReviewFinding, ReviewReport, Severity, Verdict};
pub use skills::Skill;
pub use swarm::{
    GitSwarmWorkspace, Swarm, SwarmConfig, SwarmEvent, SwarmReport, SwarmTaskResult,
    SwarmTaskStatus,
};
pub use tools::{ApprovalGate, CommandRequest, DenyAllCommands, ExternalTool, ProposedChange};
