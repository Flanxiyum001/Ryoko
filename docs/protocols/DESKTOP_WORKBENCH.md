# Desktop workbench IPC

Status: implemented in `apps/desktop/src-tauri` (module `workbench`) and the
webview modules under `apps/desktop/ui/src`.

The editor workbench exposes the workspace primitives the Monaco frontend
needs: the Git change list, regex search/replace across the workspace, agent
inline edits, and the visual settings surface. All commands are typed Tauri
commands with explicit argument names, mirroring the versioned event payloads
rule in `AGENTS.md`: every reply carries `protocol: u32`
(`WORKBENCH_PROTOCOL_VERSION = 1`) so the webview can reject a shell older
than itself instead of misinterpreting field shapes.

## Commands

| Command | Arguments | Reply | Notes |
| --- | --- | --- | --- |
| `git_changes` | – | `ChangesSnapshot` | NUL-separated `git status --porcelain=v1 --untracked-files=all --no-renames -z`; output bounded to 512 KiB and 2 000 entries; `isRepository: false` outside a repo |
| `workspace_search` | `query`, `caseSensitive` | `SearchReply` | Bounded regex over UTF-8 files ≤ 2 MiB; ≤ 1 000 hits total, ≤ 200 per file; ignores `.git`, `.ryoko`, `target`, `node_modules` |
| `workspace_replace_preview` | `query`, `replacement`, `caseSensitive` | `ReplaceReply` | Dry run only; previews include a full-file `contentHash` for drift detection |
| `workspace_replace_apply` | `query`, `replacement`, `caseSensitive`, `expected` | `ReplaceResult` | Recomputes every plan at apply time; any drift against the preview skips the file; private temp file + rename, preserving existing file permissions |
| `inline_edit` | `args { path, language, instructions, before, selected, after, model? }` | `InlineEditReply` | One targeted model call; context bounded to 12 000 chars per component; the answer is fence-stripped and validated ≤ 512 KiB before it reaches the buffer |
| `settings_read` | – | `SettingsSnapshot` | Returns raw settings JSON plus the settings directory |
| `settings_save_providers` | `json` | canonical JSON | Validated as a `ryoko-models` provider catalog before any write |
| `settings_save_lsp_servers` | `json` | canonical JSON | Validated as a `LspRegistry` (see `DESKTOP_LSP_MULTIPLEX.md`) before any write |
| `workbench_protocol_version` | – | `u32` | Lets the webview feature-detect the surface |

Search parsing and validation lives in pure modules (`search.rs`, `git_status.rs`,
`inline_edit.rs`, `servers.rs`, `settings.rs`) that carry their own unit tests
and do not require the Tauri runtime; the `workbench.rs` commands stay thin and
add only workspace confinement (`WorkspaceState`) plus bounded writes.

## Settings files

Under `<workspace>/.ryoko/desktop/`:

- `providers.json` — a `ryoko-models::ProviderCatalog`. API keys are never
  stored here; profiles reference environment variables through `api_key_env`.
- `lsp.json` — the language-server registry documented in
  [`DESKTOP_LSP_MULTIPLEX.md`](DESKTOP_LSP_MULTIPLEX.md).

Both files are written through a private (0600 on Unix) temp file and renamed
into place after the directory is created through
`paths::create_directories_without_symlinks`, and read backs are capped at
256 KiB with symlink resolution.

## Model routing for `inline_edit`

`model_runtime::load_router` resolves the selection in this order:

1. An explicit model from the request.
2. `RYOKO_MODEL` from the environment.

The provider side comes from the workspace catalog when
`.ryoko/desktop/providers.json` exists, otherwise from a direct
OpenAI-compatible route (`RYOKO_BASE_URL`, default
`https://api.openai.com/v1`, key from `RYOKO_API_KEY` or `OPENAI_API_KEY`,
keyless loopback routes allowed for local model servers). Every router is
built with an unlimited usage ledger; no key ever crosses IPC.

## Bounds

| Limit | Value |
| --- | --- |
| Search result rows | 1 000 |
| Hits per file | 200 |
| Searchable file size | 2 MiB |
| Replaceable files per run | 200 |
| Query length / regex size | 512 B / 4 MiB |
| Inline-edit context per component | 12 000 chars |
| Inline-edit answer | 512 KiB |
| Settings file | 256 KiB |
| Git change entries | 2 000 (512 KiB of porcelain output) |
