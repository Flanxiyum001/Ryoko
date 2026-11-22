# IACP v2 — proposal

Status: proposal. IACP v1.0 is implemented and documented in [../IACP.md](../IACP.md).
This document describes an additive evolution; it does not describe shipped code.

## Goals

- Carry a task through its whole lifecycle, not only delegation and review.
- Make retries, cancellation and recovery explicit.
- Separate transport from orchestration state.
- Stay compatible with v1 routers for v1 messages.

## Compatibility

1. `iacp_version` becomes `"2.0"`. A v2 router accepts v1 envelopes by upgrading
   them internally; a v1 router rejects `"2.0"` with `UNSUPPORTED_VERSION`.
2. New optional fields never change the meaning of existing ones.
3. `event_type` values from v1 keep their payload shape.
4. Version negotiation happens on connect; the negotiated version is recorded in
   the audit journal.

## New envelope fields

| Field | Purpose |
| --- | --- |
| `task_id` | Authoritative task identity (v1 carries it inside payloads) |
| `attempt_id` | Distinguishes retries from the original attempt |
| `causation_id` | The message that caused this one |
| `idempotency_key` | Deduplicates retries across transport reconnects |
| `deadline` | Absolute time after which the message is invalid |
| `redaction` | Names of fields removed before journaling |

Payloads are bounded: string fields carry documented maximum lengths, file lists
are capped, and oversized envelopes are rejected before routing.

## Lifecycle events

| `event_type` | Direction | Purpose |
| --- | --- | --- |
| `TASK_DELEGATED` | delegator → agent | Task contract (v1 shape, extended) |
| `TASK_ACKNOWLEDGED` | agent → delegator | Confirms understanding and planned scope |
| `TASK_PROGRESS` | agent → delegator | Bounded progress updates with percentage or phase |
| `CONTEXT_REQUESTED` | agent → owner | Asks for specific files, symbols or facts |
| `CONTEXT_PROVIDED` | owner → agent | Returns cited, versioned context |
| `ARTIFACT_PUBLISHED` | agent → review | Patch, report or test artifact with hashes |
| `REVIEW_REQUESTED` | author → reviewer | Requests independent verification |
| `REVIEW_FEEDBACK` | reviewer → author | Findings with file and line references |
| `DEPENDENCY_SATISFIED` | orchestrator → dependents | Unblocks downstream tasks |
| `BUDGET_RESERVED` / `BUDGET_CONSUMED` | gateway ↔ runtime | Reservation and actual usage |
| `TASK_CANCELLED` | delegator → agent | Stop work; acknowledgement required |
| `TASK_RECOVERED` | runtime → journal | Resumes a recoverable attempt after a crash |

## Task contract example

```json
{
  "iacp_version": "2.0",
  "message_id": "b1e6a2c4-2f5f-4b8a-9f6a-3d2f0c1a7e11",
  "task_id": "task-123",
  "attempt_id": "attempt-1",
  "idempotency_key": "task-123:attempt-1:delegated",
  "timestamp": "2026-11-14T00:00:00Z",
  "sender": { "role": "ProjectManager", "agent_id": "pm-01" },
  "recipient": { "role": "FrontendEngineer", "agent_id": "fe-01" },
  "event_type": "TASK_DELEGATED",
  "payload": {
    "title": "Implement authentication UI",
    "goal": "Login and registration screens",
    "scope": {
      "target_directories": ["src/auth"],
      "allowed_tools": ["read_file", "search_code", "apply_patch", "run_tests"],
      "file_locks": ["src/auth/mod.rs"]
    },
    "acceptance_criteria": [
      "Login screen is keyboard accessible",
      "Form validation rejects malformed input",
      "Existing tests pass"
    ],
    "requirements": ["No new dependencies without approval"],
    "allocated_token_budget": 30000,
    "requires_human_approval": true
  }
}
```

## Routing rules added in v2

- `task_id` must be consistent across every message for that task.
- `attempt_id` must increase monotonically per task; reuse is rejected.
- Duplicate `idempotency_key` returns the original result instead of re-executing.
- Cancellation is accepted only from the delegating identity or the orchestrator.
- Review feedback must reference a published artifact hash.
- Journal entries record the negotiated version and any redacted fields.

## Orchestration separation

IACP transports validated collaboration messages. The orchestrator owns task state,
attempt history and scheduling. A message can request a transition; it cannot
perform one. This prevents a compromised agent from rewriting task truth by
sending a message.

## Migration plan

1. Implement v2 envelope types alongside v1 without changing v1 behaviour.
2. Dual-read: the router accepts both; the journal records the original version.
3. Add acknowledgement and progress events for the single-agent workflow first.
4. Add artifact and review events when structured proposals carry hashes.
5. Deprecate v1 emission internally, keep v1 acceptance for compatibility.
6. Document the removal date only after a stable release accepts v2.
