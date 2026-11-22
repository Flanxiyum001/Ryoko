# MCP developer-tool client (operator-controlled alpha)

`ryoko-core::mcp` owns the MCP JSON-RPC client, schema and response
validation, pagination limits, mandatory `McpApprovalGate`, host allowlist and
the `McpToolbox` used by model agents. The CLI implements a bounded **stdio**
transport. The agent loop remains deliberately narrow: it is bounded by tool
rounds, per-round calls, a run-wide call budget, the shared model ledger and a
fresh human approval for every call.

## List tools from a trusted server

```sh
cargo run -p ryoko-cli -- mcp --server node \
  --server-arg ./path/to/trusted-mcp-server.mjs tools
```

## Invoke a tool with human confirmation

```sh
cargo run -p ryoko-cli -- mcp --server node \
  --server-arg ./path/to/trusted-mcp-server.mjs \
  call --tool list_issues --arguments '{"state":"open"}'
```

The CLI prints the chosen tool and its exact argument JSON and asks for a scope:
`[o]` allow once, `[r]` allow this tool for the rest of the run, or `[d]` deny.
Only an explicit allow widens anything; `YES` is still accepted as "allow once",
and a blank line, an unknown word, a closed stdin or the 90-second timeout
denies. While `RYOKO_NONINTERACTIVE` is set the caller refuses without reading
stdin, so a headless run can never block on a prompt nobody can answer. Server
tools cannot grant themselves permissions, and listing tools does not imply
approval.

## Session approval scopes

A host that can show a richer prompt uses `SessionApprovalGate` instead of a
one-shot boolean. It implements the same `McpApprovalGate` trait and offers the
three scopes a native dialog needs:

| Scope | Effect |
| --- | --- |
| `Once` | This call only. The next call asks again. |
| `ForRun` | Later calls of **that same tool** are allowed for the rest of the run without asking. Every call still passes through the gate, so the host allowlist, call budget and argument limits still apply. |
| `Deny` | Refused, and not asked again for the rest of the session. |

Grants are per tool: approving `list_issues` for the run never approves
`delete_issue`. `revoke_grants()` drops the session's allowances (for example
when a new run starts) and leaves refusals in force. `decisions()` returns the
bounded decision log, which records the tool and scope only - the arguments may
hold credentials or customer data and are never logged. A blank tool name is
refused without asking, and the grant set is capped so a loop cannot grow it
without bound.

Scopes are granted by the host. No model output, tool listing or earlier
successful call can produce a `ForRun` grant on its own.

The CLI wires this gate to `TerminalScopeAsker`. Selecting `[r]` prints the
tool that was granted for the run, and the run's gate lives for the duration of
that `ryoko run` invocation only.

## Headless policy

A run with no approval prompt cannot ask anyone, so the operator pre-approves
named tools instead:

```sh
ryoko run --team --repo . \
  --mcp-server /srv/ryoko-mcp --mcp-server-arg --stdio \
  --mcp-allow-tool list_issues --mcp-allow-tool get_issue \
  --mcp-headless-approve list_issues \
  "Fix the failing parser test"
```

`--mcp-headless-approve` (or `RYOKO_MCP_HEADLESS_APPROVE`, a comma-separated
list) is consulted **only** when no prompt is possible, that is when
`RYOKO_NONINTERACTIVE` is set. An interactive session always asks, so a stale
policy can never answer a question on a human's behalf.

The policy is an approval, not a permission grant:

- every pre-approved tool must also appear in `--mcp-allow-tool`, otherwise the
  run fails before connecting (`headless-approved MCP tool ... is not on the
  host allowlist`); the allowlist remains the outer boundary;
- a pre-approval becomes a `ForRun` scope for that one tool, so the call budget,
  argument limits and per-call gate still apply;
- **every tool not named is denied.** An empty or absent policy denies all tool
  calls, which is the default for a headless run;
- entries are trimmed, de-duplicated and dropped when blank or containing control
  characters, and dropping an entry can only ever deny a call.

The GitHub App passes the same settings through its own environment
(`RYOKO_MCP_SERVER`, `RYOKO_MCP_SERVER_ARGS`, `RYOKO_MCP_ALLOW_ENV`,
`RYOKO_MCP_ALLOW_TOOLS`, `RYOKO_MCP_HEADLESS_APPROVE`, `RYOKO_MCP_MAX_CALLS`),
so the issue-to-PR pipeline can use a small, explicitly approved tool set and
nothing more.

Servers may need credentials. The child gets a minimal PATH and
temporary-directory environment; all other variables are denied by default.
`HOME` and `USERPROFILE` are deliberately not forwarded because they expose
per-user credential directories such as `~/.ssh` and `~/.aws`. Explicitly
forward only the variable a reviewed server requires:

```sh
cargo run -p ryoko-cli -- mcp --server node \
  --server-arg ./path/to/trusted-mcp-server.mjs \
  --allow-env GITHUB_TOKEN tools
```

Environment variable **names**, not secret values, are passed on the
command line. Never select a server executable from model output and never
run an untrusted MCP server without a stronger process sandbox. MCP server
output may contain untrusted/prompt-injection content and is not fed into
agent prompts by this integration.

## Give a run access to selected tools

```sh
cargo run -p ryoko-cli -- run "close stale issues" --team \
  --mcp-server node \
  --mcp-server-arg ./path/to/trusted-mcp-server.mjs \
  --mcp-allow-tool list_issues \
  --mcp-allow-tool add_comment \
  --mcp-max-calls 10
```

`--mcp-server` is an explicitly chosen executable. `--mcp-allow-tool` is
mandatory; a tool that the server does not advertise makes connection fail
closed. The toolbox attaches to every model agent through
`Orchestrator::with_mcp`. Each call still prompts the terminal operator for
`YES`. The model sees only the allowlisted tool definitions, gets at most four
tool rounds and 16 calls per round, and tool results are truncated before they
are sent back. The shared `UsageLedger` is checked before each call, including
the owning DAG task's scope, so an exhausted run or task budget blocks further
tools; `--mcp-max-calls` is an additional independent cap.

`McpToolPolicy` and `McpToolbox` are public in `ryoko-core` for embedders that
build their own trusted transport and approval UI.

## Security boundaries

- Legacy MCP initialize/initialized handshake and `tools/list`,
  `tools/call` over newline-delimited JSON-RPC 2.0 stdio.
- Protocol negotiation supports 2025-03-26 and 2025-06-18. Other versions
  fail rather than silently guessing framing or tool semantics.
- Requests and tool arguments capped at 64 KiB; response frames and results at
  1 MiB; max 256 advertised tools and 16 pagination pages.
- 15-second per-message timeout; child killed when the transport drops.
- No shell is invoked to run the server; the server binary and argument
  array are explicitly provided by the operator.
- Tool calls require human approval through a trusted host-owned gate, either
  per call or through an explicit per-tool session scope.
- Agent tools require a non-empty host allowlist; the server catalogue cannot
  add tools to it. Call count and argument size are host-bounded.
- Remote Streamable HTTP, OAuth, multi-session persistence, sampling and
  elicitation are **not** implemented. The scoped gate is wired into the CLI;
  the desktop native approval dialog is still pending, so nothing in the Tauri
  shell offers the scopes yet.

Next: surface `SessionApprovalGate` through Tauri IPC so the dialog can offer
"allow once", "allow for this run" and "deny", and extend the same scoped gate
to agent shell and network actions. Never permit an agent to fabricate an
approval gate or execute MCP calls without that trusted host implementation.

## Connectors: several reviewed servers in one run

A connector is a declarative, secret-free description of one external MCP
server. The registry lives at `<project>/.ryoko/connectors.json` (that directory
is git-ignored) and is shared by the CLI and the desktop shell. It stores
environment variable **names**, never values; credentials still come from the
host environment or the OS credential store.

```json
{
  "version": 1,
  "connectors": [
    {
      "id": "github",
      "name": "GitHub",
      "category": "Source Control",
      "transport": {"kind": "stdio", "command": "npx", "args": ["-y", "@modelcontextprotocol/server-github"]},
      "required_env": ["GITHUB_PERSONAL_ACCESS_TOKEN"],
      "allowed_tools": ["list_issues", "get_issue"],
      "write_tools": ["create_issue", "create_pull_request"],
      "enabled": true,
      "max_calls_per_run": 20
    }
  ]
}
```

`configs/connectors.example.json` is the built-in catalogue: GitHub,
Cloudflare, Filesystem, Fetch and a generic custom template. Every preset ships
`enabled: false` with read-only tools allowed; `write_tools` names the tools that
mutate remote state so a UI can tag them, and a write tool runs only after it is
moved into `allowed_tools` and then passes the usual approval gate.

`transport` is either `stdio` (`command` + `args`) or a network endpoint
(`sse` / `streamable_http` with `url`). Network URLs follow the provider rule:
HTTPS outside loopback, no credentials, query or fragment. The current CLI
mounts stdio connectors only; an enabled network connector fails the run with an
actionable message instead of being silently skipped.

### Manage connectors

```sh
ryoko connectors list                 # presets, configured connectors, offline readiness
ryoko connectors list --json          # machine-readable snapshot
ryoko connectors add github           # copy a preset into the workspace registry
ryoko connectors add custom --id internal --command node --arg server.mjs \
  --env INTERNAL_TOKEN --allow-tool lookup
ryoko connectors enable github
ryoko connectors disable github
```

`list` mirrors `ryoko doctor`: it reads whether each declared variable is set and
never contacts the server. A connector with no allowed tools cannot be enabled.

### What a run mounts

`ryoko run` loads every enabled connector automatically and still accepts
`--mcp-server` for one explicitly chosen server, which is mounted beside them.
`--no-connectors` opts out; `--connectors-config <PATH>` loads a different
registry.

Each connector is its own `McpToolbox`, so it keeps its own allowlist,
`max_calls_per_run` (default 25), argument limit and `SessionApprovalGate`. The
`MultiMcpToolbox` in `ryoko-core` routes a namespaced name to its owner:

| Connector | Server tool | Model-visible name |
| --- | --- | --- |
| `github` | `list_issues` | `github__list_issues` |
| `cloudflare` | `kv_get` | `cloudflare__kv_get` |

A tool-name collision between connectors, or between a connector and the
unprefixed `--mcp-server`, fails before the run starts. Approval prompts show the
namespaced name, so a human always sees which service is asking.

### Credential isolation

A connector child process is spawned with a cleared environment. Only `PATH`,
`TMPDIR` and `TEMP` are forwarded for the interpreter itself, plus exactly the
variables listed in that connector's `required_env`. `HOME` and `USERPROFILE`
are never forwarded. A declared variable that is unset or blank fails the run
with its name, rather than starting a server that silently cannot authenticate.

In a headless run, `--mcp-headless-approve` entries for connectors are written
`<connector>__<tool>` and must name a tool that connector allows. An approval
for one connector cannot approve another connector or the unprefixed server.

### Desktop catalog

The Tauri shell exposes `list_connectors`, `add_connector`,
`set_connector_enabled`, `set_connector_tools` and `inspect_connector`. The
Connectors panel shows readiness badges ("Ready", "Disabled", "Missing
`CLOUDFLARE_API_TOKEN`"), lets a user add a preset, toggle a connector, pick
tools (write tools tagged) and start a configured stdio server to read its
advertised tool names. Writes go through a dedicated command that refuses a
symlinked `.ryoko` directory; `.ryoko` itself remains outside normal file editing.
