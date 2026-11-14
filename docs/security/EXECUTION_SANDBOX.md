# Execution sandbox

Status: `ryoko-sandbox` validates WASM artifacts under fuel, time and memory limits
with no host filesystem or network preopens. Project build/test commands run on the
host. A hardened project runner is planned.

Do not confuse the two. The WASM guardrail validates a submitted artifact; it does
not isolate `cargo test` against a real project.

## Current guardrail

- Fresh wasmtime store per artifact.
- No host preopens, no inherited environment, no network.
- Fuel bounds CPU; epoch deadline bounds wall time; `StoreLimits` bounds linear
  memory and tables.
- Only a clean `_start` return counts as a pass.
- `SandboxSpecValidator` refuses proposals without tests.

## Planned restricted runner

```text
task request → policy check → allocate isolated environment
  → materialise exact source snapshot read-only
  → run allowlisted commands with limits
  → export declared artifacts only
  → destroy environment
```

Requirements:

| Area | Requirement |
| --- | --- |
| Isolation | Container or disposable VM; VM preferred for sensitive workloads |
| Filesystem | Read-only source mount, writable scratch outside it, explicit artifact export |
| Identity | Non-root user, no privilege escalation, no host device access |
| Limits | CPU, memory, process count, disk, wall-clock timeout |
| Network | Denied by default; allowlist per task if the user approves it |
| Secrets | Not mounted; injected only for explicitly approved operations |
| Dependencies | Controlled cache, pinned by lockfile |
| Cleanup | Environment destroyed after the run; nothing persists implicitly |

The host enforces limits. A command, container flag or configuration supplied by a
model is never trusted to enforce its own isolation.

## Attestation

Every validation result records:

```json
{
  "workspace_id": "workspace-1",
  "task_id": "task-123",
  "attempt_id": "attempt-2",
  "source_tree_hash": "sha256:...",
  "runner_image_digest": "sha256:...",
  "commands": ["cargo test --locked"],
  "exit_code": 0,
  "started_at": "2026-11-14T00:00:00Z",
  "finished_at": "2026-11-14T00:02:00Z",
  "status": "passed"
}
```

If the source tree changes, the attestation no longer applies to it. Review shows
the hash so a human can see exactly what was validated.

## Refusal conditions

Refuse to run when: the source hash cannot be computed, the runner image digest is
unknown, the task has no acceptance command, the command would modify the source
mount, or the required network or secret access was not approved.

## Remaining work

Container/VM backend, runner image build and pinning, artifact export rules, secret
injection interface, cancellation that terminates the process tree, and a hostile
test suite covering escape attempts, symlink tricks, fork bombs and output floods.
