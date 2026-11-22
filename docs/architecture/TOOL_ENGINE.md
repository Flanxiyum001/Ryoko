# Ryoko Tool Engine

## Overview

The `ryoko-tools` crate provides a unified, typed Tool Execution API for Ryoko agents with:

- **Capability-based permissions** with read, write, execute, and network scopes
- **Worktree isolation** - all mutating operations restricted to authorised isolated worktrees
- **Explicit operator approval** for sensitive or elevated operations
- **Resource limits** including output-size limits, timeouts, cancellation, and tool-call budgets
- **Security hardening** against path traversal, symlink escapes, command injection, prompt injection, and credential exposure
- **MCP integration** using existing transports without duplication

## Architecture

### Core Types

```rust
ToolRegistry          // Catalog of available tools with typed definitions
ToolExecutor          // Policy-enforcing execution engine
ExecutionContext      // Per-call context (workspace, security, limits, approval)
ExecutionResult       // Structured tool results
ExecutionPolicy       // Combined policy configuration
SecurityPolicy        // Security limits and checks
ResourceLimits        // Budgets, timeouts, concurrency controls
```

### Capabilities

The four fundamental capability scopes:

| Capability | Description |
|------------|-------------|
| `Read` | File discovery, reading, search, git status, diagnostics |
| `Write` | Patch-based editing, file creation/deletion |
| `Execute` | Sandboxed build/test/lint commands |
| `Network` | MCP connector tool calls |

### Permission Scopes

Tools are granted capabilities through the `ToolCapability` mapping:

```rust
ToolCapability {
    tool_id: "fs.patch",
    required: CapabilitySet { Read, Write },
    scope: PermissionScope::Category,
}
```

## Built-in Tools

### Filesystem Tools (Read Capability)

| Tool | Description |
|------|-------------|
| `fs.discover` | List files matching a glob pattern |
| `fs.read` | Read file contents with optional offset/limit |
| `fs.glob` | Find files matching a glob pattern |

### Code Analysis Tools (Read Capability)

| Tool | Description |
|------|-------------|
| `code.search` | Search for code patterns across the worktree |
| `code.symbols` | Find symbol definitions by name |
| `code.callers` | Find callers of a function |

### File Editing Tools (Write Capability)

| Tool | Description |
|------|-------------|
| `fs.patch` | Apply a unified diff patch |
| `fs.create` | Create a new file |
| `fs.delete` | Delete a file |

### Git Tools (Read Capability)

| Tool | Description |
|------|-------------|
| `git.status` | Show working tree status |
| `git.diff` | Show changes between commits/refs |
| `git.history` | Show commit history |

### Sandbox Tools (Execute Capability)

| Tool | Description |
|------|-------------|
| `sandbox.exec` | Execute a command in the isolated sandbox |
| `sandbox.build` | Run the project build command |
| `sandbox.test` | Run the project test suite |
| `sandbox.lint` | Run the project linter |

### Diagnostics Tools (Read Capability)

| Tool | Description |
|------|-------------|
| `diag.read` | Read diagnostic information |
| `diag.logs` | Read execution logs |

### MCP Tools (Network Capability)

| Tool | Description |
|------|-------------|
| `mcp.<connector>__<tool>` | Namespaced MCP tool calls |

## Security Model

### Path Traversal Protection

All paths are validated by `SecurityPolicy::validate_path()`:

```rust
// Rejects:
- "../etc/passwd"
- "/etc/passwd" (absolute paths not allowed)
- ".git/config" (forbidden prefixes)
- symlinks that escape the worktree
```

### Command Injection Protection

Commands are validated by `SecurityPolicy::validate_command()`:

```rust
// Blocked commands include:
rm, dd, mkfs, sudo, chmod, chown, mount, 
curl, wget, ssh, scp, rsync, docker, kubectl,
terraform, ansible, etc.
```

### Credential Exposure Protection

Arguments are scanned for credential patterns:

```rust
let patterns = [
    r"(?i)(api[_-]?key|secret[_-]?key).*",
    r"sk-[A-Za-z0-9]{48,}",  // OpenAI keys
    r"xox[baprs]-[A-Za-z0-9-]{10,}",  // Slack tokens
    "BEGIN.*PRIVATE KEY",
];
```

### Prompt Injection Protection

Tool arguments are checked for prompt injection patterns:

```rust
let injection_patterns = [
    "ignore previous instructions",
    "disregard previous",
    "new instructions:",
    "jailbreak",
    "DAN",
];
```

## Approval Flow

1. Tool is invoked with arguments
2. Tool definition is looked up in registry
3. Required capabilities are checked against granted set
4. Approval policy is determined (never, always, first_call, on_mutating, by_capability)
5. Approval gate is consulted (may prompt operator)
6. Security validations run (path, command, prompt, credentials)
7. Resource budget/limits are checked
8. Tool executes with timeout
9. Result size is validated against limits
10. Execution is recorded in audit log

## Resource Limits

### Configuration

```rust
ResourceLimits {
    output_limits: OutputLimits {
        max_output_bytes: 1_048_576,      // 1 MiB
        max_result_bytes: 4_194_304,     // 4 MiB
        max_total_output_bytes: 104_857_600, // 100 MiB
        truncate_instead_of_fail: false,
    },
    timeout_policy: TimeoutPolicy {
        tool_timeout: Duration::from_secs(300),
        max_timeout: Duration::from_secs(3600),
        sandbox_timeout: Duration::from_secs(600),
        mcp_timeout: Duration::from_secs(60),
        git_timeout: Duration::from_secs(60),
        file_timeout: Duration::from_secs(30),
    },
    max_total_calls: 1000,
    max_calls_per_tool: Some(20),
    max_calls_per_category: Some(50),
    rate_limit_max: Some(30),
    rate_limit_window: Duration::from_secs(60),
    max_concurrent: 4,
    max_memory_bytes: 512 * 1024 * 1024,
}
```

### Strict Mode

For untrusted code, use `ResourceLimits::strict()`:
- Total calls: 100
- Concurrent executions: 2
- Per-tool limit: 20 calls
- Rate limit: 30 calls per minute
- Output limits much smaller

## Integration with MCP

The `mcp_bridge` module bridges to existing MCP connectors:

```rust
// Configure MCP connection
let config = McpConnectionConfig {
    connector_id: "github",
    transport_type: McpTransportType::Sse,
    transport_config: json!({"url": "https://example.com/sse"}),
    allowed_tools: vec!["list_issues".to_string()],
    max_calls_per_session: 50,
    enabled: true,
};

// Build multi-toolbox
let toolbox = build_multi_mcp_toolbox(
    vec![config],
    approval_gate
).await?;

// Bridge to registry
let bridge = McpToolboxBridge::new(toolbox, approval_gate);
bridge.register_tools(&registry)?;
```

Tools are namespaced: `github__list_issues`, `cloudflare__kv_get`.

## Testing

The crate includes comprehensive tests:

```bash
cargo test -p ryoko-tools
```

Tests cover:
- Capability enforcement
- Approval gating
- Security validations (path traversal, injection, credentials)
- Resource limits
- Tool registration and execution

## Usage Example

```rust
use ryoko_tools::{
    ToolExecutor, ToolRegistry, ExecutionContext,
    ApprovalGate, SecurityPolicy, ResourceLimits,
    register_builtin_tools,
};

// Create registry with built-in tools
let registry = Arc::new(ToolRegistry::with_builtins());

// Set up execution engine
let executor = ToolExecutor::new(
    registry,
    ResourceLimits::default(),
    SecurityPolicy::default(),
    Arc::new(ApprovalGate::new(None)), // No approval in this example
    None, // No worktree manager
);

// Register tool implementations
register_builtin_tools(&executor)?;

// Execute a tool
let result = executor.execute(
    "fs.read",
    json!({"path": "src/main.rs", "limit": 100}),
    &context,
).await?;

if result.success {
    println!("Content: {:?}", result.result);
} else {
    println!("Error: {:?}", result.error);
}
```

## Future Work

- Integration with the model adapters for tool-calling
- Integration with the orchestrator for task-scoped capabilities
- Better handling of large file reading (streaming)
- Incremental symbol index integration for code search tools
- Web-search tools with network capability
- Additional sandbox tools for different ecosystems