//! Secure agent tool engine for Project Ryoko.
//!
//! This crate provides a unified, typed Tool Execution API with capability-based
//! permissions, worktree isolation, and integration with existing MCP connectors.
//!
//! # Architecture
//!
//! - **Tool Registry**: Typed tool definitions with input/output schemas
//! - **Capability System**: Read, Write, Execute, Network scopes with explicit grants
//! - **Worktree Isolation**: All mutating operations restricted to authorised isolated worktrees
//! - **Approval Gates**: Explicit operator approval for sensitive operations
//! - **Resource Limits**: Output-size limits, timeouts, cancellation, tool-call budgets
//! - **Security Hardening**: Path traversal, symlink escapes, command injection, prompt injection, credential exposure protection
//! - **MCP Integration**: Bridges to existing MCP connectors without duplicating transport

pub mod capabilities;
pub mod approval;
pub mod registry;
pub mod execution;
pub mod tools;
pub mod mcp_bridge;
pub mod security;
pub mod limits;
pub mod error;

pub use capabilities::{Capability, CapabilitySet, PermissionScope, ToolCapability};
pub use approval::{ApprovalGate, ApprovalDecision, ApprovalScope, ApprovalPolicy};
pub use registry::{ToolRegistry, ToolDefinition, ToolMetadata, ToolCategory};
pub use execution::{ToolExecutor, ExecutionContext, ExecutionResult, ExecutionPolicy};
pub use tools::{
    FileDiscoveryTool, FileReadTool, SymbolSearchTool, PatchEditTool,
    FileCreateTool, FileDeleteTool, GitStatusTool, GitDiffTool, GitHistoryTool,
    SandboxExecTool, DiagnosticsTool, ExecutionLogsTool,
};
pub use mcp_bridge::{McpToolAdapter, McpToolboxBridge};
pub use security::SecurityPolicy;
pub use limits::{ResourceLimits, ToolBudget, OutputLimits, TimeoutPolicy};
pub use error::{ToolError, ToolResult};