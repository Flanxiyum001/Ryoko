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
