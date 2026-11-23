# Phase 4 — IDE experience

Status: **in progress**. This file distinguishes existing alpha components from the remaining work; it is not a release declaration.

## Outcome

Ryoko should feel like an excellent editor even when no model is connected. Code navigation, reviewing diffs, agent collaboration, session recovery and debugging must be predictable, keyboard-accessible and safe by default.

| Workstream | Available on main before Phase 4 | Phase 4 first increment | Still required for phase exit |
| --- | --- | --- | --- |
| Navigation | Workspace tree/filter, search/replace, Monaco/LSP | Cmd/Ctrl+P fuzzy quick-open with path ranking, keyboard controls and workspace-scoped recents; repeatable 10k quick-open latency harness with recorded before/after evidence ([performance/QUICK_OPEN_BENCHMARK.md](performance/QUICK_OPEN_BENCHMARK.md)) | Symbol/workspace search navigation, jump-back/forward, benchmark budgets certified on real macOS/Windows/Linux hardware |
| Difference review | Monaco side-by-side diff; staged hunk actions and validation gates | No change to merge permissions or review semantics in this increment | Inline/side-by-side toggle, clear stage/unstage/restore, review comments linked to exact lines, binary/rename handling, stale-base detection |
| Inline agent interaction | Cmd/Ctrl+K inline-edit request, proposed changes and acceptance | Preserve the existing explicit acceptance boundary | Context scope UI, progress/cancel, targeted edits, rebase-safe preview and undo with no silent overwrites |
| Persistent sessions | SQLite-backed worktree leases; read-only Active/Published panel | Recent-file navigation remembered per workspace (not dirty-buffer persistence) | Restore editor tabs/layout safely, crash recovery for unsaved edits, session resume with verified process state and consent, cross-version migrations |
| Terminal and debug | Explicit PTY shell; user-selected DAP adapter | Incremental UTF-8 PTY output, process-exit status reconciliation, bounded DAP stderr, session-scoped events, stop on workspace switch, DAP stack/variables/breakpoints inspector, restart lifecycle | Multiple PTYs, cross-platform stress tests |

## Mandatory engineering rules

- **No unsupervised execution.** Inline agent results and terminal/debug commands remain subject to existing user authorization. Never treat chat content or diff text as commands.
- **No invisible Git mutation.** Stage, reject, merge and revision actions must show their exact scope. Require verification after a changed base; refuse stale or ambiguous hunks.
- **No misleading session claim.** Persisting a lease does not mean an interrupted model task or terminal process is resumable. Recovery must detect stale processes and offer explicit recovery choices.
- **No optimistic pass state.** Report build/test success only when a runner confirms it. Failed, skipped and not-run are distinct.
- **No expensive per-keystroke filesystem scans.** Quick-open operates over the already-loaded workspace file list; future indexing must be incremental, cancellable, symlink-safe and bounded.
- **No untrusted adapter output in HTML.** All terminal/debug and agent output must remain text-only, size-bounded and session-scoped.

## Acceptance gates

1. **Fast navigation:** search 10,000 pre-indexed paths without blocking the UI; measure p50/p95 key-to-results on target hardware. Query ranking is deterministic, with tests for exact filenames, fuzzy subsequences, recents and empty results. A repeatable 10k harness and recorded Linux evidence now exist ([performance/QUICK_OPEN_BENCHMARK.md](performance/QUICK_OPEN_BENCHMARK.md)); the measurement is still evidence rather than a CI threshold until it is repeated on real target hardware.
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

### DAP Inspector manual smoke checklist
1. **Adapter start & initialize:** Enter `python3` with args `tests/fixtures/fake_dap_adapter.py`, click Start. Verify status updates to "Adapter initialized" and execution controls (Continue, Next, Step in, Step out) are available.
2. **Breakpoints:** Toggle breakpoint on current line via F9 or button. Verify breakpoint appears in the Breakpoints list with file, line, and verified badge. Clicking breakpoint navigates editor to file:line.
3. **Call Stack navigation:** When paused, verify paused stack frames populate the Call Stack list with function name and `source:line:col`. Selecting frames with arrow keys / Enter navigates the editor and loads frame scopes.
4. **Variables inspection:** Verify local variables appear in the Variables tree. Expanding nested variables (`variablesReference > 0`) fetches child properties without freezing UI.
5. **Restart & Stop:** Click Restart; verify previous session terminates cleanly and new session starts without duplicate logs or leaked child processes. Click Stop; verify controls reset to idle state and "Not paused" is displayed.
6. **Workspace switch:** Switch workspace while debugging; verify active adapter process is killed, all session frames/variables are cleared, and no stale data persists.
7. **Resilience:** Provide invalid adapter command or non-existent path; verify bounded error message appears in status/output without hanging the webview.

## Recommended sequencing

1. Navigation and terminal/debug lifecycle hardening (current increment).
2. Complete diff review UX and protected review history.
3. Inline agent workflow with cancellation, provenance and conflict-aware previews.
4. Durable editor state and opt-in recovery/resume semantics.
5. DAP feature parity, multi-session PTY and measured performance/accessibility release gates.

## Workbench chrome

The desktop workbench follows familiar editor conventions. Every action below
is a registered command (`apps/desktop/ui/src/commands.ts`), so the command
palette, menus and keybindings all run the same code.

| Feature | How to use it | Where it lives |
| --- | --- | --- |
| Command palette | `Ctrl+Shift+P` / `⇧⌘P`. Fuzzy search over every command, with its shortcut. Recent commands are listed first. | `command_palette.ts` |
| Menu bar | File / Edit / View / Run / Help. In-window on Windows and Linux, native on macOS. | `menu_bar.ts`, `src-tauri/src/menu.rs` |
| Resizable panes | Drag the edges of the side bar, the Mission Control side bar and the bottom panel. Arrow keys resize a focused edge. Double-click resets it. Dragging the side bar very narrow collapses it; `Ctrl+B` toggles it and `Ctrl+J` toggles the panel. | `layout_state.ts` |
| File tree | Folders with expand arrows; files and tabs show a file-type badge. Git changes are coloured and marked A / M / D. | `file_icons.ts`, `tree.ts` |
| Breadcrumbs and minimap | The path of the active file and the symbols around the cursor appear above the editor. Click a symbol to jump to it. View › Toggle Minimap. | `breadcrumbs.ts` |
| Colour themes | Dark, Light and High Contrast, from View › Color Theme, the palette (`Theme:`) or Settings. Monaco's built-in theme changes with the chrome. | `themes.ts` |

Sizes, the collapsed side bar, the minimap setting and the theme are stored in
`localStorage` under `ryoko.layout.v1` and `ryoko.theme.v1`. Stored values are
clamped or replaced with defaults when invalid.

Known limitations:

- **Breadcrumb symbols need a language server.** They come from
  `textDocument/documentSymbol`. Without a running server for the file's
  language, breadcrumbs show only the path.
- **macOS native menu.** On macOS the renderer hides its menu bar and Rust
  installs a native one. Its command ids must match `MENUS` in `menu_bar.ts`
  (a Rust test checks this). Native accelerators use the default keybindings,
  so a rebound shortcut is not reflected in the macOS menu. The macOS build is
  compiled only in the macOS CI job.
- The renderer runs a menu event only when its id is a registered command.
  Unknown ids are ignored.
