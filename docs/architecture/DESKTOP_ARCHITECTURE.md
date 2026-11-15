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

- Persisting raw provider prompts or credentials in event logs is intentionally
  unsupported. The node's task brief is the safe, operator-visible prompt summary.
- Native push-based event streaming is not yet implemented; Mission Control
  currently polls bounded private JSONL logs.
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


## Mission Control event logs

The desktop exposes read-only `list_mission_logs` and `read_mission_log`
commands. They access only `.ryoko/events/*.jsonl` in the currently opened
workspace, reject path traversal, disallow symlinks outside the workspace,
and bound log reads to 4 MiB. The panel never executes event payloads or
accepts agent-supplied executable paths. Enable optional logging with
`ryoko run --event-log .ryoko/events/run.jsonl`.

### Mission Control attempt inspection

The interactive DAG presents task dependency edges, current agent status,
operator-visible task briefs, and reported per-task token usage (ledger costs
are estimates, **not verified billing**). Select a node to inspect the agent's
validation results, bounded container/test logs and the chronological retry
history. The **Focus** control narrows the DAG to the selected node, its
prerequisite chain and direct consumers. No event payload is executed, and
only trusted local log files are read.
