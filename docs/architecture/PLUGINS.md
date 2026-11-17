# WASI extension API

`ryoko-scripting` hosts third-party extensions as ordinary **WASI command
modules**. A plugin is a `.wasm` file plus a JSON manifest; the host feeds one
JSON request on stdin and accepts one JSON response on stdout. Any toolchain that emits a WASI command module can use this ABI. The host
has only been exercised with Rust/WAT modules; Python or JavaScript support
requires a toolchain that compiles to a WASI command module and is not an
embedded interpreter.

This is an extension boundary, not a second orchestrator. Plugins cannot read
files, spawn processes, open sockets or see environment variables. They return
data that the host validates before it becomes an agent result, a lint result or
a context outline.

## Manifest

```json
{
  "schema": 1,
  "id": "acme-python-linter",
  "name": "Acme Python linter",
  "version": "1.0.0",
  "kind": "linter",
  "description": "Checks Acme framework conventions"
}
```

`schema` must be `1`. `id` is 1-64 lowercase ASCII letters, digits, `.`, `_` or
`-`. `kind` is one of `agent`, `linter` or `context`. The manifest is limited to
64 KiB.

## Host policy

Plugins are denied unless the host policy names both the id and the kind. There
is no allow-all policy.

```json
{
  "allowed_ids": ["acme-python-linter"],
  "allowed_kinds": ["linter"],
  "limits": {
    "fuel": 100000000,
    "timeout_ms": 10000,
    "max_memory_bytes": 134217728,
    "max_output_bytes": 2097152
  }
}
```

The host applies its own ceiling to each limit, so a policy cannot raise the
process-wide maximum. Fuel bounds CPU, epoch interruption bounds wall time, and
a linear-memory limiter bounds memory. Request input is capped at 256 KiB;
source input at 512 KiB; stderr is capped at 16 KiB and never parsed.

## Requests and responses

Every payload carries `schema: 1`.

Agent request:

```json
{
  "schema": 1,
  "operation": {
    "type": "agent",
    "task": { "id": "...", "description": "...", "assigned_to": "custom-role", "dependencies": [], "metadata": {} },
    "project": { "project_root": "/repo", "active_branch": "main", "shared_memory": {} },
    "role": "custom-role"
  }
}
```

Agent response:

```json
{
  "schema": 1,
  "result": {
    "type": "agent",
    "summary": "add a health endpoint",
    "files": [
      { "path": "src/health.rs", "content": "pub fn health() {}" },
      { "path": "tests/health.rs", "content": "#[test]\\nfn healthy() {}" }
    ]
  }
}
```

A linter request uses `{"type":"lint","path":"src/app.py","source":"..."}` and
returns `{"type":"lint","diagnostics":[{"line":1,"column":1,"severity":"warning","message":"..."}]}`.
A context request uses `{"type":"context","path":"src/app.py","source":"..."}`
and returns `{"type":"context","outline":"...","symbols":["main"]}`.

The returned files are still untrusted. `PluginAgent` validates relative paths
and sizes, then hands them to the normal workspace, review and sandbox
validation path. A plugin never writes a file directly.

## Host usage

```rust
use ryoko_scripting::{PluginHost, PluginLimits, PluginPolicy};

let host = PluginHost::new(PluginLimits::default())?;
let policy = PluginPolicy::new(
    ["acme-python-linter"],
    [ryoko_scripting::PluginKind::Linter],
    PluginLimits::default(),
);
let plugin = host
    .load("acme.plugin.json".as_ref(), "acme.wasm".as_ref(), &policy)
    .await?;
let diagnostics = plugin.lint("src/app.py", source).await?;
```

`PluginAgent::new(...)` wraps an `agent` plugin as an `AgentRuntime`; register it under a custom role with the orchestrator:

```rust
let agent = ryoko_scripting::PluginAgent::new(
    std::sync::Arc::new(plugin),
    ryoko_core::AgentRole::Custom("acme-reviewer".into()),
    "acme-reviewer",
    "Acme review plugin",
)?;
let agents: Vec<std::sync::Arc<dyn ryoko_core::AgentRuntime>> = vec![std::sync::Arc::new(agent)];
// tasks assigned to AgentRole::Custom("acme-reviewer") use this agent.
```

A linter/context plugin cannot be promoted to an agent because the declared kind
is checked before execution.

## CLI usage

`ryoko plugin` loads one explicitly named manifest/module pair and applies the
same host policy as the library API. Every subcommand requires `--manifest`,
`--wasm` and `--policy`; a policy that omits the manifest id or kind fails
closed before the module is compiled.

Lint a file and print the diagnostics as JSON:

```sh
ryoko plugin \
  --manifest acme.plugin.json \
  --wasm acme.wasm \
  --policy ryoko.plugins.json \
  lint --path src/app.py --source-file src/app.py
```

When `--source-file` is omitted, `lint` and `context` read at most 512 KiB from
stdin, so a shell pipeline works too:

```sh
ryoko plugin ... context --path src/app.py < src/app.py
```

Run an agent plugin against one task and print the resulting `TaskResult` as
JSON. The plugin still returns proposed files only; the CLI never writes them
to the workspace:

```sh
ryoko plugin ... agent --task "add a health endpoint" --repo .
```

The policy file is the same JSON shape documented above:

```json
{
  "allowed_ids": ["acme-python-linter"],
  "allowed_kinds": ["linter"],
  "limits": {
    "fuel": 100000000,
    "timeout_ms": 10000,
    "max_memory_bytes": 134217728,
    "max_output_bytes": 2097152
  }
}
```

The command also fails when the module's declared kind does not match the
subcommand, when the plugin response violates the typed ABI, or when any fuel,
time, memory or output limit is exceeded.

## Isolation properties

- Fresh wasmtime store per invocation.
- WASI preview 1 only; no host functions beyond the preview-1 ABI.
- No preopened directories, no inherited arguments, no inherited environment.
- Default WASI network policy denies all addresses; preview 1 exposes no socket
  API in this host.
- Fuel, epoch deadline, memory limit, stderr cap and stdout cap.
- Manifest, wasm and policy files are read only from explicit paths; symlinked
  files are rejected.
- Output is parsed as narrow typed JSON, not executed or interpreted.

## Limitations

- No streaming protocol, host callbacks, capability grants or plugin signing
  yet. A plugin that needs repository data must receive it in the bounded
  request; it cannot read the workspace.
- Module compilation happens before the per-invocation fuel, time and memory
  limits apply and is not interruptible. Load only reviewed wasm files from
  paths the operator selected; a hostile module can make compilation expensive.
- Python and JavaScript support means "compile to a WASI command module", not an
  embedded Python/JS interpreter.
- This has unit tests using WAT modules, not a published Rust/Python/JS SDK.
- Plugin code runs in the same host process; wasmtime isolation is the boundary.
  It is not an OS-level VM.
