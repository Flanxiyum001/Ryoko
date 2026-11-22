# PR: Secure Agent Tool Engine (Part 2)

## Summary

This PR implements a unified, typed Tool Execution API in Rust for Ryoko's agent system with strict security controls, capability-based permissions, and integration with existing MCP connectors.

## Implementation

### New Crate: `ryoko-tools`

A new crate is added under `crates/ryoko-tools/` with the following modules:

- **`capabilities.rs`** - Capability-based permission system (Read, Write, Execute, Network scopes)
- **`approval.rs`** - Approval gates for sensitive operations with scopes (Once, ForSession, Deny)
- **`registry.rs`** - Tool registry with typed definitions and category-based organization
- **`execution.rs`** - Core `ToolExecutor` with policy enforcement
- **`security.rs`** - Security policy for path traversal, command injection, prompt injection, credential exposure protection
- **`limits.rs`** - Resource limits: budget tracking, timeouts, concurrency, output size limits
- **`error.rs`** - Error types for tool operations
- **`mcp_bridge.rs`** - Bridge to existing MCP connectors without duplicating transport
- **`tools.rs`** - Built-in tool implementations

### Built-in Tools

| Tool | Category | Capabilities |
|------|----------|--------------|
| `fs.discover` | Filesystem | Read |
| `fs.read` | Filesystem | Read |
| `fs.glob` | Filesystem | Read |
| `code.search` | Code Analysis | Read |
| `code.symbols` | Code Analysis | Read |
| `code.callers` | Code Analysis | Read |
| `fs.patch` | File Editing | Write |
| `fs.create` | File Editing | Write |
| `fs.delete` | File Editing | Write |
| `git.status` | Git | Read |
| `git.diff` | Git | Read |
| `git.history` | Git | Read |
| `sandbox.exec` | Sandbox | Execute |
| `sandbox.build` | Sandbox | Execute |
| `sandbox.test` | Sandbox | Execute |
| `sandbox.lint` | Sandbox | Execute |
| `diag.read` | Diagnostics | Read |
| `diag.logs` | Diagnostics | Read |

### Security Features

1. **Path Traversal Protection**: Rejects `../`, absolute paths, forbidden prefixes (`.git`, `.ssh`, etc.)
2. **Symlink Escape Detection**: Validates resolved paths stay within worktree
3. **Command Injection Prevention**: Blocks dangerous commands, detects shell metacharacters
4. **Prompt Injection Protection**: Detects jailbreak patterns, instruction overrides
5. **Credential Exposure Scanning**: Detects API keys, passwords, private keys in arguments
6. **Output Size Limits**: Configurable limits with truncation option

### Capability Model

```rust
CapabilitySet {
    Read:   "read files, search code, inspect git history"
    Write:  "create, modify, delete files"
    Execute: "run sandboxed build/test/lint"
    Network: "MCP connector tool calls"
}
```

Tools declare their required capabilities, and the executor checks against granted capabilities before allowing execution.

### Resource Limits

```rust
ResourceLimits {
    output_limits: OutputLimits,
    timeout_policy: TimeoutPolicy,
    max_total_calls: u64,
    max_calls_per_tool: Option<u64>,
    max_calls_per_category: Option<u64>,
    rate_limit_max: Option<u64>,
    rate_limit_window: Duration,
    max_concurrent: usize,
    max_memory_bytes: usize,
}
```

Includes `ResourceLimits::strict()` for untrusted code and `ResourceLimits::permissive()` for trusted environments.

### Worktree Isolation

All file modifications through `fs.*` tools are written to the isolated worktree via the `WorkspaceLease` trait. The main checkout is never modified until a task is validated and merged.

### Approval Flow

1. Tool invoked with arguments
2. Tool definition looked up
3. Capabilities checked
4. Approval policy determined (never, always, first_call, on_mutating, by_capability)
5. Approval gate consulted (may prompt operator)
6. Security validations run
7. Resource budget checked
8. Tool executes with timeout
9. Result validated

### MCP Integration

The `mcp_bridge` module bridges to existing MCP connectors using the transport implementations from `ryoko-core::mcp`. Tools are namespaced: `github__list_issues`, `cloudflare__kv_get`.

## Testing

### Unit Tests

Each module includes unit tests for:
- Capability enforcement
- Approval gating
- Security validations
- Resource limits
- Tool registration

### Adversarial Tests

Located in `tests/adversarial.rs`:
- Path traversal attack prevention
- Command injection detection
- Prompt injection detection  
- Credential exposure detection
- Budget exhaustion protection
- Output size limit enforcement
- Concurrency limit enforcement

## Documentation

- `docs/architecture/TOOL_ENGINE.md` - Complete API documentation
- Cargo docs generated via `cargo doc -p ryoko-tools`

## Breaking Changes

None - this is a new crate with no modifications to existing APIs.

## Checklist

- [x] Unit tests pass
- [x] Adversarial tests pass
- [x] Documentation updated
- [x] API follows existing Ryoko patterns
- [x] No direct filesystem/network access outside approved paths
- [x] All mutating operations require write capability
- [x] Operator approval required for sensitive operations

## Example Usage

```rust
use ryoko_tools::{
    ToolExecutor, ToolRegistry, ExecutionContext,
    ApprovalGate, SecurityPolicy, ResourceLimits,
    register_builtin_tools,
};

let registry = Arc::new(ToolRegistry::with_builtins());

let executor = ToolExecutor::new(
    registry,
    ResourceLimits::default(),
    SecurityPolicy::default(),
    Arc::new(ApprovalGate::new(None)),
    None,
);

register_builtin_tools(&executor)?;

let result = executor.execute(
    "fs.read",
    json!({"path": "src/main.rs", "limit": 100}),
    &context,
).await?;

assert!(result.success);
```

## Related Issues

Implements core requirements from the Part 2 mission:
- ✅ Unified, typed Tool Execution API
- ✅ Capability-based permissions
- ✅ Worktree isolation for mutating operations
- ✅ Explicit operator approval for sensitive operations
- ✅ Output-size limits, timeouts, cancellation, budgets
- ✅ Protection against path traversal, symlink escapes, command injection, prompt injection, credential exposure
- ✅ Integration with existing MCP connectors
- ✅ Never silently modifies user's original checkout

## Future Work

- Integration with ModelAgent for tool-calling
- Integration with Orchestrator for task-scoped capabilities
- Additional language-specific sandbox tools
- Web-search tools with network capability
- Incremental symbol index integration for code search