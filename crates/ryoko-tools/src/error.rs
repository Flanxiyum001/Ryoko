//! Error types for the tool execution engine.

use std::path::PathBuf;
use thiserror::Error;

/// Result type for tool operations.
pub type ToolResult<T> = Result<T, ToolError>;

/// Errors that can occur during tool execution.
#[derive(Debug, Error)]
pub enum ToolError {
    /// The tool was not found in the registry.
    #[error("tool not found: {0}")]
    ToolNotFound(String),

    /// The tool requires capabilities that are not granted.
    #[error("permission denied: tool {tool} requires {capability:?}")]
    PermissionDenied {
        tool: String,
        capability: crate::capabilities::Capability,
    },

    /// The tool requires explicit approval but none was given.
    #[error("approval required: {0}")]
    ApprovalRequired(String),

    /// The operator denied the approval request.
    #[error("approval denied: {0}")]
    ApprovalDenied(String),

    /// The tool call budget was exceeded.
    #[error("tool call budget exceeded: {0}")]
    BudgetExceeded(String),

    /// The tool timed out.
    #[error("tool timed out after {seconds}s: {tool}")]
    Timeout { tool: String, seconds: u64 },

    /// The tool was cancelled.
    #[error("tool cancelled: {0}")]
    Cancelled(String),

    /// The output size limit was exceeded.
    #[error("output size limit exceeded: {tool} produced {bytes} bytes (limit: {limit})")]
    OutputLimitExceeded { tool: String, bytes: usize, limit: usize },

    /// Invalid input arguments for the tool.
    #[error("invalid arguments for tool {tool}: {message}")]
    InvalidArguments { tool: String, message: String },

    /// Path traversal attempt detected.
    #[error("path traversal not allowed: {path}")]
    PathTraversal { path: PathBuf },

    /// Symlink escape attempt detected.
    #[error("symlink escape not allowed: {path}")]
    SymlinkEscape { path: PathBuf },

    /// Command injection attempt detected.
    #[error("command injection not allowed: {command}")]
    CommandInjection { command: String },

    /// Credential exposure attempt detected.
    #[error("credential exposure not allowed: {detail}")]
    CredentialExposure { detail: String },

    /// Prompt injection attempt detected in tool output.
    #[error("prompt injection detected in tool output")]
    PromptInjection,

    /// The workspace is not available for this tool.
    #[error("workspace not available: {0}")]
    WorkspaceUnavailable(String),

    /// The tool execution failed with an error.
    #[error("tool execution failed: {tool}: {message}")]
    ExecutionFailed { tool: String, message: String },

    /// The tool's preconditions were not met.
    #[error("precondition failed for tool {tool}: {message}")]
    PreconditionFailed { tool: String, message: String },

    /// Internal error in the tool engine.
    #[error("internal error: {0}")]
    Internal(String),

    /// I/O error during tool execution.
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    /// JSON serialization/deserialization error.
    #[error("JSON error: {0}")]
    Json(#[from] serde_json::Error),

    /// Worktree error.
    #[error("worktree error: {0}")]
    Worktree(#[from] ryoko_worktree::WorktreeError),

    /// Sandbox error.
    #[error("sandbox error: {0}")]
    Sandbox(#[from] ryoko_sandbox::BuildSandboxError),

    /// Core error.
    #[error("core error: {0}")]
    Core(#[from] ryoko_core::CoreError),
}

impl ToolError {
    /// Check if this error is a security violation.
    pub fn is_security_violation(&self) -> bool {
        matches!(
            self,
            ToolError::PathTraversal { .. }
                | ToolError::SymlinkEscape { .. }
                | ToolError::CommandInjection { .. }
                | ToolError::CredentialExposure { .. }
                | ToolError::PromptInjection
        )
    }

    /// Check if this error is recoverable (e.g., budget, timeout).
    pub fn is_recoverable(&self) -> bool {
        matches!(
            self,
            ToolError::BudgetExceeded(_)
                | ToolError::Timeout { .. }
                | ToolError::Cancelled(_)
                | ToolError::OutputLimitExceeded { .. }
        )
    }

    /// Check if this error indicates the tool was denied by policy.
    pub fn is_policy_denial(&self) -> bool {
        matches!(
            self,
            ToolError::PermissionDenied { .. }
                | ToolError::ApprovalRequired(_)
                | ToolError::ApprovalDenied(_)
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn security_violation_detection() {
        let path_traversal = ToolError::PathTraversal { path: "../etc/passwd".into() };
        assert!(path_traversal.is_security_violation());

        let symlink = ToolError::SymlinkEscape { path: "/tmp/link".into() };
        assert!(symlink.is_security_violation());

        let injection = ToolError::CommandInjection { command: "rm -rf /".into() };
        assert!(injection.is_security_violation());

        let budget = ToolError::BudgetExceeded("limit reached".into());
        assert!(!budget.is_security_violation());
    }

    #[test]
    fn recoverable_detection() {
        let budget = ToolError::BudgetExceeded("limit reached".into());
        assert!(budget.is_recoverable());

        let timeout = ToolError::Timeout { tool: "test".into(), seconds: 30 };
        assert!(timeout.is_recoverable());

        let cancelled = ToolError::Cancelled("user cancelled".into());
        assert!(cancelled.is_recoverable());

        let permission = ToolError::PermissionDenied {
            tool: "test".into(),
            capability: crate::capabilities::Capability::Execute,
        };
        assert!(!permission.is_recoverable());
    }

    #[test]
    fn policy_denial_detection() {
        let permission = ToolError::PermissionDenied {
            tool: "test".into(),
            capability: crate::capabilities::Capability::Execute,
        };
        assert!(permission.is_policy_denial());

        let approval_req = ToolError::ApprovalRequired("tool needs approval".into());
        assert!(approval_req.is_policy_denial());

        let approval_denied = ToolError::ApprovalDenied("operator said no".into());
        assert!(approval_denied.is_policy_denial());

        let budget = ToolError::BudgetExceeded("limit reached".into());
        assert!(!budget.is_policy_denial());
    }
}