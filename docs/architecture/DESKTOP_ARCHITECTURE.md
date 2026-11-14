# Desktop architecture

Status: planned. No Tauri or frontend package exists in this workspace yet.

## Shape

```text
apps/desktop/
  src/                React + TypeScript UI
  src-tauri/          Rust application shell
    src/              command handlers, window lifetime, services wiring
    capabilities/     Tauri capability manifests
```

`Cargo.toml` already excludes `apps/desktop/src-tauri` from the root workspace, so
the desktop shell builds independently while reusing the domain crates.

## Responsibilities

| Part | Owns |
| --- | --- |
| `src-tauri` | Window lifetime, command handlers, service composition, credential access, event dispatch |
| `src/` | Rendering, keybindings, panel layout, optimistic UI, local view state |

The frontend never owns project files, model credentials, permission policy or
task truth. It renders what the backend reports and requests typed operations.

## IPC contract

Commands are verbs with typed arguments and typed results. Events are facts, not
requests.

```text
workspace.open / close / list
editor.open_document / save_document / apply_patch / get_document
git.status / diff / stage / commit
model.list_providers / list_models / test_connection
agent.list / start / pause / resume / cancel
task.list / get / approve / reject
terminal.create / write / resize / terminate
lsp.start / stop / restart
approval.list / respond
```

```text
workspace.changed            editor.document_changed
editor.diagnostics_updated   git.status_changed
git.diff_updated             agent.status_changed
agent.progress               agent.tool_requested
agent.artifact_created       task.created / updated / completed / failed
approval.required / resolved execution.started / output / completed
model.usage_updated          security.policy_violation
```

Rules:

- Every payload is Serde-typed on both sides; no untyped JSON blobs.
- Every event carries `workspace_id`, and where relevant `task_id`,
  `attempt_id`, `document_version` and a monotonic sequence number.
- Event schemas are versioned (`v1`). Adding a field is compatible; removing or
  changing meaning is not.
- Long-running work returns an identifier immediately and reports progress through
  events. Cancellation is a first-class command.
- Commands validate arguments before touching services; errors are structured
  (`code`, `message`, `retryable`).

## Security

- Capability manifests grant the minimum surface: no shell plugin, no unrestricted
  filesystem scope.
- Content Security Policy is strict; no remote code, no inline scripts in
  production.
- Provider keys and tokens live in the OS credential store, never in `localStorage`,
  project files or event logs.
- The custom protocol handler for project files validates canonical paths inside
  the workspace and refuses symlink escapes.
- Approvals are recorded server-side; the UI cannot manufacture an approval.

## State model

UI state is derived. Sources of truth:

- backend snapshots for workspace, tasks, approvals and diagnostics;
- a local document buffer only while the user edits, reconciled by version.

When a backend event reports a newer `document_version`, the editor marks unsaved
local buffers as conflicting instead of overwriting them.

## Packaging

- Build on Windows, macOS and Linux in CI; never infer cross-platform behaviour
  from Linux alone.
- Sign and notarise macOS builds; sign Windows installers.
- Versioned database migrations run before the first window opens; failure to
  migrate is a fatal, visible error, not a silent reset.
- Crash reporting is opt-in and redacts paths, prompts and credentials.
