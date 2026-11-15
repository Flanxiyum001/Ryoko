# Execution sandbox

Status: Ryoko has two independent execution paths: WASM artifact validation
with no host preopens, and a Docker/Podman **container-backed** project
pre-flight runner. The project runner fails closed if the operator has not
selected an image or an available runtime. There is no automatic host fallback.

## Container-backed pre-flight checks

An operator supplies an offline-ready image via `--sandbox-image` and can select
`--sandbox-backend auto|docker|podman`. Ryoko invokes the runtime without
shell interpretation of runtime arguments. The untrusted test command itself
runs in `/bin/sh -c` **inside** the container. The runtime does not pull images.

- Only the canonical task worktree is bind-mounted, at `/workspace`, with write access
  so compilers can emit local build outputs.
- No host home directory, SSH agent, Docker socket or credential environment
  variables are passed through.
- Container networking is disabled; the container root is read-only.
- All Linux capabilities are dropped and privilege escalation is disabled.
- PID, memory, CPU, output and timeout limits are applied.
- The container is named per attempt and forcibly removed after the check.
- On Unix, the container process uses the invoking user's UID and GID.
- `--unsafe-host-check` is an explicit escape hatch, intended only for trusted
  repositories. It is never selected by backend auto-discovery.

**Security boundary and limitations:** This is an initial OCI isolation backend,
not a disposable VM. Docker/Podman daemon and image supply-chain trust remain
critical. Worktree bind mounts are writable and not immutable snapshots;
there is no image digest attestation, disk quota, automatic dependency cache,
per-task network allowlist or isolated dependency-audit runner yet.
Windows Docker Desktop bind-mount permissions may require image-specific
configuration. There is no protection against another host process changing
the worktree while validation runs. Treat cross-tenant malicious builds as
requiring a stronger VM-based runner.

Prepare an image with all required Cargo/npm/Python/Go compilers and offline
dependencies outside of untrusted validation. For reproducibility, pin the
exact image digest and project lockfiles. Absent dependencies make the check
fail; the pre-flight runner never enables network access to fetch them.

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

## Pre-merge audits

Worktree acceptance secret-scans every proposed file (matched values are
redacted) and runs `cargo audit` and `npm audit` when the matching manifest,
lockfile and tool are present. Findings fail the validation report and therefore
block merge. Missing audit tools are recorded as skipped, not as clean. These audit tool invocations are still host-side and may use network access;
use a trusted repository or arrange to run them separately in controlled CI.

## Remaining work

VM backend, runner image build and pinning, artifact export rules, secret
injection interface, cancellation that terminates the process tree, and a hostile
test suite covering escape attempts, symlink tricks, fork bombs and output floods.
