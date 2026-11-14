# Editor architecture

Status: the LSP server and virtual document store exist; the desktop editor does
not.

## Layering

```text
project files
  → workspace service (watch, canonical paths, external-change detection)
  → document manager (versions, dirty buffers, recovery)
  → editor model (Monaco instances, view state)
  → language services (LSP, formatting, diagnostics)
```

Monaco renders buffers. It does not own the filesystem, the document registry or
the language-server lifecycle.

## Document model

Each open document carries:

- `uri`, `language_id`, `version`;
- last saved content hash;
- current buffer and dirty flag;
- last known on-disk `mtime` and hash.

Rules:

1. Version increments on every accepted edit.
2. An external change with a different hash marks the buffer as externally
   modified; the user chooses reload, keep, or diff.
3. Agent patches are proposals against a **specific version**. If the document has
   advanced, the proposal is marked stale and must be rebased or reviewed again.
4. Saves are atomic (write temp, fsync, rename) where the platform allows it.
5. Crash recovery keeps an editor-owned snapshot directory outside the repository.

## LSP manager

The manager owns server processes as external processes with explicit executable
discovery, arguments, working directory, environment allowlist, timeouts and
restart policy.

Lifecycle: discover → spawn → initialize → initialized → document sync →
requests → shutdown → exit; kill on timeout.

Server selection is per language and per workspace root. Capabilities returned by
the server gate which features the UI enables. Servers are never downloaded
silently: installation is an explicit user action with a visible source.

Supported first: rust-analyzer, typescript-language-server, Pyright, gopls, clangd.
Debugging uses DAP and a separate session manager.

## Diagnostics pipeline

Two producers share one editor surface:

1. **Language servers** — push diagnostics per document.
2. **Ryoko agents** — publish findings through `ryoko_native::diagnostic_channel()`.

Both are normalised into one record:

```text
uri · range · severity · code · source · message · optional fix
```

Editor requirements: stable identifiers so diagnostics can be updated or cleared,
per-producer replacement (an agent refresh does not wipe compiler output), and a
Problems view grouped by file and producer.

Quick fixes are proposals. Applying one is an edit against a document version and
follows the same staleness rules as any other patch. `ryoko.fix`, `ryoko.explain`
and `ryoko.review` commands surface intent; the host decides whether to start a
workflow, and no model call happens inside the language server.

## Editing surface (P0)

Tabs and split groups, syntax highlighting, completion, go to definition, find
references, rename, find in files, diagnostics and markers, formatting, integrated
terminal, Git status and diff, configurable keybindings, command palette, autosave
and crash recovery, workspace settings.

## Review surface

Agent changes appear as a diff against the proposal's base, with task, agent,
changed files, added/removed lines, explanation, validation evidence and review
findings. Accept, reject, request changes, open in editor, apply selected hunks.
Accepting never silently merges into the main checkout without policy
authorization.
