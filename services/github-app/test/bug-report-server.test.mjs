import assert from 'node:assert/strict';
import { after, before, test } from 'node:test';
import { createRyokoServer } from '../src/server.mjs';

let server;
let base;
let githubRequest;
const origin = 'tauri://localhost';

before(async () => {
  server = createRyokoServer({
    config: {
      origin: 'http://127.0.0.1:4173',
      bugReportToken: 'server-only-test-token',
      bugReportRepo: 'owner/repo',
      bugReportAllowedOrigins: [origin],
      bugReportFetch: async (url, options) => {
        githubRequest = { url, options };
        return new Response(JSON.stringify({
          number: 4,
          title: 'Report from desktop',
          html_url: 'https://github.com/owner/repo/issues/4',
        }), { status: 201 });
      },
    },
  });
  await new Promise((resolve, reject) => {
    server.once('error', reject);
    server.listen(0, '127.0.0.1', resolve);
  });
  base = 'http://127.0.0.1:' + server.address().port;
});

after(async () => {
  await new Promise((resolve) => server.close(resolve));
});

test('bug report preflight permits configured native desktop origins only', async () => {
  const allowed = await fetch(base + '/api/bug-report', {
    method: 'OPTIONS',
    headers: { Origin: origin, 'Access-Control-Request-Method': 'POST', 'Access-Control-Request-Headers': 'content-type' },
  });
  assert.equal(allowed.status, 204);
  assert.equal(allowed.headers.get('access-control-allow-origin'), origin);
  const denied = await fetch(base + '/api/bug-report', {
    method: 'OPTIONS', headers: { Origin: 'https://attacker.example' },
  });
  assert.equal(denied.status, 403);
});

test('bug report endpoint creates issue and returns safe link', async () => {
  const response = await fetch(base + '/api/bug-report', {
    method: 'POST',
    headers: { Origin: origin, 'Content-Type': 'application/json' },
    body: JSON.stringify({ title: 'Report from desktop', description: 'A problem occurred.', version: '0.1.0', os: 'Linux' }),
  });
  assert.equal(response.status, 201);
  assert.equal(response.headers.get('access-control-allow-origin'), origin);
  assert.deepEqual(await response.json(), {
    number: 4, url: 'https://github.com/owner/repo/issues/4', title: 'Report from desktop',
  });
  assert.equal(githubRequest.url, 'https://api.github.com/repos/owner/repo/issues');
  assert.equal(githubRequest.options.headers.Authorization, 'Bearer server-only-test-token');
  assert.equal(JSON.parse(githubRequest.options.body).labels[0], 'bug');
});

test('bug report route rejects non-JSON submissions', async () => {
  const response = await fetch(base + '/api/bug-report', {
    method: 'POST', headers: { Origin: origin, 'Content-Type': 'text/plain' }, body: 'nope',
  });
  assert.equal(response.status, 415);
});
