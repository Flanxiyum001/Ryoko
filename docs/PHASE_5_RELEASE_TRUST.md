# Phase 5 — Release Security, Integrity and Crash Recovery

Status: **implementation in progress**. This document records release gates and gaps; it is not a certification that Ryoko is production-ready.

## Baseline verified from the repository

- The public `.github/workflows/desktop-preview.yml` currently builds **unsigned** installers for Windows x64, macOS Apple Silicon and Linux x64; it publishes prereleases and SHA-256 sums. A checksum delivered by the same publisher is a corruption check, **not** proof of publisher identity.
- `.github/workflows/desktop.yml` already requires signing material for macOS and Windows and verifies Windows Authenticode and macOS code signatures. Production credentials, correct notarization/stapling, installed-app launch and user trust checks must still be validated in real release runs.
- The marketplace verifies a registry-supplied SHA-256 before writing artifacts, but registry metadata alone does **not** authenticate a third-party plugin publisher. The desktop marketplace surface is browse-only.
- `ryoko-storage` already has a durable task journal with conservative, read-only recovery; that does **not** recover unsaved editor buffers or automatically make interrupted agent work safe to replay.
- **Windows WASM guardrail:** native Wasmtime 26 compiled-code guest traps previously aborted the host process (`0xc0000409`). Windows now selects the portable Pulley interpreter while retaining fuel, epoch timeout, memory and test-evidence checks; initialization failures reject validation instead of falling back to the native JIT. Native Unix platforms keep their original backend. Cross-platform unit tests now exercise actual WASM execution, trap rejection and WASI imports on Windows, but this is not yet an out-of-process security boundary, production security certification, or desktop installer acceptance test.
- Existing CI checks Rust and Tauri mainly on Linux. The Phase 5 platform workflow adds native Windows/macOS/Linux tests but **does not** test installer UX or GUI automation.

## Gate A: release signing and provenance

1. Keep unsigned previews unmistakably labeled and separate from signed production tags.
2. For each production release, sign Windows installers using a controlled code-signing certificate; sign and notarize macOS application bundles, staple tickets where applicable, and verify on clean machines with OS security controls enabled. Never weaken OS security to make installers pass.
3. Attest the exact distributed assets, record SHA-256 digests, source commit, workflow identity, Rust and Node versions, and dependency lockfiles. Verify `gh attestation verify FILE --repo Flanxiyum001/Ryoko` against the downloaded file and expected source repository. Build provenance does **not** replace platform signing.
4. Require an explicit manual release approval, a protected release environment, least-privilege tokens, and an immutable published version. Do not grant signing secrets to pull-request builds.
5. Test deliberate tampering: changed installer, wrong repository attestation, expired/revoked signer, absent notarization and missing artifact must fail closed.
6. Do not enable an automatic updater until update metadata and binary payload signatures are verified, key rotation/rollback protections exist, and updater failures leave the last known-good version installed.

**Exit:** signed, notarized where required, attributable and verified installers on supported OSes, plus a documented rollback drill. The existing production workflow is groundwork, not evidence that this gate has passed.

## Gate B: reproducible, traceable builds

1. Pin Rust toolchain and GitHub Actions to reviewed revisions; keep Cargo and npm lockfiles committed, require `--locked` / `npm ci`, and eliminate mutable `npm install --no-save --package-lock=false` from the preview pipeline before claiming reproducibility.
2. Record an environment/build manifest for each platform: source SHA, toolchain version, runner image, lockfile digests, SDK versions and build arguments.
3. Run two independent unsigned-payload builds from the same source and toolchain, normalize variable metadata/paths/timestamps, and compare deterministic outputs or explain unavoidable differences.
4. Keep code-signing timestamps, platform notarization and installer packaging metadata **outside** byte-for-byte reproducibility claims unless independently demonstrated. A list of SHA-256 hashes alone is not a reproducibility test.

**Exit:** repeatable documented builds and independently verified comparisons for each supported target. Current builds have not met this exit gate.

## Gate C: verified plugin publisher identity

The registry is **not** a source of publisher trust. Define canonical signed metadata binding `publisher_id`, `plugin_id`, `version`, `artifact_sha256`, `capabilities`, `expiry` and a versioned verification policy. Use a reviewed signature format (for example Ed25519 with an explicit trusted-key registry, or verified Sigstore identity); do not implement ad-hoc cryptography.

- Verify the signature against a locally trusted publisher key/identity, and bind it to the exact downloaded bytes **before** installation or execution.
- Pin publisher identity per plugin ID; reject a later artifact claiming the same ID under a different identity unless the user approves a verified transfer.
- Enforce expiry, revoked keys, monotonic update versions, artifact limits, and explicit capability consent. Refuse unsigned/malformed/unknown-publisher packages by default.
- Persist verified publisher fingerprint, artifact digest, permissions and verification result to the installation lock. Do not accept a registry-provided 'verified' boolean.
- Test altered manifest, altered bytes, key substitution, revoked/expired cert, rollback, impersonation and compromised registry index.
- Keep the desktop installation affordance disabled until the verifier and trust store are wired end-to-end.

**Exit:** hostile registry metadata cannot cause arbitrary unauthenticated package execution.

## Gate D: crash recovery without unsafe replay

- Save opt-in, bounded, per-workspace editor recovery snapshots in an application-owned directory using atomic writes and durability syncs; do not put plaintext project contents in telemetry, logs or cloud storage.
- Snapshot identities must include workspace, file URI, base content hash, editor revision, session ID and timestamps. Restore by **diff/preview and explicit consent**, never blindly overwrite changed disk files.
- On clean exit discard obsolete snapshots; on abnormal exit offer recovery. Recover only non-secret material under an explicit retention/privacy policy; encrypt at rest where supported and handle unavailable keys transparently.
- Replay durable agent records in **read-only** mode first. Revalidate branch/worktree identity, lock leases, approvals and last validated SHA before permitting a resumed tool call. Never silently retry external writes or purchases.
- Crash-injection tests cover editor keystrokes, mid-atomic-write, interrupted validation, agent cancellation, corrupted snapshots and concurrent workspaces.

**Exit:** a forced termination cannot silently lose a recoverable edit or apply a stale agent change.

## Gate E: cross-platform integration

The new `.github/workflows/platform-integration.yml` runs native Rust workspace tests, desktop frontend tests/build, the CLI smoke test and Tauri shell check/tests on GitHub-hosted Windows, macOS and Linux. It produces **test coverage**, not automatic OS installer certification.

Before a public beta, also require:
- Native installer install/uninstall and launch on clean Windows 10/11 x64, supported macOS Apple Silicon, and representative Linux distributions.
- File create/edit/save/reload, Unicode PTY, DAP start/stop, LSP lifecycle, Git hunk review, permission dialogs, multi-agent cancellation/restart and recovery.
- No leaked secrets in logs or crash reports. Disconnected/offline modes must be tested.
- Expected-failure and recovery scenarios; OS code-signature and attestation checks.
- Required branch protection on both primary CI and cross-platform jobs; no release if any required platform fails.

## PR policy and implementation order

- **5A:** native cross-platform test matrix + unsigned-preview build provenance (initial infrastructure increment).
- **5B:** deterministic build inputs, independent comparison and signed-production release verification.
- **5C:** plugin publisher signature enforcement and revocation (security review required).
- **5D:** editor snapshots and session recovery with crash/fault-injection tests.
- **5E:** installer/GUI end-to-end tests, staged updates and rollback.

Avoid bundling cryptographic policy, editor persistence and signing-secret changes into one PR. Every claim must be backed by green CI plus the platform-specific manual evidence listed above.
