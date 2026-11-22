//! Secure agent tool execution engine for Project Ryoko.
//!
//! This crate provides implementations of the tool execution engine traits
//! defined in `ryoko-core`. It enforces capability-based permissions,
//! worktree isolation, output limits, timeouts, cancellation, tool-call
//! budgets, and structured error reporting.
//!
//! Security guarantees:
//! - Every mutating operation is restricted to an authorised isolated worktree
//! - Explicit operator approval required for sensitive/elevated operations
//! - Protection against path traversal, symlink escapes, command injection,
//!   prompt injection, and accidental credential exposure
//! - The tool engine can never silently modify the user's original checkout
//!   or merge changes

pub mod approval;
pub mod capabilities;
pub mod error;
pub mod execution;
pub mod fs;
pub mod git;
pub mod limits;
pub mod mcp;
pub mod search;
pub mod shell;

// Re-export core traits and types
pub use ryoko_core::tools::{
    Tool, ToolRegistry, ToolExecutionResult, ToolContext, ToolExecutor, ToolOutput,
    FileOutput, FileOperation, ToolLimits, ToolBudget, OutputLimits, McpToolConfig,
    Capability, CapabilitySet, NetworkScope, PermissionScope, ReadScope, WriteScope,
    ApprovalGate, ApprovalScope, ToolApproval, SessionApprovalGate, TerminalApprovalAsker,
};

// Re-export implementations
pub use approval::*;
pub use capabilities::*;
pub use error::*;
pub use execution::*;
pub use fs::{FileTool, FileToolInput, FileToolOutput, register_file_tools};
pub use git::{GitTool, GitToolInput, GitToolOutput, register_git_tools};
pub use limits::*;
pub use mcp::{McpToolAdapter, register_mcp_tools};
pub use search::{SearchTool, SearchToolInput, SearchToolOutput, register_search_tools};
pub use shell::{ShellTool, ShellToolInput, ShellToolOutput, register_shell_tools};

use ryoko_core::model::Task;
use ryoko_worktree::WorkspaceLease;
use std::sync::Arc;

/// Create the default tool registry with all built-in tools.
pub fn default_tool_registry(
    workspace: Option<Arc<dyn WorkspaceLease>>,
    approval_gate: Option<Arc<dyn ApprovalGate>>,
    sandbox: Option<ryoko_sandbox::BuildSandbox>,
    mcp_services: Vec<(String, Arc<dyn ryoko_core::mcp::McpToolService>)>,
) -> ToolRegistry {
    let mut registry = ToolRegistry::new();

    // File system tools
    registry.register(Box::new(FileTool::new(workspace.clone())));

    // Search tools
    registry.register(Box::new(SearchTool::new(workspace.clone())));

    // Git tools
    registry.register(Box::new(GitTool::new(workspace.clone())));

    // Shell execution tools (require approval for mutating operations)
    registry.register(Box::new(ShellTool::new(workspace.clone(), approval_gate.clone(), sandbox)));

    // Register MCP tools
    let mcp_configs: Vec<McpToolConfig> = mcp_services
        .into_iter()
        .map(|(namespace, service)| McpToolConfig::new(service, namespace))
        .collect();
    register_mcp_tools(&mut registry, mcp_configs);

    registry
}

/// Create a tool registry for read-only operations (speculative tasks).
pub fn read_only_tool_registry(
    workspace: Option<Arc<dyn WorkspaceLease>>,
) -> ToolRegistry {
    let mut registry = ToolRegistry::new();
    register_file_tools(&mut registry, workspace.clone());
    register_search_tools(&mut registry, workspace.clone());
    register_git_tools(&mut registry, workspace, None, None);
    // No shell or MCP tools for read-only
    registry
}

/// Create a tool registry for code generation tasks.
pub fn code_generation_tool_registry(
    workspace: Option<Arc<dyn WorkspaceLease>>,
    approval_gate: Option<Arc<dyn ApprovalGate>>,
    sandbox: Option<ryoko_sandbox::BuildSandbox>,
    mcp_services: Vec<(String, Arc<dyn ryoko_core::mcp::McpToolService>)>,
) -> ToolRegistry {
    default_tool_registry(workspace, approval_gate, sandbox, mcp_services)
}

/// Create a tool registry for test execution tasks.
pub fn test_execution_tool_registry(
    workspace: Option<Arc<dyn WorkspaceLease>>,
    approval_gate: Option<Arc<dyn ApprovalGate>>,
    sandbox: ryoko_sandbox::BuildSandbox,
) -> ToolRegistry {
    let mut registry = ToolRegistry::new();
    register_file_tools(&mut registry, workspace.clone());
    register_search_tools(&mut registry, workspace.clone());
    register_git_tools(&mut registry, workspace, None, None);
    register_shell_tools(&mut registry, None, approval_gate, Some(sandbox));
    registry
}

#[cfg(test)]
mod tests {
    use super::*;
    use ryoko_core::model::{AgentRole, Task};
    use ryoko_models::ModelRouter;
    use ryoko_core::workspace::InMemoryWorkspaceProvider;

    #[tokio::test]
    async fn default_registry_has_tools() {
        let workspace = InMemoryWorkspaceProvider::new("main");
        let lease = workspace.allocate(&Task::new("test", AgentRole::SeniorDeveloper), &Default::default()).await.unwrap();

        let registry = default_tool_registry(Some(lease), None, None, Vec::new());
        let tools = registry.list();

        let tool_names: Vec<&str> = tools.iter().map(|t| t.name()).collect();
        assert!(tool_names.contains(&"fs"));
        assert!(tool_names.contains(&"search"));
        assert!(tool_names.contains(&"git"));
        assert!(tool_names.contains(&"shell"));
    }

    #[tokio::test]
    async fn read_only_registry_no_mutating() {
        let workspace = InMemoryWorkspaceProvider::new("main");
        let lease = workspace.allocate(&Task::new("test", AgentRole::SeniorDeveloper), &Default::default()).await.unwrap();

        let registry = read_only_tool_registry(Some(lease));
        let tools = registry.list();

        let tool_names: Vec<&str> = tools.iter().map(|t| t.name()).collect();
        assert!(tool_names.contains(&"fs"));
        assert!(tool_names.contains(&"search"));
        assert!(tool_names.contains(&"git"));
        assert!(!tool_names.contains(&"shell"));
    }
}