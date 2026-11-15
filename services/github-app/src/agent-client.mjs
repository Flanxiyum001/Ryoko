#!/usr/bin/env node
// Trusted host-side adapter. The model must never see the broker bearer token.
// This script queues a proposal; it cannot approve or execute remote writes.
import { readFile, stat } from 'node:fs/promises';
import { resolve } from 'node:path';
import { validateProposal } from './security.mjs';

async function main() {
  const file = process.argv[2];
  if (!file || process.argv.length !== 3) {
    throw Error('Usage: node src/agent-client.mjs /absolute/path/to/approval-request.json');
  }
  const rawToken = process.env.RYOKO_AGENT_BROKER_TOKEN || '';
  if (rawToken.length < 32) throw Error('RYOKO_AGENT_BROKER_TOKEN must be set securely by the host');
  const endpoint = new URL(process.env.RYOKO_GITHUB_BROKER_URL || 'http://127.0.0.1:4173');
  const loopback = ['localhost', '127.0.0.1', '[::1]'].includes(endpoint.hostname);
  if (endpoint.protocol !== 'https:' && !(endpoint.protocol === 'http:' && loopback)) {
    throw Error('GitHub broker URL must be HTTPS or an explicit loopback development URL');
  }
  if (endpoint.username || endpoint.password || endpoint.search || endpoint.hash) {
    throw Error('GitHub broker URL must not embed credentials or query strings');
  }
  const absolute = resolve(file);
  const info = await stat(absolute);
  if (!info.isFile() || info.size > 120_000) throw Error('proposal JSON must be a file smaller than 120KB');
  const proposal = validateProposal(JSON.parse(await readFile(absolute, 'utf8')));
  const target = new URL('/api/agent/proposals', endpoint);
  const response = await fetch(target, {
    method: 'POST', redirect: 'error', signal: AbortSignal.timeout(15_000),
    headers: {
      Authorization: 'Bearer ' + rawToken,
      'Content-Type': 'application/json',
      Accept: 'application/json',
    },
    body: JSON.stringify(proposal),
  });
  if (!response.ok) throw Error('GitHub broker rejected the proposal (HTTP ' + response.status + ')');
  const result = await response.json();
  console.log(JSON.stringify({ id: result.id, status: result.status, operation: proposal.operation }));
  console.error('Proposal queued; a human must approve it in Ryoko Mission Control.');
}

main().catch((error) => {
  console.error('Ryoko GitHub proposal failed: ' + error.message);
  process.exitCode = 1;
});
