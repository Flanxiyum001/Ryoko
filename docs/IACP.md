# IACP v1.0 — Inter-Agent Communication Protocol

Implementation: [`crates/ryoko-core/src/iacp.rs`](../crates/ryoko-core/src/iacp.rs)

IACP makes agent collaboration typed and auditable. An agent never hands
freeform text to another agent directly: it emits an envelope, and the Rust
router either accepts it or rejects it with a structured error before any model
or terminal call happens.

## Envelope

```json
{
  "iacp_version": "1.0",
  "message_id": "e2a5f4b8-c3d9-4b2e-a1f7-6c8d9e0f1a2b",
  "timestamp": "2026-11-07T05:52:00Z",
  "correlation_id": "9b1deb4d-3b7d-4bad-9bdd-2b0d7b3dcb6d",
  "sender": { "role": "ProjectManager", "agent_id": "pm-agent-01" },
  "recipient": { "role": "SeniorDeveloper", "agent_id": "dev-agent-01" },
  "event_type": "TASK_DELEGATED",
  "payload": { }
}
```

`event_type` and `payload` are flattened onto the envelope, matching the JSON
contract above. The Rust payload enum is adjacently tagged with
`tag = "event_type", content = "payload"` (not `data`).

Supported events:

| `event_type` | Purpose |
| --- | --- |
| `TASK_DELEGATED` | PM assigns scoped, budgeted work with DoD and file locks |
| `CODE_REVIEW_REQUESTED` | Author hands a validated branch/proposal to a reviewer |
| `CODE_REVIEW_FEEDBACK` | Reviewer returns a verdict and line annotations |
| `CONTEXT_QUERY_REQUEST` | Agent asks another agent for a schema/context fact |

Enums are wire-strict: `priority` is `CRITICAL|HIGH|MEDIUM|LOW`, verdict is
`APPROVED|CHANGES_REQUESTED|REJECTED`, file status is `ADDED|MODIFIED|DELETED`,
severity is `INFO|WARNING|CRITICAL`, compilation status is
`SUCCESS|FAILED|NOT_RUN`.

## Router as a firewall

`IacpRouter::route` performs these checks in order:

1. **Envelope integrity** — version is exactly `1.0`, IDs are non-nil, sender and
   recipient are different known actors with non-empty roles.
2. **Schema** — required fields, non-empty requirements, test-count consistency,
   unique changed files, `line_number >= 1`, etc.
3. **Path safety** — every target directory, file lock, changed file and
   annotation path rejects absolute paths and `..` traversal via the same
   validator used by workspace leases.
4. **Tool allowlist** — every `scope.allowed_tools` entry must exist in
   `IacpPolicy::allowed_tools`. Unknown tools (e.g. `rm_rf`) never reach a bridge.
5. **Role permissions** — only `ProjectManager`/`TechLead` may delegate; review
   requests must target `CodeReviewer`/`QaEngineer`; only reviewers may return
   feedback. A `UI_Designer` cannot delegate work at all.
6. **Budget ceiling** — `allocated_token_budget` must be positive and at most
   `IacpPolicy::max_token_allocation` (default 1,000,000). Accepted grants are
   credited to the **recipient**.
7. **File locks** — locks are acquired for the recipient. A conflicting holder
   rejects the whole envelope atomically; release with `release_agent_locks` or
   `release_task_locks`.
8. **Audit** — accepted and rejected envelopes are appended to a bounded,
   ordered journal (`iacp.audit`) for the replay timeline.

Accepted envelopes are published, in order, to `IacpRouter::subscribe()`; that
stream is the local WebSocket/Tauri bridge to the UI.

## Token enforcement

Delegation grants budget; the model gateway must charge it before dispatch:

```rust
let remaining = router.record_token_usage("dev-agent-01", 8_400)?;
// Err(TokenBudgetExceeded { requested, remaining, .. }) before any LLM call.
```

`record_token_usage` is atomic and returns the remaining grant. `budget()`
exposes `granted_tokens`, `used_tokens`, `remaining_tokens`.

## JSON-RPC 2.0 ingress

`IacpService` accepts frames from a local WebSocket (or IPC) and exposes:

| Method | Params | Result |
| --- | --- | --- |
| `iacp.route` | IACP envelope | `RoutedMessage` with audit sequence and locks |
| `iacp.audit` | none | ordered audit journal |
| `iacp.budget` | `{ "agent_id": "..." }` | budget status |

```json
{"jsonrpc":"2.0","id":7,"method":"iacp.route","params":{ "...envelope..." }}
```

Successful routing result:

```json
{"jsonrpc":"2.0","id":7,"result":{"message_id":"...","event_type":"TASK_DELEGATED","accepted":true,"audit_sequence":1,"locked_files":["src/lib.rs"]}}
```

Errors use standard JSON-RPC codes (`-32700` parse, `-32600` invalid request,
`-32601` unknown method, `-32602` invalid params) plus `-32010` for policy,
role, lock and budget violations, with `error.data.kind` set to a stable
machine-readable reason.

## Rust usage

```rust
use ryoko_core::iacp::{
    AgentIdentity, IacpEnvelope, IacpPayload, IacpPolicy, IacpRouter,
};

let router = IacpRouter::new(IacpPolicy::default());
let envelope = IacpEnvelope::new(
    AgentIdentity::new("ProjectManager", "pm-agent-01"),
    AgentIdentity::new("SeniorDeveloper", "dev-agent-01"),
    IacpPayload::TaskDelegated {
        task_id: uuid::Uuid::new_v4(),
        title: "Implement vector indexing".into(),
        priority: ryoko_core::iacp::TaskPriority::High,
        scope: ryoko_core::iacp::TaskScope {
            target_directories: vec!["src/indexing".into()],
            allowed_tools: vec!["read_file".into(), "cargo_test".into()],
            file_locks: vec!["src/lib.rs".into()],
        },
        requirements: vec!["85% block coverage".into()],
        allocated_token_budget: 500_000,
    },
);

router.route(envelope)?; // validated, locked, budgeted, audited
```

## Tests

`cargo test -p ryoko-core` covers wire round-trips, documented payload D,
unknown-event rejection, role/tool/path policy, file-lock conflicts and release,
token exhaustion, audit ordering, broadcast delivery and all JSON-RPC paths.
