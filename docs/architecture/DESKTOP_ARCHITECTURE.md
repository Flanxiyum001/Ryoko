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
lsp_start(root)                 spawns ryoko-lsp with piped stdio
lsp_send(message)               validates JSON, writes one Content-Length frame
lsp_stop()                      terminates the language server
```

`lsp_start` resolves the server from `RYOKO_LSP_BIN`, then a sibling
`ryoko-lsp` binary (the bundled Tauri sidecar), then `PATH`. Output frames are
parsed in Rust and emitted as `ryoko-lsp-message` events; stderr lines become
`ryoko-lsp-log`; process exit emits `ryoko-lsp-closed`.

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

- Signing/notarisation credentials and automatic release publishing.

## Packaging

- `npm run tauri build` builds `.dmg` (macOS), `.msi`/NSIS (Windows) and
  `.AppImage`/`.deb` (Linux).
- CI builds `ryoko-lsp` with the workspace and copies it to
  `src-tauri/binaries/ryoko-lsp-<target-triple>` so Tauri bundles it as an
  external binary next to the app executable.
- Builds are unsigned until certificate secrets are configured; do not present
  unsigned artifacts as release-ready.


## Mission Control event logs

The desktop exposes read-only `list_mission_logs` and `read_mission_log`
commands. They access only `.ryoko/events/*.jsonl` in the currently opened
workspace, reject path traversal, disallow symlinks outside the workspace,
and bound log reads to 4 MiB. The panel never executes event payloads or
accepts agent-supplied executable paths. Enable optional logging with
`ryoko run --event-log .ryoko/events/run.jsonl`.

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
