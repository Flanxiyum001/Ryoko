# Phase 4 — IDE experience

Status: **in progress**. This file distinguishes existing alpha components from the remaining work; it is not a release declaration.

## Outcome

Ryoko should feel like an excellent editor even when no model is connected. Code navigation, reviewing diffs, agent collaboration, session recovery and debugging must be predictable, keyboard-accessible and safe by default.

| Workstream | Available on main before Phase 4 | Phase 4 first increment | Still required for phase exit |
| --- | --- | --- | --- |
| Navigation | Workspace tree/filter, search/replace, Monaco/LSP | Cmd/Ctrl+P fuzzy quick-open with path ranking, keyboard controls and workspace-scoped recents | Symbol/workspace search navigation, jump-back/forward, quick-open performance benchmarks |
| Difference review | Monaco side-by-side diff; staged hunk actions and validation gates | No change to merge permissions; every staged hunk is re-verified against its reviewed file identity and base, stale selections fail closed per file, and renamed, copied, binary, deleted, mode-only and submodule changes surface as explicit non-editable review states | Inline/side-by-side toggle, clear stage/unstage/restore, review comments linked to exact lines |
| Inline agent interaction | Cmd/Ctrl+K inline-edit request, proposed changes and acceptance | Preserve the existing explicit acceptance boundary | Context scope UI, progress/cancel, targeted edits, rebase-safe preview and undo with no silent overwrites |
| Persistent sessions | SQLite-backed worktree leases; read-only Active/Published panel | Recent-file navigation remembered per workspace (not dirty-buffer persistence) | Restore editor tabs/layout safely, crash recovery for unsaved edits, session resume with verified process state and consent, cross-version migrations |
| Terminal and debug | Explicit PTY shell; user-selected DAP adapter | Incremental UTF-8 PTY output, process-exit status reconciliation, bounded DAP stderr, session-scoped events, stop on workspace switch | Multiple PTYs, robust session UX, DAP stack/variables/breakpoints UI, cross-platform stress tests |

## Mandatory engineering rules

- **No unsupervised execution.** Inline agent results and terminal/debug commands remain subject to existing user authorization. Never treat chat content or diff text as commands.
- **No invisible Git mutation.** Stage, reject, merge and revision actions must show their exact scope. Require verification after a changed base; refuse stale or ambiguous hunks.
- **No misleading session claim.** Persisting a lease does not mean an interrupted model task or terminal process is resumable. Recovery must detect stale processes and offer explicit recovery choices.
- **No optimistic pass state.** Report build/test success only when a runner confirms it. Failed, skipped and not-run are distinct.
- **No expensive per-keystroke filesystem scans.** Quick-open operates over the already-loaded workspace file list; future indexing must be incremental, cancellable, symlink-safe and bounded.
- **No untrusted adapter output in HTML.** All terminal/debug and agent output must remain text-only, size-bounded and session-scoped.

## Acceptance gates

1. **Fast navigation:** search 10,000 pre-indexed paths without blocking the UI; measure p50/p95 key-to-results on target hardware. Query ranking is deterministic, with tests for exact filenames, fuzzy subsequences, recents and empty results. The first increment includes ranking tests, not a certified 10k-file benchmark.
2. **Diff review:** selected hunks apply only to intended paths and expected base; stale edits fail closed; validation is rerun before integration. The diff preview must visibly distinguish tested from untested changes.
3. **Inline agents:** unchanged buffers are not overwritten silently; every edit is previewable, cancellable, and undoable; failures preserve the original document.
4. **Session continuity:** restart with dirty and clean editors, active/published worktrees and interrupted tasks. Never silently resume execution after a crash or update. Capture, restore and migration behavior must have regression tests.
5. **Terminal/debug:** round-trip Unicode across arbitrary PTY read boundaries; start/stop/restart and switch workspaces without leaking old output; handle dead adapters, oversized stderr and malformed DAP frames without freezing; test Windows, macOS and Linux.

## CI and manual verification

On each Phase 4 PR:

```sh
cd apps/desktop
npm ci
npm test
npm run build
cd ../..
cargo fmt --all -- --check
cargo test --workspace --locked
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo fmt --manifest-path apps/desktop/src-tauri/Cargo.toml -- --check
cargo test --manifest-path apps/desktop/src-tauri/Cargo.toml --locked
```

The Tauri tests also require platform-native prerequisites. After CI, run interactive smoke tests with a disposable workspace and a user-selected debug adapter on each target OS. Passing CI does **not** substitute for interactive manual verification.

## Recommended sequencing

1. Navigation and terminal/debug lifecycle hardening (current first increment).
2. Complete diff review UX and protected review history.
3. Inline agent workflow with cancellation, provenance and conflict-aware previews.
4. Durable editor state and opt-in recovery/resume semantics.
5. DAP feature parity, multi-session PTY and measured performance/accessibility release gates.
