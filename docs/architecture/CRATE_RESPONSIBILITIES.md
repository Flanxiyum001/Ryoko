# Crate responsibilities

The workspace keeps a small number of focused crates. A planned crate is created
only when its first real consumer exists.

## Current crates

| Crate | Owns | Does not own |
| --- | --- | --- |
| `ryoko-cli` | Argument parsing, terminal workflows, provider/team configuration, run and review commands | Domain rules, provider payloads |
| `ryoko-core` | Domain types (`Task`, `TaskResult`, `AgentRole`), agent runtime contract, `ModelAgent`, orchestrator, IACP, in-process memory blackboard, tree-sitter context extraction | HTTP, process spawning, editor UI |
| `ryoko-models` | Provider adapters, provider catalogue, model routing, streaming events, usage and budget ledger | Task planning, file access |
| `ryoko-native` | LSP server over stdio, diagnostics publishing, code lenses, quick fixes, execute commands | Workspace lifetime, model calls |
| `ryoko-vfs` | Virtual document store, incremental text edits, UTF-16 positions, snapshots | LSP transport, persistence |
| `ryoko-sandbox` | WASM validation guardrail, execution limits, sandbox spec evidence | Project build isolation, container management |
| `ryoko-worktree` | Git worktrees, task branches, change proposals, publication and cleanup | Review UI, permission policy |
| `ryoko-scripting` | Reserved scripting/plugin layer | Anything until its API is specified |

## Planned crates

| Crate | First deliverable | Depends on |
| --- | --- | --- |
| `ryoko-workspace` | Project open/close, workspace state, file watching | `ryoko-vfs` |
| `ryoko-editor` | Document model, versions, stale-patch rejection | `ryoko-vfs`, `ryoko-lsp` |
| `ryoko-lsp` | Language-server process manager and capability negotiation | `ryoko-vfs` |
| `ryoko-context` | Indexing pipeline, symbol graph, hybrid retrieval | `ryoko-core`, tree-sitter |
| `ryoko-memory` | Durable memory with provenance and lifecycle | storage |
| `ryoko-agents` | Definitions, prompts, tool contracts, dynamic team planner | `ryoko-core`, `ryoko-models` |
| `ryoko-orchestrator` | Durable DAG, attempts, cancellation, recovery | `ryoko-core`, storage |
| `ryoko-tools` | Typed tool surface with scope checks | permissions |
| `ryoko-permissions` | Capability model, policy evaluation, approvals | core types |
| `ryoko-execution` | Restricted project runner with attestations | sandbox, permissions |
| `ryoko-git` | Status, diff, staging, branch and conflict operations | `ryoko-worktree` |
| `ryoko-terminal` | Interactive terminal sessions under policy | permissions |
| `ryoko-debugger` | DAP client and session state | `ryoko-execution` |
| `ryoko-plugins` | Plugin manifest, host functions, quotas | sandbox, permissions |
| `ryoko-storage` | SQLite schema, migrations, repositories | nothing domain-specific |
| `ryoko-events` | Event definitions, versioning, journal encoding | serialization only |
| `ryoko-telemetry` | Structured logs, spans, metrics, redaction | `ryoko-events` |

## Rules

1. **One owner per concept.** Task state lives in the orchestrator; IACP transports
   messages and never becomes the source of truth for task state.
2. **Traits at the boundary.** Provider, workspace, runner and storage access go
   through traits owned by domain crates.
3. **No upward dependencies.** `ryoko-core` must not depend on adapters, the CLI or
   any UI crate.
4. **No cycles, including through features.** Split a crate rather than adding a
   back-edge.
5. **Extract on demand.** Do not create a crate until a second consumer needs the
   abstraction or the module is too large to review comfortably.
6. **Tests travel with the crate.** A crate without tests is not finished.

## Promotion checklist

Before adding a crate: name the consumer, name the trait boundary, name the tests,
and confirm no existing crate already owns the responsibility. Adding a crate is a
public API decision and is recorded in `docs/decisions/`.
