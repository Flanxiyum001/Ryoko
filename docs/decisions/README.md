# Architecture decision records

Each decision records the choice, the alternatives considered and the trade-off
accepted. An ADR is superseded, not edited, when the decision changes.

| ADR | Decision | Alternatives considered | Trade-off accepted |
| --- | --- | --- | --- |
| 001 | Tauri 2 as the native shell | Electron; native toolkit | Smaller binaries and a Rust backend; smaller plugin ecosystem |
| 002 | Rust as the trusted backend | Node/Go backend | One language across domain and shell; longer compile times |
| 003 | React + TypeScript frontend | Svelte; Solid | Largest ecosystem and hiring pool; higher baseline bundle cost |
| 004 | Monaco as the editor engine | CodeMirror 6; custom editor | VS Code-class editing now; larger bundle, benchmark before lock-in |
| 005 | LSP for language intelligence | Per-language in-process parsers | Reuse mature servers; process management complexity |
| 006 | SQLite for local persistence | JSON files; embedded KV | Relational queries and migrations; single-writer constraints |
| 007 | Typed IPC commands and versioned events | Ad-hoc JSON RPC | Compile-time safety; schema upkeep |
| 008 | IACP for inter-agent messages | Freeform agent chat | Auditable, validateable collaboration; envelope overhead |
| 009 | Git worktrees for task isolation | File copies; branches in one working tree | True filesystem isolation; worktree lifecycle cost |
| 010 | Restricted execution mandatory for agent code | Trust the agent's command | Safety; runner implementation cost |
| 011 | Provider-neutral gateway with per-adapter wire code | Single universal API | Real provider fidelity; more adapters to maintain |
| 012 | Human approval for privileged operations | Policy-only automation | User control; interruption cost |
| 013 | Durable event journal | In-memory broadcast only | Recovery and replay; storage and retention work |
| 014 | Versioned plugin API before a marketplace | Ship a marketplace early | Stability; slower ecosystem growth |

## Format for new ADRs

```markdown
# ADR-NNN: Title

Status: proposed | accepted | superseded by ADR-MMM
Date: YYYY-MM-DD

## Context
## Decision
## Alternatives
## Consequences
## Verification
```

## Open decisions

- Editor engine final choice after bundle, accessibility and language benchmarks.
- Container versus VM default for project execution.
- Memory scoping across workspaces.
- Whether the CLI and desktop share one storage schema or separate databases.
