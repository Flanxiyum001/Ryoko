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
