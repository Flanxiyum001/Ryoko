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

The CLI prints the chosen tool and its exact argument JSON. It invokes the
tool **only** if the operator types `YES` for that invocation. A fresh
approval is required for every call. Server tools cannot grant themselves
permissions, and listing tools does not imply approval.

Servers may need credentials. The child gets a minimal PATH/HOME/temporary
directory environment; all other variables are denied by default. Explicitly
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
- Tool calls require human approval through a trusted host-owned gate.
- Agent tools require a non-empty host allowlist; the server catalogue cannot
  add tools to it. Call count and argument size are host-bounded.
- Remote Streamable HTTP, OAuth, multi-session persistence, sampling and
  elicitation are **not** implemented. The desktop approval dialog is still
  pending; the CLI uses its terminal gate.

Next: integrate the gate into Tauri IPC with native approval dialogs. Never
permit an agent to fabricate an approval gate or execute MCP calls without that
trusted host implementation.
