//! Editor-agnostic reporting of task findings.
//!
//! The orchestrator must be able to tell a connected editor what happened to a
//! task without knowing which editor or protocol is on the other side. Hosts
//! implement [`TaskDiagnosticSink`] over their own transport; `ryoko-native`
//! implements it over the LSP diagnostic channel.

/// Severity of a task finding, independent of any editor protocol.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiagnosticLevel {
    Info,
    Warning,
    Error,
}

/// One document-level finding produced by a task.
///
/// `file` is relative to the project root recorded in the task context; the
/// sink is responsible for resolving it into whatever addressing its transport
/// requires.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TaskDiagnostic {
    pub file: String,
    pub level: DiagnosticLevel,
    pub message: String,
    pub code: Option<String>,
}

/// Receives the findings of a task that finished validation.
///
/// Implementations must be cheap, concurrency-safe and best-effort: a task has
/// already succeeded by the time this is called, so a failing or disconnected
/// editor must not propagate an error. An empty `project_root` means the run has
/// no editor-addressable location and no findings should be published.
pub trait TaskDiagnosticSink: Send + Sync {
    fn publish_task_findings(&self, project_root: &str, findings: &[TaskDiagnostic]);
}
