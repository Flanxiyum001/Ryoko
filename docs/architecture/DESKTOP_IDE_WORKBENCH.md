# Desktop IDE workbench

The native Tauri frontend (`apps/desktop/ui`) uses a compact activity rail,
workspace file picker, explorer, tab strip, Monaco editor, bottom tools and status
bar. Open folder uses the native directory dialog; editing and saving an existing
project does not require a CLI. Search, changes, extensions, settings and agent
runs are reachable by buttons.

## Document tabs

Each open file retains its Monaco model, undo history, saved text, dirty flag and
editor view state. Switching tabs does not reread the file or replace its buffer.
The existing backend optimistic concurrency check still rejects saving over an
external edit. To reload a disk change, close and reopen the file; dirty buffers
require explicit discard confirmation first.

Closing the active tab selects its next neighbor, falling back to the previous
one. Closing the last tab restores the welcome screen. Closing a background tab
does not change the selection. Close cancels pending document-change timers,
notifies the LSP client and disposes the Monaco model. A save in flight prevents
closing its buffer or switching workspace. Late file-read responses cannot
replace a newer selection or create a model in another workspace.

Tab labels expose unsaved state to assistive technology and disambiguate duplicate
basenames. Arrow keys, Home and End navigate tabs; Delete closes the focused tab.
Ctrl/Cmd+W closes the active file and is configurable in Settings.

## Scope

This change does not add VS Code extension-host compatibility, VSIX installation,
remote extension execution, Git clone/push/pull controls, new-file creation,
split editors, autosave or crash recovery. The marketplace catalog remains
browse-only; a local WASI extension directory can be copied through the existing
`ryoko plugin install` workflow after native confirmation, and still requires a
separate policy opt-in before it runs. Desktop installers remain governed by the
existing preview and release workflows; a successful renderer test does not
attest to native packaging or signing.

## Prompt context pins

The chat prompt accepts `@file:path`, `@symbol:name`, `@docs:query`, `@docs`,
`@git-diff` and `@selection`. `context_mentions.ts` parses and de-duplicates the
tokens for display; the native `context_mentions` module is the trust boundary.
It rejects paths outside the workspace, sensitive filenames, malformed symbol or
docs queries, more than eight pins and more than 128 KiB of combined context.
The workspace root and candidate paths are canonicalized before containment
checks, so macOS temporary-directory aliases and Windows short paths cannot
produce false escapes.
File bodies are capped at 32 KiB, docs sections at 4 KiB, and the working-tree
diff at 64 KiB after sensitive tracked paths are removed. Symbols are read from the existing SQLite index; docs come from
`.ryoko/memory/*.md` and a bounded `docs/**/*.md` walk. Project text is wrapped
in explicit tags and never treated as instructions. An unresolved pin blocks the
send rather than being silently dropped.

## Agent sessions and worktrees

Published sessions can be expanded to list changed files and open a read-only
Monaco diff, with previous/next navigation across the file list. The native
sidebar commands read Git objects directly; they do not check out or modify the
main working tree. **Detach**, **Discard** and **Merge…** call the CLI's explicit
`review --release`, `--discard` and `--accept --json` actions after a native
confirmation. Merge requires a reviewed OCI sandbox image and uses the same
sandboxed pre-flight validation as the CLI. The UI cannot merge when validation
fails, cannot pass a target branch that looks like an option, and never executes
model output as a command.

## Editor ecosystem

Settings offers Default, Vim and Emacs keymaps. Vim and Emacs are loaded lazily
into Monaco; workbench chords such as `Mod+S` stay available, while
single-modifier editor motions (`Mod+B`, `Mod+P`) are left to the modal layer
while focus is in the editor. A VS Code color-theme JSON or `settings.json` can
be imported locally. Theme import accepts only mapped hex colors and bounded
token rules; settings import accepts a fixed list of font, tab, wrap, line-height
and minimap values. Imported values are stored in local storage and can be reset.
Local WASI installation validates the selected directory and optional SHA-256
pin before calling the CLI; the CLI re-reads and validates the manifest. The
installed copy is then listed with its pinned hash. Installation does not edit
the plugin policy or run the module.

## Acceptance checks

- Open a folder, open two files, edit one and switch between them: text, undo
  history and cursor/scroll state remain associated with that file.
- Cancel closing a dirty tab: its text and tab remain. Save, close and reopen:
  the saved content is read from disk. Confirm discard: only that buffer closes.
- Close a background tab: the active editor is unchanged. Close the final tab:
  the welcome view returns.
- Navigate tabs with arrow keys and close with Ctrl/Cmd+W. Open Search, the
  extension catalog and Quick Open from visible controls.
- At 680 px width, select a file from Explorer: the editor becomes visible and
  the window has no horizontal overflow.
- Switch workspaces while dirty: cancel preserves buffers; confirm clears tabs.
- Pin `@file`, `@symbol`, `@docs` and `@git-diff`; an unknown pin blocks the send
  and a symbol pin without an index reports how to build one. Resolve file and
  symbol pins from a workspace reached through a symlinked path.
- Pin a file through `..` or a sensitive name (`.env`, `.ssh`); both are rejected
  with an error and no context is attached.
- Expand a published session, open two files and navigate with Previous/Next.
  Cancel Detach, Discard and Merge before confirming; no branch or worktree
  changes. A merge with a failing check reports failure and leaves the checkout
  unchanged.
- Switch to Vim and Emacs modes and confirm normal editing plus `Mod+S`. Import
  a valid VS Code theme and settings file; reject a non-hex color and an
  out-of-range font size. Install a local fixture extension and confirm it is
  listed without running.

`npm test` covers tab selection and the existing frontend models. `npm run build`
checks TypeScript and produces the frontend bundle. `npm run test:ui` runs the
real renderer and Monaco in Chromium against a deterministic mocked Tauri IPC
boundary. CI runs these browser checks and uploads welcome/editor/narrow-window
screenshots plus failure traces as `workbench-ui-evidence`. They do not exercise
native OS dialogs, real disk writes, agent providers or language-server processes.
