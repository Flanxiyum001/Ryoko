# Ryoko 2.0 — master blueprint

Status: architecture proposal. Ryoko is pre-alpha. This document records scope and
milestones; it does not assert that any proposed subsystem exists.

Product vision: a developer environment where a human developer and a team of
specialised AI engineers work together inside one professional IDE, with the human
retaining ownership of the project and every decision.

## Principles

| Principle | Engineering consequence |
| --- | --- |
| Human-first | Integration, push, deploy and release always need human approval |
| AI-native | Agents are part of the workflow, not an attached chat box |
| Provider-neutral | No mandatory model vendor; adapters live in `ryoko-models` |
| Local-first | Project data stays local by default; no silent upload |
| Secure by design | Capabilities, scoped tools, restricted execution, audit |
| Verifiable | Every completed task carries evidence bound to a source snapshot |
| Extensible | Plugins, custom agents and workflow templates behind a stable API |
| Cross-platform | Windows, macOS and Linux are release targets |
| Professional | Keyboard-first, fast, predictable, accessible |

## Two pillars

1. **IDE engine** — editor, LSP, Git, debugger, terminal.
2. **AI engine** — models, context, memory, routing, agent teams.

Neither is a secondary feature. Both sit on a shared foundation of workspace,
storage, events, plugins and a cross-platform runtime.

## Operating modes

- **Editor** — normal development, with AI available inline.
- **Agent** — assign work, watch execution, review proposals.
- **Mission Control** — plans, dependency graphs, agent teams, approvals,
  execution history.

These are views over one shared application state, not separate products.

## Stack

| Layer | Choice | Status |
| --- | --- | --- |
| Core backend | Rust (2024 edition) | exists |
| Desktop shell | Tauri 2 | planned |
| Frontend | React + TypeScript | planned |
| Editor | Monaco | planned |
| Language intelligence | LSP (`tower-lsp`) | server exists in `ryoko-native` |
| Parsing/indexing | tree-sitter | exists for Rust in `ryoko-core` context extraction |
| Async runtime | Tokio | exists |
| Local storage | SQLite via SQLx | planned |
| Serialization | Serde | exists |
| Inter-agent protocol | IACP | v1 implemented, v2 proposed |
| Sandbox | WASM validation + restricted runner | WASM validator exists; project runner planned |
| IPC | typed Tauri commands and versioned events | planned |
| GitHub | separate `services/github-app` broker | experimental |

Monaco is the editor, not the IDE. File management, language-server lifecycle,
diagnostics, debugging, terminal, review and workspace state belong to Ryoko.

## Target architecture

```text
Desktop application   React · TypeScript · Monaco · panels · command palette
        |
Typed desktop IPC     commands · events · requests · cancellation
        |
Workspace services    files · Git · LSP · terminal
AI services           models · context · memory
Agent runtime         DAG · IACP · scheduling · review
Security services     permissions · sandbox · audit
        |
Infrastructure adapters  providers · Git · LSP servers · runners · plugins
        |
Local environment     project files · SQLite · worktrees · credentials
```

Hard rule: **the frontend never directly controls system-level operations.** An
agent requests a typed operation; the Rust runtime checks identity, task scope,
permissions, limits and approval requirements before anything executes.

## Milestones

### M0 — Repository stabilisation

- Audit every crate, feature and test; record a verified current-state map.
- Fix outstanding security findings (filesystem TOCTOU, stale validation).
- Establish stable domain contracts and architecture decision records.
- Make CI checks mandatory; define the supported OS and release matrix.

Exit: clean, reproducible workspace with documented security boundaries.

### M1 — Native desktop foundation

Tauri 2 shell, React/TypeScript frontend, typed Rust commands and events, SQLite
migrations, settings, OS credential storage, application shell.

Exit: packaged application launches on Windows, macOS and Linux.

### M2 — Professional editor

Monaco integration, file explorer, tabs and splits, document synchronisation,
LSP manager, search, terminal, Git status and diff.

Exit: a developer can do normal daily work without leaving Ryoko.

### M3 — Single-agent engineering

Context retrieval, model gateway improvements, structured patch contract, scoped
tool execution, isolated worktree, restricted validation, diff and approval.

Exit: one agent can implement, validate and propose a real project change.

### M4 — Multi-agent orchestration

Agent registry, task DAG, dynamic team planning, IACP v2, parallel execution,
conflict detection, independent review, Mission Control.

Exit: several agents complete coordinated tasks without silent overwrites.

### M5 — Deep project intelligence

Incremental indexing, tree-sitter symbols, LSP semantic retrieval, symbol graph,
hybrid search, persistent project memory, retrieval evaluation.

Exit: agents answer project-specific questions with traceable evidence.

### M6 — Advanced IDE

Debug Adapter Protocol, test explorer, application preview, advanced Git,
extension API, themes and custom workflows.

Exit: Ryoko is a comprehensive developer environment.

### M7 — Public release

Adversarial security testing, performance testing, crash recovery, installers,
code signing, notarisation, updates, documentation, rollback.

Exit: dependable public beta with documented limitations.

## Critical path

```text
Repository stabilisation
  → native desktop foundation
  → workspace and document model
  → editor + LSP + Git
  → single-agent execution
  → multi-agent orchestration
  → advanced context and memory
  → public release
```

Parallel work is allowed for security, provider adapters, GitHub integration,
indexing and UI design. Multi-agent parallel file editing must not start before
document synchronisation, patch validation and worktree isolation are dependable.

## What not to build yet

Plugin marketplace, cloud agent hosting, real-time team collaboration, fully
autonomous deployment, custom model training, a custom editor engine, a large
agent catalogue, a complex vector database and an AI UI builder. Each depends on
foundations that do not exist yet.

The main risk is feature sprawl. Ryoko must be excellent at ordinary development
and single-agent engineering before expanding.

## Definition of done for the product

A native IDE on three platforms; a professional editor with tabs, splits,
intelligence, search and diagnostics; Git, terminal, LSP and debugging; provider
independence; safe single-agent project changes; durable multi-agent
orchestration; retrievable context with provenance; inspectable diffs and
approvals; scoped permissions and restricted execution; cancellation and
recovery; reproducible AI evaluation; signed, tested cross-platform releases.

## Current gap map

| Blueprint area | Today | Gap |
| --- | --- | --- |
| Editor UI | LSP server and virtual document store only | No desktop shell, no Monaco |
| Preview | none | Preview runner, screenshot and console capture |
| Code review | CLI diff review in isolated worktrees | Structured findings with file/line refs |
| Architecture designer | none | Model, diagram generation, decision sync |
| AI debugging | LSP diagnostics channel | Runner output capture and feedback loop |
| Extensions | `ryoko-scripting` placeholder | Stable API, permissions, WASM host |
| Project memory | In-process `SharedMemory` blackboard | Durable, searchable, provenance-tracked store |
| Multi-agent | Review-only team preview | Real parallel execution with durable state |
| Persistence | none | SQLite schema, migrations, event journal |
