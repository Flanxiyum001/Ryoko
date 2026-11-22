# Release criteria

A release is defined by evidence, not by features existing in a branch. Gates are
cumulative: a later release must still satisfy earlier gates.

## Universal gates

| Gate | Criterion | Evidence required |
| --- | --- | --- |
| Build | Workspace builds with `--locked` on all target platforms | CI matrix logs |
| Format | `cargo fmt --all -- --check` passes | CI log |
| Lint | `cargo clippy --workspace --all-targets --locked -- -D warnings` passes | CI log |
| Tests | `cargo test --workspace --locked` passes | CI log with counts |
| Docs | Documented contracts match behaviour | Review of affected docs |
| Secrets | No credentials in tree, logs or fixtures | Secret scan |
| License | Dependency licences are compatible | Licence report |

## Product gates

| Gate | Criterion |
| --- | --- |
| Native IDE | Installs and launches on Windows, macOS and Linux |
| Professional editor | Tabs, splits, intelligence, search and diagnostics work on a real project |
| Developer tooling | Git, terminal, LSP and debugging foundations available |
| Provider independence | Multiple native adapters plus local models, each with a connection test |
| Single-agent engineering | Can modify and validate a real project with reviewable evidence |
| Multi-agent orchestration | Coordinated tasks with durable state and no silent overwrites |
| Context intelligence | Retrieval returns cited, current file and symbol references |
| Human control | Every integration requires an inspectable diff and an explicit approval |
| Security | Scoped permissions and restricted execution enforced by the host |
| Reliability | Cancellation, recovery and structured error reporting |
| AI evaluation | Reproducible benchmark suite with recorded results |
| Distribution | Signed, tested, versioned installers and a rollback path |

## Performance targets

These are targets to measure, not current results.

| Metric | Target |
| --- | --- |
| Cold start | under 3 s |
| Keystroke-to-render | under 50 ms |
| File navigation | under 200 ms |
| Incremental index update | under 2 s for an ordinary edit |
| UI frame rate | 60 FPS under normal load |
| Cancellation acknowledgement | under 2 s |
| Crash recovery | restores last saved workspace state |

Measure Ryoko overhead separately from provider latency. Report the machine and
project used for each measurement.

## AI quality metrics

Task success rate, acceptance-test pass rate, regression rate, security finding
precision and recall, human correction rate, cost per successful task, retries,
retrieval precision and the share of results carrying reproducible evidence. Line
count is not a quality metric.

## Security release blocker

Release is blocked by any open critical finding, any failing security test, an
unverifiable attestation path, or a documented boundary without a regression test.

## Release process

1. Freeze scope; update `docs/` for every contract change.
2. Run the full matrix and security suite; attach results to the release record.
3. Publish notes listing limitations without hedging.
4. Stage to beta, monitor, then promote with explicit approval.
5. Keep the previous signed artifact available for rollback.

No merge, publish, deploy or release step happens automatically from an agent
proposal.


## Desktop installer signing gate (workflow-ready, credentials not supplied)

The \`Desktop installers\` Actions workflow now requires real platform signing
material and refuses to emit macOS or Windows installer artifacts if credentials
are missing. This is a release gate; it is **not evidence that a certificate
has been installed or that installers have been signed**.

Configure GitHub Actions secrets using the repository's encrypted Secrets and
Variables controls. Do **not** add certificates/passwords to source files:

| Platform | Secret | Meaning |
| --- | --- | --- |
| macOS | \`APPLE_CERTIFICATE\` | Base64 Developer ID Application \`.p12\` |
| macOS | \`APPLE_CERTIFICATE_PASSWORD\` | P12 export password |
| macOS | \`APPLE_SIGNING_IDENTITY\` | Verified Developer ID signing identity |
| macOS | \`APPLE_ID\` | Notarization Apple ID |
| macOS | \`APPLE_PASSWORD\` | App-specific Apple notarization password |
| macOS | \`APPLE_TEAM_ID\` | Apple developer team |
| macOS | \`KEYCHAIN_PASSWORD\` | Ephemeral runner keychain password |
| Windows | \`WINDOWS_CERTIFICATE\` | Base64 signing \`.pfx\` |
| Windows | \`WINDOWS_CERTIFICATE_PASSWORD\` | PFX export password |

On macOS the workflow imports the signing certificate into a temporary
keychain, invokes Tauri with notarization credentials and requires both
\`codesign --verify\` and Gatekeeper \`spctl --assess\` to pass. On Windows it
imports the PFX, constructs an ephemeral Tauri configuration containing the
certificate thumbprint, signs installers and verifies Authenticode on the MSI
and EXE outputs. The build stops instead of publishing unsigned artifacts.

**Before triggering a release:** protect the signing credentials using a
GitHub Environment with required reviewers, validate the requested
\`desktop-v*\` tag/version and run the installer build matrix on actual Windows
and macOS runners. A certificate does not guarantee the absence of all
Windows SmartScreen warnings (publisher reputation still matters).

**Updater not enabled yet.** Tauri's updater requires an operator-generated
public update-signing key and controlled HTTPS update feed, plus compatible
dependency lockfiles and runtime wiring. Never bundle a placeholder public key
or generate update signatures with unrelated ephemeral keys. Configure and
test key rotation, signed manifests and rollback before enabling auto-update.
