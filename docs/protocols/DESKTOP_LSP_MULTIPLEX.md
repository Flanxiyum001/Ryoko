# Desktop language-server multiplexing

Status: implemented in `apps/desktop/src-tauri/src/{lsp,servers,lsp_frame}.rs`
and `apps/desktop/ui/src/{lsp,lsp_routing,lsp_bridge}.ts`.

## Why

`ryoko-lsp` publishes Ryoko's custom diagnostics and agent code lenses, but
standard languages need their standard servers (rust-analyzer, pyright,
typescript-language-server). The desktop shell therefore runs a small pool of
stdio language servers and routes JSON-RPC per method.

## Registry

`.ryoko/desktop/lsp.json`, schema `{ "version": 1, "servers": [...] }` with
`{ id, command, args, languages }` per entry:

- `id`: kebab-case, unique; `ryoko-lsp` is reserved for the built-in server.
- `command`: a bare executable name resolved through `PATH`. No path
  separators, no shell metacharacters, no traversal, no leading dash — spawn
  is argv-only, never through a shell.
- `languages`: lowercase language ids; one owner per language, enforced at
  validation time so routing stays deterministic.
- At most 16 servers with 16 arguments each; the file is capped at sizes
  enforced by `servers::LspRegistry::parse` and stored by `settings.rs`.

`ryoko-lsp` always starts; optional servers that are simply not installed are
reported through `lsp_start`'s `failed` list and never take the shell down.

## Routing rules

| Traffic | Target |
| --- | --- |
| `textDocument/didOpen`/`didChange`/`didClose`/`didSave`, `workspace/didChange*` | every running server (fan-out) |
| `workspace/executeCommand`, `workspace/symbol`, agent notifications | `ryoko-lsp` only |
| document requests (`textDocument/completion`, `hover`, `definition`, …) | the language's owner server; `ryoko-lsp` when none is registered |
| `$/…` internal notifications | dropped |
| `client/registerCapability`, `workspace/configuration`, `workspace/workspaceFolders`, `window/workDoneProgress/create`, `window/showMessageRequest` | answered by the webview itself |
| any other server→client request | `MethodNotFound` (-32601) |

The pure decision table lives in `lsp_routing.ts` and is mirrored by
`servers::route_outbound` on the Rust side; both are unit-tested.

## Transport

Frames cross the Tauri boundary as JSON strings with an explicit `server` id:

| Event | Payload | Purpose |
| --- | --- | --- |
| `lsp-frame` | `{ server, message }` | a full inbound JSON-RPC message |
| `lsp-log` | `{ server, line }` | stderr line or protocol error |
| `lsp-exit` | `{ server }` | the child exited; the webview drops its state |

Outbound frames go through the `lsp_send` command (`server`, `message`), are
JSON-validated, and capped to `lsp_frame::MAX_FRAME_BYTES`.

## Monaco bridge

`lsp_bridge.ts` registers completion, hover and definition providers for the
known language set. Requests route through `LspClient::requestForLanguage`,
which resolves the owner and applies a bounded 3-second timeout so a hung
server degrades to "no suggestions" instead of blocking typing. Diagnostics
arrive per server via `textDocument/publishDiagnostics` and are rendered with
per-server marker owners, so `rust-analyzer` and `ryoko-lsp` never overwrite
each other's markers.
