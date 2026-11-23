#!/usr/bin/env node
// One JSON-RPC message per line, matching Ryoko's bounded stdio MCP transport.
// This process has broker credentials but never exposes them in tool output.
import { createInterface } from 'node:readline';
import { createAgentMcpHandler } from './agent-mcp.mjs';

const MAX_LINE = 64 * 1024;
const handler = createAgentMcpHandler();
const reader = createInterface({ input: process.stdin, crlfDelay: Infinity, terminal: false });

for await (const line of reader) {
  if (Buffer.byteLength(line) > MAX_LINE) {
    process.stdout.write(JSON.stringify({
      jsonrpc: '2.0', id: null, error: { code: -32600, message: 'MCP frame too large' },
    }) + '\n');
    continue;
  }
  let response;
  try {
    const request = JSON.parse(line);
    response = await handler(request);
  } catch {
    response = {
      jsonrpc: '2.0', id: null, error: { code: -32700, message: 'invalid MCP JSON' },
    };
  }
  if (response !== null) process.stdout.write(JSON.stringify(response) + '\n');
}
