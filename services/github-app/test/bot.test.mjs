import assert from 'node:assert/strict';
import { createHmac, randomUUID } from 'node:crypto';
import { test } from 'node:test';
import { issueBotCommand, postBotComment, botStatus } from '../src/bot.mjs';
import { createRyokoServer } from '../src/server.mjs';

const baseIssue = {
  action: 'created',
  installation: { id: 17 },
  repository: { id: 41, full_name: 'approved-owner/repo', default_branch: 'main' },
  issue: { number: 9, state: 'open', title: 'Improve editor', body: 'Add a file tree.' },
  sender: { login: 'approved-owner' },
  comment: { user: { login: 'approved-owner', type: 'User' }, body: '/ryoko run' },
};

test('bot accepts only exact commands from an allowlisted GitHub actor', () => {
  for (const [body, command] of [
    ['/ryoko run', 'run'], ['/ryoko status', 'status'], ['/ryoko retry', 'retry'],
  ]) {
    assert.equal(issueBotCommand('issue_comment', {
      ...baseIssue, comment: { ...baseIssue.comment, body },
    }, ['APPROVED-OWNER'])?.command, command);
  }
  assert.equal(issueBotCommand('issues', baseIssue, ['approved-owner']), null);
  assert.equal(issueBotCommand('issue_comment', { ...baseIssue, action: 'edited' }, ['approved-owner']), null);
  for (const body of ['/ryoko merge', '/ryoko run please', 'hello /ryoko run', '/ryoko run\nplease']) {
    assert.equal(issueBotCommand('issue_comment', {
      ...baseIssue, comment: { ...baseIssue.comment, body },
    }, ['approved-owner']), null);
  }
  assert.equal(issueBotCommand('issue_comment', baseIssue, ['someone-else']), null);
  assert.equal(issueBotCommand('issue_comment', {
    ...baseIssue, sender: { login: 'impersonator' },
  }, ['approved-owner', 'impersonator']), null);
  assert.equal(issueBotCommand('issue_comment', {
    ...baseIssue, comment: { ...baseIssue.comment, user: { login: 'approved-owner', type: 'Bot' } },
  }, ['approved-owner']), null);
  assert.equal(issueBotCommand('issue_comment', {
    ...baseIssue, issue: { ...baseIssue.issue, pull_request: { url: 'https://example.com' } },
  }, ['approved-owner']), null);
  assert.equal(issueBotCommand('issue_comment', {
    ...baseIssue, issue: { ...baseIssue.issue, state: 'closed' },
  }, ['approved-owner']), null);
  assert.equal(issueBotCommand('issue_comment', {
    ...baseIssue, issue: { ...baseIssue.issue, state: 'closed' },
    comment: { ...baseIssue.comment, body: '/ryoko status' },
  }, ['approved-owner'])?.command, 'status');
});

test('bot issue comments use an isolated issues-write token', async () => {
  const calls = [];
  const broker = {
    async installationToken(installationId, repoId, mode) {
      calls.push(['token', installationId, repoId, mode]);
      return 'issues-only';
    },
    async request(path, token, options) {
      calls.push(['request', path, token, options]);
      return { id: 101 };
    },
  };
  const request = issueBotCommand('issue_comment', baseIssue, ['approved-owner']);
  await postBotComment(broker, request, 'Safe update');
  assert.deepEqual(calls[0], ['token', 17, 41, 'issues']);
  assert.equal(calls[1][1], '/repos/approved-owner/repo/issues/9/comments');
  assert.equal(calls[1][2], 'issues-only');
  assert.deepEqual(calls[1][3], { method: 'POST', body: { body: 'Safe update' } });
  await assert.rejects(() => postBotComment(broker, request, 'x'.repeat(2001)), /invalid bot comment/);
});

test('bot status never claims a PR was merged', () => {
  const request = issueBotCommand('issue_comment', baseIssue, ['approved-owner']);
  assert.match(botStatus(request, null), /no recorded job/);
  assert.match(botStatus(request, { status: 'running' }), /working/);
  assert.match(botStatus(request, { status: 'failed' }), /\/ryoko retry/);
  const completed = botStatus(request, { status: 'completed', number: 42 });
  assert.match(completed, /https:\/\/github.com\/approved-owner\/repo\/pull\/42/);
  assert.match(completed, /manually merging/);
});

test('signed webhook starts one review-only bot run and posts result', async () => {
  const messages = [];
  let runCount = 0;
  let finishRun;
  const gate = new Promise((resolve) => { finishRun = resolve; });
  const broker = {
    async installationToken(_installationId, _repoId, mode) {
      assert.equal(mode, 'issues');
      return 'issues-only';
    },
    async request(path, token, options) {
      assert.equal(token, 'issues-only');
      assert.equal(path, '/repos/approved-owner/repo/issues/9/comments');
      messages.push(options.body.body);
      return { id: messages.length };
    },
  };
  const server = createRyokoServer({
    broker,
    config: {
      origin: 'http://127.0.0.1:4173',
      allowedUsers: ['approved-owner'],
      webhookSecret: 'test-signature-secret',
      agentRepoIds: [41],
      ryokoBinary: '/trusted/ryoko',
      projectsRoot: '/tmp',
    },
    runPipeline: async () => {
      runCount += 1;
      await gate;
      return { number: 72, branch: 'ryoko/issue-9' };
    },
  });
  await new Promise((resolve, reject) => {
    server.once('error', reject);
    server.listen(0, '127.0.0.1', resolve);
  });
  const address = 'http://127.0.0.1:' + server.address().port;
  async function send(event, payload, delivery = randomUUID(), validSignature = true) {
    const body = JSON.stringify(payload);
    const digest = createHmac('sha256', 'test-signature-secret').update(body).digest('hex');
    return fetch(address + '/webhooks/github', {
      method: 'POST',
      headers: {
        'X-GitHub-Event': event,
        'X-GitHub-Delivery': delivery,
        'X-Hub-Signature-256': 'sha256=' + (validSignature ? digest : '0'.repeat(64)),
      },
      body,
    });
  }
  async function until(predicate) {
    for (let i = 0; i < 100; i++) {
      if (predicate()) return;
      await new Promise((resolve) => setTimeout(resolve, 5));
    }
    assert.fail('bot did not reach expected state');
  }
  try {
    const bad = await send('issue_comment', baseIssue, randomUUID(), false);
    assert.equal(bad.status, 401);
    const delivery = randomUUID();
    assert.equal((await send('issue_comment', baseIssue, delivery)).status, 202);
    const duplicate = await send('issue_comment', baseIssue, delivery);
    assert.equal((await duplicate.json()).duplicate, true);
    await until(() => runCount === 1 && messages.length >= 1);

    const status = { ...baseIssue, comment: { ...baseIssue.comment, body: '/ryoko status' } };
    assert.equal((await send('issue_comment', status)).status, 202);
    await until(() => messages.some((message) => message.includes('is working')));
    assert.equal(runCount, 1);

    const retry = { ...baseIssue, comment: { ...baseIssue.comment, body: '/ryoko retry' } };
    assert.equal((await send('issue_comment', retry)).status, 202);
    assert.equal(runCount, 1, 'a running job may not be retried');

    finishRun();
    await until(() => messages.some((message) => message.includes('draft PR #72')));
    assert.equal(runCount, 1);
    assert.equal((await send('issue_comment', retry)).status, 202);
    await new Promise((resolve) => setTimeout(resolve, 10));
    assert.equal(runCount, 1, 'a completed job may not be retried');

    const forbidden = { ...baseIssue, sender: { login: 'other' },
      comment: { user: { login: 'other', type: 'User' }, body: '/ryoko run' } };
    assert.equal((await send('issue_comment', forbidden)).status, 202);
    assert.equal(runCount, 1, 'an untrusted actor cannot start work');
  } finally {
    finishRun();
    await new Promise((resolve) => server.close(resolve));
  }
});

test('native desktop starts an authorized issue job once and exposes its result', async () => {
  let runs = 0;
  const broker = {
    configured: true, clientId: 'Iv1.mock',
    async exchangeCode() { return 'owner-oauth-token'; },
    async user() { return { id: 3, login: 'approved-owner' }; },
    async installations() { return [{ id: 17 }]; },
    async repositories() { return [{ id: 41, full_name: 'approved-owner/repo', default_branch: 'main' }]; },
    async authorizedRepo(_token, installationId, repoId) {
      if (Number(installationId) !== 17 || Number(repoId) !== 41) {
        throw Object.assign(new Error('forbidden'), { status: 403 });
      }
      return { id: 41, full_name: 'approved-owner/repo', default_branch: 'main' };
    },
    async issue(_token, installationId, repoId, number) {
      await this.authorizedRepo(_token, installationId, repoId);
      return { installationId: 17, repoId: 41, owner: 'approved-owner', name: 'repo',
        fullName: 'approved-owner/repo', number, title: 'Improve search', body: 'Fix it',
        defaultBranch: 'main' };
    },
    async installationToken() { return 'issues-only'; },
    async request() { return { id: 10 }; },
  };
  const server = createRyokoServer({
    broker,
    config: {
      origin: 'http://127.0.0.1:4173',
      allowedUsers: ['approved-owner'], webhookSecret: 'secret',
      agentRepoIds: [41], ryokoBinary: '/trusted/ryoko', projectsRoot: '/tmp',
    },
    runPipeline: async () => {
      runs++;
      return { number: 55, branch: 'ryoko/integrated' };
    },
  });
  await new Promise((resolve, reject) => {
    server.once('error', reject);
    server.listen(0, '127.0.0.1', resolve);
  });
  const address = 'http://127.0.0.1:' + server.address().port;
  const request = (path, options = {}) => fetch(address + path, { redirect: 'manual', ...options });
  try {
    const auth = await request('/auth/github/start');
    const cookie = auth.headers.get('set-cookie').split(';')[0];
    const state = new URL(auth.headers.get('location')).searchParams.get('state');
    const callback = await request('/auth/github/callback?code=valid&state=' + state, {
      headers: { Cookie: cookie },
    });
    assert.equal(callback.status, 302);
    const session = await (await request('/api/session', { headers: { Cookie: cookie } })).json();
    const paired = await request('/api/desktop/pair', {
      method: 'POST',
      headers: { Cookie: cookie, Origin: 'http://127.0.0.1:4173', 'X-Ryoko-CSRF': session.csrf },
    });
    const { code } = await paired.json();
    const desktop = await request('/api/desktop/exchange', {
      method: 'POST', headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ code }),
    });
    const { token } = await desktop.json();
    const headers = { Authorization: 'Bearer ' + token, 'Content-Type': 'application/json' };
    const details = await request('/api/desktop/issue?installationId=17&repoId=41&number=9', {
      headers,
    });
    assert.equal(details.status, 200);
    assert.equal((await details.json()).title, 'Improve search');
    const body = JSON.stringify({ installationId: 17, repoId: 41, number: 9 });
    assert.equal((await request('/api/desktop/jobs', { method: 'POST', headers, body })).status, 202);
    assert.equal((await request('/api/desktop/jobs', { method: 'POST', headers, body })).status, 409);
    for (let attempt = 0; attempt < 50 && runs !== 1; attempt++) {
      await new Promise((resolve) => setTimeout(resolve, 5));
    }
    assert.equal(runs, 1);
    const list = await request('/api/desktop/jobs?installationId=17&repoId=41', { headers });
    const jobs = (await list.json()).jobs;
    assert.equal(jobs[0].issue, 9);
    assert.equal(jobs[0].status, 'completed');
    assert.equal(jobs[0].number, 55);
    assert.equal((await request('/api/desktop/jobs', {
      method: 'POST', headers, body: JSON.stringify({ installationId: 17, repoId: 99, number: 9 }),
    })).status, 403);
  } finally {
    await new Promise((resolve) => server.close(resolve));
  }
});
