# System architecture

Status: target design. Only the crates and services listed under "Implemented
today" exist.

## Implemented today

```text
ryoko-cli  ──►  ryoko-core (agents, IACP, orchestrator, memory, context)
    │              │
    │              ├─► ryoko-models (provider adapters, routing, usage)
    │              ├─► ryoko-worktree (isolated Git worktrees)
    │              └─► ryoko-sandbox (WASM validation guardrail)
    └─► ryoko-native (LSP server) ─► ryoko-vfs (document store)
```

Everything else in this document is the destination.

## Layers

| Layer | Responsibility | Must not |
| --- | --- | --- |
| Presentation | Render state, capture intent, keyboard-first UX | Execute shell commands, read credentials, talk to providers |
| IPC | Typed commands, versioned events, cancellation, backpressure | Contain business rules |
| Workspace services | Files, Git, LSP lifecycle, terminal sessions | Call models |
| AI services | Provider routing, context retrieval, memory | Touch the filesystem directly |
| Agent runtime | DAG scheduling, IACP routing, isolation, review gates | Bypass permissions |
| Security services | Capability checks, approvals, sandbox, audit | Be mutable by model output |
| Adapters | Provider, Git, LSP, runner, plugin boundary crossings | Leak vendor payloads upward |
| Local environment | Project files, database, worktrees, credentials | Be reachable from the frontend directly |

## Dependency direction

```text
presentation → ipc → services → adapters → environment
```

Dependencies point inward. Domain crates never depend on the desktop frontend.
Adapters depend on domain traits, never the reverse. No circular crate
dependencies.

## Trust boundaries

1. **Model output → validation.** Responses, tool arguments and patches are
   untrusted. They are schema-checked, path-checked, scope-checked and
   budget-checked before use.
2. **Frontend → backend.** The frontend is a view. It cannot widen a capability,
   mark a task complete, or approve its own request.
3. **Project content → instructions.** README files, comments, dependencies and
   build output are data. They are never parsed as policy.
4. **Backend → operating system.** Only the runner performs privileged work, under
   explicit policy, with resource limits and audit.

## Request lifecycle for an engineering task

```text
user goal
  → plan (tasks, dependencies, acceptance criteria)
  → team proposal (roles, models, budgets)
  → approval of scope and permissions
  → per-task worktree + branch + tool allowlist
  → execution with streamed progress events
  → structured proposal (patch + tests + evidence)
  → validation in restricted runner (bound to source tree hash)
  → independent review (findings with file/line references)
  → human review of diff and evidence
  → integrate, reject, or request changes
```

Every transition is a durable, ordered event. Retries create new attempts; history
is never rewritten.

## State ownership

| State | Owner | Durability |
| --- | --- | --- |
| Open documents and versions | Document manager | memory + recovery snapshot |
| Workspace files | Filesystem via workspace service | filesystem |
| Task graph and attempts | Orchestrator | SQLite |
| IACP messages and audit | IACP router | append-only journal |
| Agent transcripts | Agent runtime | SQLite + artifact store |
| Approvals | Security service | SQLite |
| Memories | Memory service | SQLite, with provenance |
| Usage and budgets | Model gateway | SQLite ledger |

In-memory broadcast channels may accelerate UI updates, but they are never the
only record of execution state.

## Failure semantics

- A failed validation rolls back the task's lease and skips dependents.
- A crashed run is recoverable from the journal; incomplete attempts are marked
  as such rather than silently resumed.
- Cancellation propagates to tool execution, runner processes and provider
  streams, and is acknowledged within a bounded interval.
- Stale evidence (produced from a different source tree) is rejected, not
  presented as validation.
