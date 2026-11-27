# Reproducible desktop build evidence (#45)

Status: pinned input proposal; independent comparisons pending. The preview and production desktop workflows pin the Rust and Node versions observed in successful native integration run [46017581694](https://github.com/Flanxiyum001/Ryoko/actions/runs/46017581694) and pin third-party actions to the resolved commits of their existing release lines. Runner images, apt packages, compiler SDKs and bundler downloads remain mutable; do not claim a hermetic build.

The frontend dependency lock aligns the API and dialog bindings with the Rust versions previously selected by the preview-only install. The exact version/integrity entries were cross-checked in multiple public npm lockfiles; the first CI npm ci must validate the tarball integrity against the registry. No install-time dependency override is needed.

Each preview target publishes a redacted JSON input manifest with the commit, target, runner identity/version (where exposed), tool versions and SHA-256 of the three lockfiles. The release checksum and attestation cover those files. No environment dump, secrets or absolute checkout paths are recorded.

## Independent comparison procedure (not yet executed)

1. Select a reviewed source commit, exact version and target. Use two independently provisioned clean runners with identical image/SDK versions and no shared build cache. Keep signing and updater private keys absent. Record tool versions, lockfile hashes and runner image versions; fail if they differ.
2. Build each target twice using npm ci and locked Cargo inputs. Use the same version/configuration on both runners. Preserve unsigned pre-bundle binaries and frontend dist; archive each payload with a relative-path SHA-256 inventory. Check the lockfiles are unchanged.
3. Compare the inventories, and inspect any differing binaries before attempting normalization. For packaging-only comparisons extract installer contents without executing them; compare file names and payload bytes and record file modes separately. Do not delete or mask arbitrary binary differences. Treat timestamps, archive ordering, path metadata and signing/notarization envelopes as separate observed sources of nondeterminism, not automatic excuses.
4. Record target, source SHA, both workflow/run IDs, runner/tool metadata, raw and normalized inventories, exact normalization steps and remaining differences. A mismatch or missing comparison remains an open gate. Signed installers are outside the unsigned-payload byte-equivalence claim.

Gate B remains open until this evidence exists for Windows x64, macOS Apple Silicon and Linux x64. See [Phase 5](../PHASE_5_RELEASE_TRUST.md).
