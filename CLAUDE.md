# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Common Development Commands

```sh
# Format check
cargo fmt --all -- --check

# Lint
cargo clippy --workspace --all-targets --locked -- -D warnings

# Run all tests
cargo test --workspace --locked

# Run a single test (e.g., in ryoko-core)
cargo test -p ryoko-core --locked <test_name>

# Build CLI
cargo build -p ryoko-cli --locked

# Run CLI commands
cargo run -p ryoko-cli -- <command> [args]

# Run team preview (read-only, contacts provider)
cargo run -p ryoko-cli -- team-preview --config configs/providers.example.json --model ollama/qwen2.5-coder --goal "..."

# Run isolated agent task
cargo run -p ryoko-cli -- run --config configs/providers.example.json --model ollama/qwen2.5-coder --repo . "..."

# Index workspace symbols
cargo run -p ryoko-cli -- index --repo .

# Check worktree with sandbox
cargo run -p ryoko-cli -- check --repo . --sandbox-image reviewed-toolchain:latest
```

## Architecture Overview

### Workspace Crates

| Crate | Purpose |
|-------|---------|
| `ryoko-models` | Provider routing, normalized messages, streaming, usage accounting |
| `ryoko-core` | Task DAG, agent lifecycle, IACP messages, MCP tool policy/toolbox, memory, permissions, tree-sitter context extraction |
| `ryoko-worktree` | Isolated task Git worktrees, merge integration, reviewable internal change proposals |
| `ryoko-sandbox` | WASI guardrail, generated offline OCI images, restricted Docker/Podman pre-flight execution |
| `ryoko-cli` | Terminal-facing provider setup, team preview, isolated generation, human review |
| `ryoko-native` | `tower-lsp` language server, agent diagnostic publisher |
| `ryoko-vfs` | Incremental virtual document store used by language server |
| `ryoko-scripting` | WASI-sandboxed plugin manifest, ABI, host, agent adapter |
| `ryoko-marketplace` | Extension/skill marketplace client |

### Key Modules in `ryoko-core`

- **`context.rs`** - Multi-language structural context extraction via tree-sitter (Rust, TypeScript/TSX, JavaScript, Python, Go, C/C++, Java, Kotlin, C#, Swift, SQL)
- **`index.rs`** - SQLite cross-file symbol index with call graph (`WorkspaceIndex`)
- **`orchestrator.rs`** - Task DAG execution with concurrency control, retry/repair cycles
- **`agents.rs`** - Agent role definitions and runtime traits
- **`spec_link.rs`** - Living spec↔code links with drift detection (FNV-1a digests)
- **`memory.rs` / `project_memory.rs`** - Human-editable project memory, rejection log
- **`team.rs`** - Standard engineering team roles and dynamic planning
- **`mcp.rs`** - MCP client, toolbox, connector management

### Context Extraction Flow

1. `ContextExtractor` parses source with tree-sitter → `FileContext` (imports, symbols, intra-file calls, optional bodies)
2. `extract_files()` parallelizes across workers preserving input order
3. `WorkspaceIndex.rebuild()` stores symbols + call edges in SQLite
4. `WorkspaceIndex.context_for_terms()` retrieves bounded context for goal terms
5. `structural_context()` in `ryoko-cli/src/context_window.rs` scans project for bounded outline

### Worktree & Review Flow

1. `ryoko run` → creates isolated Git worktree, runs agent, validates in sandbox, publishes branch
2. `ryoko review` → shows diff + pre-flight result, accepts/rejects/hunk-selects/feeds back
3. On accept → merges to target branch, optionally signs attestation

### Security Model

- All pre-flight runs in Docker/Podman container (never host by default)
- `--unsafe-host-check` requires explicit opt-in with warning
- API keys only via environment variables (`api_key_env`)
- Attestations signed with SSH keys, stored in Git notes
- Model output never trusted for filesystem/network/shell actions

## Important Files

- `AGENTS.md` - Engineering rules, ground rules, required checks, layout, review process
- `docs/architecture/CRATE_RESPONSIBILITIES.md` - Ownership boundaries and dependency direction rules
- `docs/architecture/IACP.md` - Inter-Agent Communication Protocol specification
- `docs/security/EXECUTION_SANDBOX.md` - Sandbox security model and limitations
- `configs/providers.example.json` - Provider configuration template

## Current Part 4 Goal: Incremental Context Engine

The current symbol index (`WorkspaceIndex`) does full rebuilds. The goal is to implement:

1. **Incremental indexing** - Update only changed files using content hashes
2. **Filesystem events** - `notify` crate for real-time changes, with periodic reconciliation
3. **Transactional updates** - Per-file SQLite transactions
4. **Cross-file relationships** - Imports, dependencies, call graph, test discovery
5. **Hybrid context retrieval** - Filename, symbol, text, dependency, call graph search with token budgets
6. **Cache invalidation** - Stale context detection before engineering decisions
7. **Benchmarks** - Initial index, reindex, lookup, retrieval, token consumption vs Part 1 baseline