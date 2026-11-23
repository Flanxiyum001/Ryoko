import assert from 'node:assert/strict';
import { createHmac, randomBytes } from 'node:crypto';
import { mkdtempSync, rmSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { test } from 'node:test';
import { createRyokoServer } from '../src/server.mjs';
import { ProductionState } from '../src/production-state.mjs';
import { startGithubWorker } from '../src/worker-loop.mjs';

const config = (file, key, root) => ({
  origin: 'http://127.0.0.1:4173', allowedUsers: ['approved-owner'],
  webhookSecret: 'trusted-webhook-secret',
  agentRepoIds: [41], ryokoBinary: '/not-executed/ryoko', projectsRoot: root,
  stateDbFile: file, stateKey: key, workerMode: 'api',
});
const broker = {
  configured: true, clientId: 'Iv1.test',
  async exchangeCode() { return 'token-only-inside-encrypted-session'; },
  async user() { return { login: 'approved-owner', id: 12 }; },
  async authorizedRepo(_token, installation, repoId) {
    if (Number(installation) !== 17 || Number(repoId) !== 41) {
      throw Object.assign(new Error('forbidden'), { status: 403 });
    }
    return { id: 41, full_name: 'approved-owner/repo', default_branch: 'main' };
  },
  async installationToken() { return 'scoped-test-token'; },
  async request() { return { id: 100 }; },
};
async function listen(server) {
  await new Promise((resolve, reject) => {
    server.once('error', reject);
    server.listen(0, '127.0.0.1', resolve);
  });
  return 'http://127.0.0.1:' + server.address().port;
}
const post = (url, data, extra = {}) => fetch(url, {
  method: 'POST', redirect: 'manual',
  headers: { 'Content-Type': 'application/json', ...extra },
  body: JSON.stringify(data),
});

test('separate HTTP brokers share OAuth, one-shot desktop pairing, webhooks and worker jobs', async () => {
  const dir = mkdtempSync(join(tmpdir(), 'ryoko-shared-broker-'));
  const file = join(dir, 'state.sqlite3');
  const key = randomBytes(32).toString('base64url');
  const settings = config(file, key, dir);
  const apiA = createRyokoServer({ broker, config: settings });
  const apiB = createRyokoServer({ broker, config: settings });
  const stateA = new ProductionState(file, key);
  const stateB = new ProductionState(file, key);
  const baseA = await listen(apiA);
  const baseB = await listen(apiB);
  let w1, w2;
  try {
    const start = await fetch(baseA + '/auth/github/start', { redirect: 'manual' });
    const cookie = start.headers.get('set-cookie').split(';')[0];
    const oauth = new URL(start.headers.get('location'));
    const callback = await fetch(baseB
      + '/auth/github/callback?code=valid&state=' + oauth.searchParams.get('state'), {
      redirect: 'manual', headers: { Cookie: cookie },
    });
    assert.equal(callback.status, 302, 'OAuth callback works on another HTTP replica');
    const session = await (await fetch(baseA + '/api/session', { headers: { Cookie: cookie } })).json();
    assert.equal(session.connected, true);
    assert.equal(session.user.login, 'approved-owner');

    const paired = await post(baseA + '/api/desktop/pair', null, {
      Cookie: cookie, Origin: 'http://127.0.0.1:4173', 'X-Ryoko-CSRF': session.csrf,
    });
    assert.equal(paired.status, 201);
    const { code } = await paired.json();
    const exchanged = await post(baseB + '/api/desktop/exchange', { code });
    assert.equal(exchanged.status, 200);
    const { token } = await exchanged.json();
    assert.equal((await post(baseA + '/api/desktop/exchange', { code })).status, 401);
    const headers = { Authorization: 'Bearer ' + token };

    const verifiedRepo = await fetch(
      baseB + '/api/desktop/authorized-repository?installationId=17&repoId=41',
      { headers },
    );
    assert.equal(verifiedRepo.status, 200);
    assert.equal((await verifiedRepo.json()).repository, 'approved-owner/repo');
    const goal = await post(baseA + '/api/desktop/goals', {
      installationId: 17, repoId: 41, goal: 'Implement better tests for the editor',
    }, headers);
    assert.equal(goal.status, 202);
    assert.equal((await goal.json()).kind, 'goal');
    assert.equal((await post(baseB + '/api/desktop/goals', {
      installationId: 17, repoId: 41, goal: 'Implement better tests for the editor',
    }, headers)).status, 409);

    const payload = {
      action: 'labeled', label: { name: 'ryoko-fix' },
      sender: { login: 'approved-owner' },
      installation: { id: 17 },
      repository: { id: 41, full_name: 'approved-owner/repo', default_branch: 'main' },
      issue: { number: 9, state: 'open', title: 'Fix search',
        body: 'Correct the search bug', labels: [{ name: 'ryoko-fix' }] },
    };
    const bytes = JSON.stringify(payload);
    const signature = 'sha256=' + createHmac('sha256', settings.webhookSecret).update(bytes).digest('hex');
    const webhookHeaders = {
      'X-Hub-Signature-256': signature,
      'X-GitHub-Delivery': 'deadbeef-1234-1234-1234-123456789012',
      'X-GitHub-Event': 'issues',
      'Content-Type': 'application/json',
    };
    const first = await fetch(baseA + '/webhooks/github', {
      method: 'POST', headers: webhookHeaders, body: bytes,
    });
    assert.equal(first.status, 202);
    assert.equal((await first.json()).queued, true);
    const duplicate = await fetch(baseB + '/webhooks/github', {
      method: 'POST', headers: webhookHeaders, body: bytes,
    });
    assert.equal((await duplicate.json()).duplicate, true);
    assert.equal(stateA.jobs.get('41:9').status, 'queued');

    const executions = [];
    const runner = async ({ request }) => {
      executions.push(request.kind || 'issue');
      return { number: request.kind === 'goal' ? 70 : 71, branch: 'ryoko/approved' };
    };
    w1 = startGithubWorker({ store: stateA, broker, settings, runPipeline: runner, pollMs: 15 });
    w2 = startGithubWorker({ store: stateB, broker, settings, runPipeline: runner, pollMs: 15 });
    for (let i = 0; i < 100 && stateA.listJobs(41).some((job) => job.status !== 'completed'); i++) {
      await new Promise((resolve) => setTimeout(resolve, 25));
    }
    assert.deepEqual(executions.sort(), ['goal', 'issue']);
    const jobs = await fetch(baseB + '/api/desktop/jobs?installationId=17&repoId=41', { headers });
    assert.equal(jobs.status, 200);
    assert.deepEqual((await jobs.json()).jobs.map((job) => job.number).sort(), [70, 71]);

    await post(baseB + '/api/disconnect', null, {
      Cookie: cookie, Origin: 'http://127.0.0.1:4173', 'X-Ryoko-CSRF': session.csrf,
    });
    assert.equal((await fetch(baseA + '/api/desktop/jobs?installationId=17&repoId=41', {
      headers,
    })).status, 401);
  } finally {
    await Promise.all([w1?.stop(), w2?.stop()]);
    await Promise.all([apiA, apiB].map((server) => new Promise((resolve) => server.close(resolve))));
    stateA.close(); stateB.close();
    rmSync(dir, { recursive: true, force: true });
  }
});
