# MCP developer-tool client (operator-controlled alpha)

\`ryoko-core::mcp\` owns the MCP JSON-RPC client, schema and response
validation, pagination limits and mandatory \`McpApprovalGate\`. The CLI
implements a bounded **stdio** transport. This is a deliberately narrow MCP
integration: it is **not** a fully autonomous agent tool loop.

## List tools from a trusted server

\`\`\`sh
cargo run -p ryoko-cli -- mcp --server node \
  --server-arg ./path/to/trusted-mcp-server.mjs tools
\`\`\`

## Invoke a tool with human confirmation

\`\`\`sh
cargo run -p ryoko-cli -- mcp --server node \
  --server-arg ./path/to/trusted-mcp-server.mjs \
  call --tool list_issues --arguments '{"state":"open"}'
\`\`\`

The CLI prints the chosen tool and its exact argument JSON. It invokes the
tool **only** if the operator types \`YES\` for that invocation. A fresh
approval is required for every call. Server tools cannot grant themselves
permissions, and listing tools does not imply approval.

Servers may need credentials. The child gets a minimal PATH/HOME/temporary
directory environment; all other variables are denied by default. Explicitly
forward only the variable a reviewed server requires:

\`\`\`sh
cargo run -p ryoko-cli -- mcp --server node \
  --server-arg ./path/to/trusted-mcp-server.mjs \
  --allow-env GITHUB_TOKEN tools
\`\`\`

Environment variable **names**, not secret values, are passed on the
command line. Never select a server executable from model output and never
run an untrusted MCP server without a stronger process sandbox. MCP server
output may contain untrusted/prompt-injection content and is not fed into
agent prompts by this integration.

## Security boundaries

- Legacy MCP initialize/initialized handshake and \`tools/list\`,
  \`tools/call\` over newline-delimited JSON-RPC 2.0 stdio.
- Protocol negotiation supports 2025-03-26 and 2025-06-18. Other versions
  fail rather than silently guessing framing or tool semantics.
- Requests capped at 64 KiB; response frames and results at 1 MiB;
  max 256 advertised tools and 16 pagination pages.
- 15-second per-message timeout; child killed when the transport drops.
- No shell is invoked to run the server; the server binary and argument
  array are explicitly provided by the operator.
- Tool calls require human approval through a trusted host-owned gate.
- Remote Streamable HTTP, OAuth, multi-session persistence, sampling and
  elicitation are **not** implemented.

Next: integrate the gate into Tauri IPC with native approval dialogs, then
inject *read-only* MCP tool descriptions into an agent turn. Never permit an
agent to fabricate an approval gate or execute MCP calls autonomously without
that trusted host implementation.
