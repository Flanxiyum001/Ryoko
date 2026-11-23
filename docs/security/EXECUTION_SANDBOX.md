# Execution sandbox

Status: Ryoko has two independent execution paths: WASM artifact validation
with no host preopens (in-process on Unix, isolated worker process on Windows),
and a Docker/Podman **container-backed** project pre-flight runner. Neither
path silently falls back to unguarded host execution: the project runner fails
closed if the operator has not selected an image or an available runtime, and
the Windows guardrail fails closed if its worker binary cannot be resolved.

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
dependencies outside of untrusted validation. `ryoko sandbox init` can generate
and build a starting image from `Cargo.toml`, `package.json` or `pyproject.toml`;
it writes `.ryoko/sandbox/Dockerfile` and `Dockerfile.dockerignore`, caches
Cargo/npm/Python dependencies during the build, and sets the runtime to offline
mode. The generated Dockerfile is a convenience, not a reviewed trust anchor:
inspect it, pin a digest, and confirm the dependency install step before using
it for untrusted code. The build itself needs network access; validation still
uses `--network=none` and `--pull=never`. Mixed-language repositories use one
detected ecosystem (Rust, then Node, then Python) in the generated image.

### Rebuilding when dependencies change

A successful `ryoko sandbox init` records a fingerprint next to the Dockerfile
(`.ryoko/sandbox/Dockerfile.state.json`). It is a SHA-256 over the image tag,
the generated Dockerfile and the dependency inputs of the detected ecosystem:
every `Cargo.toml`, `Cargo.lock` and `rust-toolchain(.toml)` in the workspace
(bounded walk, skipping `.git`, `target`, `node_modules`, `dist` and `.ryoko`);
`package.json`, `package-lock.json` and `npm-shrinkwrap.json`; or
`pyproject.toml`, `poetry.lock`, `uv.lock`, `requirements.txt`, `setup.cfg`
and `setup.py`. Only file bytes are read and symlinks are never followed.

- `ryoko sandbox status [--json]` reports the runtime (Docker, then Podman;
  never the host), whether the image is in the local store, and which inputs
  changed since the last build. Nothing is pulled or built.
- `ryoko sandbox init --if-stale` skips the build when the image is present and
  the fingerprint matches, and rebuilds otherwise.

The fingerprint is a staleness hint, not an attestation: it does not prove the
image in the store was built from these inputs, and a failed build never
records one. Rebuilds are explicit; nothing watches the repository in the
background.

For reproducibility, pin the exact image digest and project lockfiles. Absent
dependencies make the check fail; the pre-flight runner never enables network
access to fetch them.

Container execution now requires a full local `sha256:<64 hex>` image ID or a
repository reference ending in `@sha256:<64 hex>`. The runner inspects the local
image immediately before execution, verifies the ID or repository digest and
passes the pinned reference to `run --pull=never`. `sandbox init --json` reports
the local image ID as `image` after a successful build and keeps the mutable
tag separately as `image_tag`. `--no-build` reports the tag only; it cannot be
used to run a check until the operator resolves a local image ID.

Git worktree proposals use capability-relative temporary writes and atomic
rename. The orchestrator seals a Git tree, checks out that commit into a
separate validation worktree, rejects changes to its tracked source while
tests run, and publishes only the sealed commit. Changes to the agent's
worktree cannot affect validation or the published bytes. Generated untracked
test artifacts are excluded from the sealed tree. Another process with access
to the validation checkout could still change and restore a file during a
check; a read-only mount plus separate writable build overlay is required to
exclude that adversary.

## Current guardrail

- Fresh wasmtime store per artifact.
- No host preopens, no inherited environment, no network.
- Fuel bounds CPU; epoch deadline bounds wall time; `StoreLimits` bounds linear
  memory and tables.
- Only a clean `_start` return counts as a pass.
- `SandboxSpecValidator` refuses proposals without tests.

## Isolated Windows worker process

Wasmtime 26 guest traps can abort the process hosting them on Windows
(`0xc0000409`), so Windows never evaluates guardrail artifacts in-process. The
host spawns the dedicated `wasm-guardrail-worker` binary next to the
application executable (or at `RYOKO_WASM_WORKER_BIN`) for every evaluation:

- One typed JSON request on stdin carries only the artifact bytes and the
  enforced fuel, timeout and memory limits; one typed JSON response on stdout
  carries the outcome. No paths, environment or credentials cross the
  boundary.
- The worker is spawned with a cleared environment (`env_clear`), only the
  three standard streams piped and `kill_on_drop` armed. Empty WASI state
  means no preopens, no inherited environment and no network at runtime.
- The host enforces the wall-clock deadline itself (sandbox timeout plus a
  small protocol grace) and kills plus reaps the worker on every exit path,
  including future cancellation.
- A fresh worker is spawned per evaluation; there is no worker pool and no
  implicit reuse of stale processes.
- A guest trap can abort the worker; the host then observes EOF without a
  response and reports a bounded failed validation. A missing or crashing
  worker binary fails closed exactly the same way; there is never a fallback
  to unguarded host execution.
- Unix hosts keep in-process evaluation; `WasmSandbox::new_with_worker` opts
  any platform into the worker path explicitly (used by the
  `wasm_worker_process` regression suite).

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

VM backend, image digest pinning and attestation, mixed-language image
generation, artifact export rules, secret injection interface, cancellation that
terminates the process tree, and a hostile test suite covering escape attempts,
symlink tricks, fork bombs and output floods.

## Container scratch space and artefact mounts

Chromium inside a hardened container needs areas that are writable even though
the root filesystem is not, and it needs enough processes:

| Flag | Why |
| --- | --- |
| `--tmpfs /tmp:rw,noexec,nosuid,size=512m` | browsers and test runners write temp files; `noexec` keeps that space useless for dropped payloads |
| `--shm-size=512m` with `--ipc=private` | renderers share memory through `/dev/shm`; the 64 MiB default makes them crash. `--ipc=none` removes `/dev/shm` entirely, so it is no longer used |
| `--pids-limit=256` | a browser launch forks renderer, GPU and utility processes; 128 was too few |
| `--mount …,target=/ryoko-artifacts` | screenshots, traces and reports, in their own bind mount |

The artefact mount is the only writable path besides `/tmp`; the worktree mount
stays read-only. Its host directory is `.ryoko/artifacts/<task id>/`, and the
sandbox refuses an artefacts directory that sits inside the worktree being
checked, so a screenshot can never appear in `git status` or a staged diff. The
generated build context ignores `.ryoko/artifacts` for the same reason.

## Browsers in the generated image

`ryoko sandbox init` reads `package.json` (dependencies, devDependencies and
peerDependencies) for React, Next.js, Vite, Svelte, Vue, Tailwind and Playwright.
When any of them is declared, the browser layers are added to the generated
Dockerfile, because a validation container has `--network=none` and cannot fetch
a browser afterwards:

- `ENV PLAYWRIGHT_BROWSERS_PATH=/ms-playwright` — a fixed global path readable by
  any unprivileged container user, rather than `$HOME/.cache`;
- the minimal headless libraries and fonts (`libnss3`, `libatk1.0-0`,
  `libatk-bridge2.0-0`, `libgbm1`, `libasound2`, `fonts-liberation`), so text
  rendering and layout metrics stay stable between runs;
- `npx playwright install --with-deps chromium` followed by
  `chmod -R o+rx /ms-playwright`.

When Playwright is a project dependency, the project's own version installs the
browser, so the browser build matches the test runner. When it is not, the runner
is installed under `/opt/playwright` and the project's dependency tree is left
untouched.

## UI verification inside the sandbox

Frontend images also carry the bundled verification harness at
`/opt/ryoko/verify-ui.mjs` (see [UI_VERIFICATION.md](../architecture/UI_VERIFICATION.md)).
It is copied from `.ryoko/sandbox/verify-ui.mjs` during the image build and needs
no network: it polls the project's own dev server on loopback, captures
screenshots, an accessibility tree and a runtime error log into
`/ryoko-artifacts`, then kills the server's whole process group.
