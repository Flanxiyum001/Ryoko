# Browse-only desktop marketplace (alpha)

The **Marketplace** toolbar button opens a small catalog browser for extensions
and skills in the Tauri desktop app. The browser uses the same `ryoko` CLI
registry client as `ryoko marketplace list <extension|skill> --json`; it does
not embed a second registry download stack in the desktop shell.

- Catalog entries are displayed with DOM `textContent` (never remote HTML).
- Catalog calls have a 4 MiB source output limit and display at most 100 items.
- The Tauri command uses an explicit CLI argv, with no shell interpolation.
- No install, update, script execution, or approval control is exposed.
- If the default registry is unavailable, the dialog reports that it cannot
  load the catalog. A live public registry is **not** guaranteed by this PR.

The browser is an intentionally limited UI milestone. A public marketplace
needs publisher identity verification, capability review, signed metadata,
worktree-aware permissions, and sandboxed installation/execution tests before
third-party packages should be installable from the desktop.

The archived unintegrated PR #16 `marketplace_panel.ts` assumed an API response
shape that the Tauri shell did not provide and was not mounted in the UI.
This replacement is wired to the actual `marketplace_catalog` command.
