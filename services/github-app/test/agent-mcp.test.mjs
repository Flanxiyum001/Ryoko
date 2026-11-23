import assert from 'node:assert/strict';
import { test } from 'node:test';
import { createAgentMcpHandler, GITHUB_AGENT_TOOL } from '../src/agent-mcp.mjs';

const scopedEnv = {
  RYOKO_AGENT_BROKER_TOKEN: 'broker-secret-value-kept-in-host-env-only',
  RYOKO_GITHUB_INSTALLATION_ID: '17',
  RYOKO_GITHUB_REPO_ID: '41',
  RYOKO_GITHUB_BROKER_URL: 'http://127.0.0.1:4173',
};
const packet = (method, params, id = 1) => ({ jsonrpc: '2.0', id, method, params });

test('MCP initializes, advertises exactly one proposal-only tool and ignores notifications', async () => {
  const handler = createAgentMcpHandler({ env: scopedEnv, fetchImpl: () => { throw Error('network forbidden'); } });
  const initialized = await handler(packet('initialize', {}));
  assert.equal(initialized.result.protocolVersion, '2025-06-18');
  const tools = await handler(packet('tools/list', {}, 2));
  assert.deepEqual(tools.result.tools.map((tool) => tool.name), ['queue_github_proposal']);
  assert.ok(GITHUB_AGENT_TOOL.description.includes('human approval'));
  assert.equal(await handler({ jsonrpc: '2.0', method: 'notifications/initialized' }), null);
  assert.equal((await handler(packet('merge', {}))).error.code, -32601);
});

test('MCP permanently binds request to host repository, and only queues', async () => {
  const sent = [];
  const handler = createAgentMcpHandler({
    env: scopedEnv,
    fetchImpl: async (url, options) => {
      sent.push({ url, options });
      return new Response(JSON.stringify({
        id: '12345678-1234-1234-1234-123456789012', status: 'pending',
      }), { status: 201 });
    },
  });
  const answer = await handler(packet('tools/call', {
    name: 'queue_github_proposal',
    arguments: { operation: 'create_branch',
      params: { branch: 'ryoko/agent-work', repoId: 999, installationId: 999 } },
  }));
  assert.equal(answer.result.isError, undefined);
  assert.match(answer.result.content[0].text, /pending_human_approval/);
  assert.equal(sent.length, 1);
  assert.equal(sent[0].url, 'http://127.0.0.1:4173/api/agent/proposals');
  assert.equal(sent[0].options.redirect, 'error');
  const body = JSON.parse(sent[0].options.body);
  assert.deepEqual(body, {
    installationId: 17, repoId: 41, operation: 'create_branch', params: { branch: 'ryoko/agent-work' },
  });
  assert.ok(!JSON.stringify(answer).includes(scopedEnv.RYOKO_AGENT_BROKER_TOKEN));
});

test('MCP rejects malicious actions, bad paths and fake repo selection without calling broker', async () => {
  let network = 0;
  const handler = createAgentMcpHandler({ env: scopedEnv, fetchImpl: async () => { network++; } });
  for (const args of [
    { operation: 'merge_pr', params: { number: 3 } },
    { operation: 'create_branch', params: { branch: 'main' } },
    { operation: 'commit_file', params: {
      branch: 'ryoko/work', path: '.github/workflows/ci.yml',
      content: 'unsafe', message: 'test',
    } },
    { operation: 'create_branch', params: { branch: 'ryoko/demo' }, repoId: 99 },
  ]) {
    const value = await handler(packet('tools/call', {
      name: 'queue_github_proposal', arguments: args,
    }));
    assert.equal(value.result.isError, true);
  }
  assert.equal(network, 0);
});

test('MCP treats broker rejections as unconfirmed, never as approval', async () => {
  const handler = createAgentMcpHandler({
    env: scopedEnv,
    fetchImpl: async () => new Response('{}', { status: 403 }),
  });
  const value = await handler(packet('tools/call', {
    name: 'queue_github_proposal',
    arguments: { operation: 'create_branch', params: { branch: 'ryoko/safe' } },
  }));
  assert.equal(value.result.isError, true);
  assert.match(value.result.content[0].text, /no GitHub write was executed/);
});

test('MCP requires authenticated broker origin and immutable host-owned scope', () => {
  for (const mutation of [
    { RYOKO_GITHUB_REPO_ID: '0' },
    { RYOKO_GITHUB_REPO_ID: '../../hack' },
    { RYOKO_AGENT_BROKER_TOKEN: 'short' },
    { RYOKO_GITHUB_BROKER_URL: 'http://evil.example' },
    { RYOKO_GITHUB_BROKER_URL: 'https://user:password@example.org/' },
    { RYOKO_GITHUB_BROKER_URL: 'http://127.0.0.1:4173/redirect' },
  ]) {
    assert.throws(() => createAgentMcpHandler({ env: { ...scopedEnv, ...mutation } }));
  }
});
