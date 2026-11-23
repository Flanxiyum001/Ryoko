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
Git clone/push/pull controls, new-file creation, split editors, autosave or crash
recovery. The existing extension/skill catalog is explicitly browse-only. Desktop
installers remain governed by the existing preview and release workflows; a
successful renderer test does not attest to native packaging or signing.

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

`npm test` covers tab selection and the existing frontend models. `npm run build`
checks TypeScript and produces the frontend bundle. `npm run test:ui` runs the
real renderer and Monaco in Chromium against a deterministic mocked Tauri IPC
boundary. CI runs these browser checks and uploads welcome/editor/narrow-window
screenshots plus failure traces as `workbench-ui-evidence`. They do not exercise
native OS dialogs, real disk writes, agent providers or language-server processes.
