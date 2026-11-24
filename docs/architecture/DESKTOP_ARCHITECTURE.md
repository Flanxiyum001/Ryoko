# Desktop architecture

Status: the Tauri foundation exists. `apps/desktop` now contains a native
`src-tauri` shell and a Vite/TypeScript frontend under `ui/`. The pre-existing
browser workbench (`index.html`, `app.js`, `styles.css`) is still served by the
GitHub App broker and remains separate from the native app.

## Shape

```text
apps/desktop/
  ui/                 Vite + TypeScript UI (Monaco editor)
    src/              editor wiring, LSP client, protocol helpers
  src-tauri/          Rust application shell
    src/              command handlers, workspace I/O, LSP process relay
    capabilities/     Tauri capability manifests
    binaries/         generated ryoko-lsp sidecars (not committed)
    icons/            bundle icons
```

`Cargo.toml` excludes `apps/desktop/src-tauri` from the root workspace, so the
desktop shell builds independently while the sidecar reuses the domain crates.

## Implemented foundation

| Part | Owns |
| --- | --- |
| `src-tauri` | Window lifetime, workspace file commands, `ryoko-lsp` process lifecycle and framing, capability scopes |
| `ui/` | Monaco models, explorer, editor keybindings, LSP JSON-RPC client, problems view |

### Commands

```text
open_workspace(path)            canonicalises and stores the workspace root
list_files()                    bounded, gitignore-aware file list
read_file(path)                 UTF-8, max 2 MiB, canonical path inside the root
write_file(path, content)       atomic save, max 2 MiB, refuses symlink escapes
lsp_start(root)                 spawns ryoko-lsp plus registered language servers
lsp_send(server, message)       validates JSON, writes one Content-Length frame
lsp_stop(server?)               terminates one or all language servers
lsp_registry()                  validated registry for the webview multiplexer
git_changes()                   bounded NUL-separated Git working-tree changes
workspace_search(...)           bounded regex search over the open workspace
workspace_replace_preview(...)  dry-run replace plans with drift markers
workspace_replace_apply(...)    applies only what the preview showed
inline_edit(args)               one targeted model edit for the active selection
settings_read() / settings_save_providers(json)        visual settings surface
settings_save_lsp_servers(json) validated language-server registry
workbench_protocol_version()    lets the webview feature-detect the surface
list_mission_logs()             bounded event-log names
read_mission_log(name)          bounded JSONL contents
list_review_hunks()             unstaged Git text hunks
stage_review_hunks(ids, patch)  stage selected hunks after native approval
reject_review_hunks(ids, patch) revert selected hunks after native approval
read_review_file(path)          committed and working copies for the diff editor
sandbox_init()                  run `ryoko sandbox init --json` in the workspace
start_run(goal, settings)       spawn ryoko run with an explicit argv
run_status()                    running/paused/exit state and bounded output tail
pause_run() / resume_run()      process-group stop/continue (Unix only)
cancel_run()                    terminate the run process group
fim_complete(prefix, suffix, id) stream loopback-only FIM ghost text
fim_cancel()                    invalidate in-flight completion generations
terminal_start(shell?)          explicit PTY shell in the open workspace
terminal_write(data)            write input to the PTY
terminal_resize(rows, cols)     resize the PTY and xterm viewport
terminal_status()               running state and session id
terminal_stop()                 terminate the shell
dap_start(adapter, args)       spawn a user-selected DAP adapter over stdio
dap_restart(adapter, args)     terminate active adapter and spawn a fresh session
pick_workspace_folder(title?)  native folder picker; on Linux a modal GTK
                              chooser made transient for the main window
dap_send(message)              validate JSON and write one framed DAP request
dap_status() / dap_stop()      inspect or terminate the adapter
```

`lsp_start` resolves `ryoko-lsp` from `RYOKO_LSP_BIN`, then a sibling
`ryoko-lsp` binary (the bundled Tauri sidecar), then, in debug builds only, the
nearest ancestor `target/debug` or `target/release` directory, so a workspace
build is found during `tauri dev`; it never falls back to `PATH`. Registered servers
resolve as bare names through `PATH` under the open workspace. Output frames
are parsed in Rust and emitted per server as `lsp-frame` events with an
explicit `{ server, message }` payload; stderr lines become `lsp-log`; process
exit emits `lsp-exit`. The routing rules live in
[`../protocols/DESKTOP_LSP_MULTIPLEX.md`](../protocols/DESKTOP_LSP_MULTIPLEX.md).

The frontend sends `initialize` / `initialized`, `didOpen`, `didChange` and
`didClose`, and turns `textDocument/publishDiagnostics` into Monaco markers and
a problems list. Documents therefore live in `ryoko-vfs` inside the server
process, not in the browser.

### Security

- Workspace commands reject absolute paths, parent traversal, NUL bytes,
  symlink escapes, non-UTF-8 content and files over 2 MiB.
- Saves write a sibling temp file, fsync, then rename (Windows replaces first).
- The capability manifest grants only `core:default` and `dialog:allow-open`;
  there is no shell plugin and no filesystem plugin scope.
- The CSP allows only local scripts/styles, local workers and Tauri IPC.

## Editor workbench

The explorer renders the bounded file list as a collapsible tree with Git
working-tree badges (`.ryoko`-private status kept out of `target` and
`node_modules`), the bottom panel gained a SEARCH tab with regex search and a
two-phase preview/apply replace flow, a CHANGES drawer lists every working-tree
change and opens it in the shared diff editor, the settings dialog edits the
provider catalog, the language-server registry and keybinding overrides
without touching JSON files by hand, and `Mod+K` runs one bounded inline model
edit against the active selection with a drift-checking apply. The IPC
contracts are documented in
[`../protocols/DESKTOP_WORKBENCH.md`](../protocols/DESKTOP_WORKBENCH.md).

## Editor speed and symbol context

`ryoko-models` exposes `FimClient`, which accepts only loopback HTTP(S)
origins, posts prefix/suffix context to Ollama's `/api/generate`, and streams
bounded NDJSON chunks. The desktop emits each chunk as `ryoko-fim-chunk`; the
Monaco content widget appends text and `Tab` accepts it. A new request or cursor
move invalidates the previous generation on both sides. This path has no cloud
credentials and does not use the agent router.

`ryoko-core` owns `WorkspaceIndex`, a SQLite database at
`.ryoko/state/symbols.sqlite3`. `ryoko index` parses up to 2,000 source files
with the existing `ContextExtractor`, skips files over 512 KiB, and atomically
replaces definitions and intra-file call edges. `ryoko run` derives identifier
terms from the goal, refreshes the index, and appends a character-bounded
"Cross-file symbol index" block to task context. The index path rejects
symlinked parent directories and a symlinked database file.

## Integrated terminal and DAP

The bottom panel starts a shell only after an explicit **Start shell** action,
using `portable-pty` with the open workspace as its working directory. Output is
streamed as bounded UTF-8 text to `@xterm/xterm`; input and resize commands are
capped. The shell receives no agent/model input and no extra privileges.

The **Debug** tab starts a user-supplied DAP adapter such as `dlv dap` with an
explicit argv and no shell. Ryoko forwards Content-Length framed JSON both ways,
sends `initialize`, `launch` and `configurationDone`, and exposes continue,
step-over, step-in, step-out, restart and line-breakpoint controls. When paused,
Ryoko inspects stack frames, variables and breakpoint verification status from
validated DAP messages, with keyboard-operable frame navigation and expandable
variable trees. Adapter responses, events and bounded stderr are logged in an
accessible output pane. Ryoko does not emulate a debugger or execute adapters
automatically from agent output.

## Verification

- `npm test` — protocol helpers (URI encoding, language mapping, severities,
  JSON-RPC guards), workbench logic (tree, search grouping, inline-edit
  context, keybindings, multiplexer routing).
- `npm run build` — TypeScript check plus Vite production build.
- `rustc --test src-tauri/src/paths.rs` and `rustc --test
  src-tauri/src/lsp_frame.rs` — path confinement and framing tests, runnable
  without Tauri or system WebKit libraries.
- `.github/workflows/desktop.yml` builds the sidecar and installers on macOS,
  Windows and Linux runners; installers cannot be produced on a Linux-only
  development host.

## Not implemented yet

- Persisting raw provider prompts or credentials in event logs is intentionally
  unsupported. The node's task brief is the safe, operator-visible prompt summary.
- Native push-based event streaming is not yet implemented; Mission Control
  currently polls bounded private JSONL logs.
- Hunk review currently targets the local unstaged Git diff. Reviewing a
  published worktree's hunk selection from the desktop still requires the
  interactive CLI.
- Pause is process-group stop/continue and therefore Unix-only. Windows offers
  cancel, and the command returns an explicit unsupported error for pause.
- Signing/notarisation credentials and automatic release publishing.

## Packaging

- `npm run tauri build` builds `.dmg` (macOS), `.msi`/NSIS (Windows) and
  `.AppImage`/`.deb` (Linux).
- CI builds `ryoko-lsp` with the workspace and copies it to
  `src-tauri/binaries/ryoko-lsp-<target-triple>` so Tauri bundles it as an
  external binary next to the app executable.
- Builds are unsigned until certificate secrets are configured; do not present
  unsigned artifacts as release-ready.


## Mission Control runs

Mission Control can start a run without a terminal. The shell resolves the
`ryoko` CLI from `RYOKO_CLI`, a bundled/sibling executable, a workspace
`target/{debug,release}` build, or `PATH` in that order. It launches the CLI
with an explicit argv (no shell), a generated private event log under
`.ryoko/events`, and a bounded console log under `.ryoko/runs`. The goal,
budget, team concurrency and OCI sandbox image are operator inputs. **Generate
image** runs the CLI's `sandbox init --json` command with an explicit argv and
fills the image field on success. The shell never supplies
`--unsafe-host-check`, never reads provider keys, and never passes model output
as a command.

Pause and resume use the run's process group and are Unix-only; Windows shows
an explicit unsupported error and can cancel instead. Cancellation terminates
the process group so sandbox/validation children do not outlive the run.

## Mission Control event logs

The desktop exposes read-only `list_mission_logs` and `read_mission_log`
commands. They access only `.ryoko/events/*.jsonl` in the currently opened
workspace, reject path traversal, disallow symlinks outside the workspace,
and bound log reads to 4 MiB. The panel never executes event payloads or
accepts agent-supplied executable paths. Enable optional logging with
`ryoko run --event-log .ryoko/events/run.jsonl`.

### Mission Control attempt inspection

The interactive DAG presents task dependency edges, live agent phase, and
operator-visible task briefs. Live phase is deliberately coarse: **Waiting**
for an unscheduled or dependency-blocked task, **Thinking** while the agent
call runs, **Coding** once a proposal is being staged, and **Testing** during
pre-flight validation. A terminal task shows its result instead. Every event
record is timestamped by the local log writer, so selecting a node shows an
agent activity timeline, a chronological retry history, validation results,
bounded container/test logs, and a failure explanation that names the stage
(workspace, agent, staging, validation or publish) that stopped the task.

The panel also shows reported per-task token usage and run totals. Ledger
costs are estimates from configured pricing, **not verified billing**. The
**Focus** control narrows the DAG to the selected node, its prerequisite chain
and direct consumers. No event payload is executed, and only trusted local log
files are read. Selecting a node shows an operator-visible prompt context made
from the task brief and dependencies; raw model prompts are deliberately not
persisted.

## Diff-editor hunk review

The hunk panel reads the local unstaged Git diff and opens a read-only Monaco
diff editor. Each hunk gets inline **Accept**, **Reject** and **Ask agent to
revise** controls. Accept stages only that hunk and Reject reverse-applies only
that hunk to the working files; both show an OS-owned confirmation and re-verify
the full diff immediately before applying it. Each selected hunk must still
match the file identity, index base and line range it was reviewed against: a
changed index base, a vanished file difference or a moved line range is
reported per file as a stale review state and nothing is applied, instead of
silently retrying or overwriting. Renamed, copied, binary, deleted, mode-only
and submodule changes are shown as explicit non-editable review states rather
than being silently dropped from the panel. Neither action commits, merges or
pushes.

A revision starts a new `ryoko run` worktree with the selected hunk preview and
operator feedback as prompt context. The local unstaged change is **not**
silently applied to the new worktree; the confirmation states this explicitly.
The revision therefore goes through the same isolated validation as any other
run.

## Agent Sessions

The **Agent Sessions** panel is a read-only view of Ryoko's own persisted
worktree leases, not a JetBrains Air integration and not a copy of its UI. It
fills the gap between Mission Control (one run's event log) and the CLI
(`ryoko sessions`): the operator can see which agents currently have active or
published worktrees, their branch and base, changed files, state and last
update.

The `list_agent_sessions` command opens `.ryoko/state/worktrees.sqlite3` only
when it is a regular file inside the open workspace. It performs no Git, model
or subprocess calls, creates no worktree, and exposes no merge, abandon or
resume operation. Results are capped at 40 sessions, 40 file names and a
240-character summary; every string is rendered as text in the frontend, never
as HTML. A missing database simply means no sessions have been recorded.

The panel refreshes when a workspace opens and when the operator presses
**Refresh**. It does not poll, so a stale view is visible rather than silently
changing while the operator reads it.

## Native GitHub review queue

The Tauri sidebar can display a read-only snapshot of up to 30 open pull requests
and 30 open issues from an authorized GitHub App repository. Sign in to the
local GitHub App workbench, choose **Pair native desktop**, then paste the
single-use code into the sidebar. The native shell exchanges it with the broker
at `127.0.0.1:4173`; the OAuth token stays inside the Node broker and the
short-lived bridge token stays in the Rust process, never in Monaco or on disk.

The backend accepts only the fixed loopback broker, disables redirects and
proxies, requires a paired bridge token, bounds each response to 256 KiB, and
permits only canonical `https://github.com/.../pull|issues/<n>` links. Click
**Refresh** to reload; it does not poll. The queue is read-only and does not
grant permission to merge or modify GitHub content.
