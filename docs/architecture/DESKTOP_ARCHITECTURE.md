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
| `src-tauri` | Window lifetime, workspace file commands, Mission Control event-log reads, `ryoko-lsp` process lifecycle and framing, capability scopes |
| `ui/` | Monaco models, explorer, editor keybindings, LSP JSON-RPC client, problems view, Mission Control graph |

### Commands

```text
open_workspace(path)            canonicalises and stores the workspace root
list_files()                    bounded, gitignore-aware file list
read_file(path)                 UTF-8, max 2 MiB, canonical path inside the root
write_file(path, content)       atomic save, max 2 MiB, refuses symlink escapes
lsp_start(root)                 spawns ryoko-lsp with piped stdio
lsp_send(message)               validates JSON, writes one Content-Length frame
lsp_stop()                      terminates the language server
list_mission_logs()             bounded .ryoko/events/*.jsonl list, newest first
read_mission_log(name)          reads one log by plain filename, max 4 MiB
```

`lsp_start` resolves the server from `RYOKO_LSP_BIN`, then a sibling
`ryoko-lsp` binary (the bundled Tauri sidecar), then `PATH`. Output frames are
parsed in Rust and emitted as `ryoko-lsp-message` events; stderr lines become
`ryoko-lsp-log`; process exit emits `ryoko-lsp-closed`.

The frontend sends `initialize` / `initialized`, `didOpen`, `didChange` and
`didClose`, and turns `textDocument/publishDiagnostics` into Monaco markers and
a problems list. Documents therefore live in `ryoko-vfs` inside the server
process, not in the browser.

### Mission Control

`ryoko run --event-log .ryoko/events/run.jsonl` writes `OrchestratorEvent`
values as one JSON object per line. The panel polls the newest log in that
directory, reduces the events into task nodes, and lays dependencies out in
columns. Selecting a node shows its brief, attempts, agent, validation summary
and logs, pull request, output and error. The event log is written beneath the
repository root, is created exclusively with private permissions, and is
capped at 8 MiB; the shell reads plain `.jsonl` names only, capped at 4 MiB.

This first milestone is deliberately read-only. Pause, reroute, and live control
of a running orchestrator require an authenticated control channel and are not
implemented yet.

### Security

- Workspace commands reject absolute paths, parent traversal, NUL bytes,
  symlink escapes, non-UTF-8 content and files over 2 MiB.
- Saves write a sibling temp file, fsync, then rename (Windows replaces first).
- The capability manifest grants only `core:default` and `dialog:allow-open`;
  there is no shell plugin and no filesystem plugin scope.
- The CSP allows only local scripts/styles, local workers and Tauri IPC.

## Verification

- `npm test` — protocol helpers (URI encoding, language mapping, severities,
  JSON-RPC guards).
- `npm run build` — TypeScript check plus Vite production build.
- `rustc --test src-tauri/src/paths.rs` and `rustc --test
  src-tauri/src/lsp_frame.rs` — path confinement and framing tests, runnable
  without Tauri or system WebKit libraries.
- `.github/workflows/desktop.yml` builds the sidecar and installers on macOS,
  Windows and Linux runners; installers cannot be produced on a Linux-only
  development host.

## Not implemented yet

- Mission Control pause/reroute controls and per-agent token telemetry.
- Side-by-side worktree hunk review (the CLI still accepts or rejects a whole
  patch).
- Signing/notarisation credentials and automatic release publishing.

## Packaging

- `npm run tauri build` builds `.dmg` (macOS), `.msi`/NSIS (Windows) and
  `.AppImage`/`.deb` (Linux).
- CI builds `ryoko-lsp` with the workspace and copies it to
  `src-tauri/binaries/ryoko-lsp-<target-triple>` so Tauri bundles it as an
  external binary next to the app executable.
- Builds are unsigned until certificate secrets are configured; do not present
  unsigned artifacts as release-ready.
