import assert from 'node:assert/strict';
import { createHmac, generateKeyPairSync, verify } from 'node:crypto';
import { after, before, test } from 'node:test';
import { mkdtemp, readFile, rm, stat } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { cloneWithInstallationToken } from '../src/clone.mjs';
import { createRyokoServer } from '../src/server.mjs';
import { GitHubBroker } from '../src/github.mjs';
import {
  positiveId, safeBranch, safePath, safeProjectName, signAppJwt,
  validateProposal, verifyWebhookSignature,
} from '../src/security.mjs';

test('webhooks require correctly signed raw bytes', () => {
  const payload = Buffer.from('Hello, World!');
  const known = 'sha256=757107ea0eb2509fc211221cce984b8a37570b6d7586c22c46f4379c8b043e17';
  assert.equal(verifyWebhookSignature(payload, known, "It's a Secret to Everybody"), true);
  assert.equal(verifyWebhookSignature(Buffer.from('Hello, world!'), known, "It's a Secret to Everybody"), false);
  assert.equal(verifyWebhookSignature(payload, 'sha256=bad', 'secret'), false);
  assert.equal(verifyWebhookSignature(payload, known, ''), false);
});

test('installation App JWT is signed with RS256 and expires in under 10 minutes', () => {
  const { privateKey, publicKey } = generateKeyPairSync('rsa', { modulusLength: 2048 });
  const jwt = signAppJwt('321', privateKey, 1_800_000_000);
  const [header, body, signature] = jwt.split('.');
  assert.deepEqual(JSON.parse(Buffer.from(header, 'base64url').toString()), { alg: 'RS256', typ: 'JWT' });
  const claims = JSON.parse(Buffer.from(body, 'base64url').toString());
  assert.equal(claims.iss, '321');
  assert.ok(claims.exp - claims.iat <= 600);
  assert.equal(verify('RSA-SHA256', Buffer.from(header + '.' + body), publicKey, Buffer.from(signature, 'base64url')), true);
});

test('agent proposals are constrained to safe branch and repository operations', () => {
  assert.equal(safeBranch('ryoko/feature-1'), 'ryoko/feature-1');
  for (const ref of ['main', 'ryoko/../main', 'ryoko/feature.lock', 'ryoko/f//g', 'ryoko/']) {
    assert.throws(() => safeBranch(ref));
  }
  for (const path of ['../secret', '.github/workflows/build.yml', 'src/.env.production', 'src/link.key', 'C:\\Users\\secrets', '.git/config']) {
    assert.throws(() => safePath(path));
  }
  const approvedShape = validateProposal({
    installationId: 18, repoId: 2, operation: 'commit_file',
    params: { branch: 'ryoko/feature', path: 'src/index.ts', content: 'export const hi = true;', message: 'feat: add file' },
  });
  assert.equal(approvedShape.params.path, 'src/index.ts');
  assert.throws(() => validateProposal({ installationId: 1, repoId: 2, operation: 'merge_pr', params: {} }));
  assert.throws(() => validateProposal({
    installationId: 1, repoId: 2, operation: 'commit_file',
    params: { branch: 'ryoko/a', path: 'src/x', content: 'x'.repeat(120_000), message: 'test' },
  }));
  assert.throws(() => positiveId('3/4'));
});

test('repository clones use a private askpass helper without tokens in argv', async () => {
  const root = await mkdtemp(join(tmpdir(), 'ryoko-clone-test-'));
  let destination;
  try {
    const result = await cloneWithInstallationToken({
      projectsRoot: root,
      projectName: 'sample_workspace',
      repo: { full_name: 'trusted/example' },
      token: 'a-test-installation-token-not-for-display',
      runner: async (command, args, options) => {
        assert.equal(command, 'git');
        assert.equal(args[args.length - 2], 'https://github.com/trusted/example.git');
        assert.ok(!JSON.stringify(args).includes('a-test-installation-token'));
        assert.equal(options.shell, false);
        assert.equal(options.env.RYOKO_GIT_CLONE_TOKEN, 'a-test-installation-token-not-for-display');
        const helper = await readFile(options.env.GIT_ASKPASS, 'utf8');
        assert.ok(helper.includes('RYOKO_GIT_CLONE_TOKEN'));
        assert.ok(!helper.includes('a-test-installation-token-not-for-display'));
        destination = args[args.length - 1];
      },
    });
    assert.equal(result.path, destination);
    assert.equal((await stat(destination)).isDirectory(), true);
    assert.throws(() => safeProjectName('../escape'));
    assert.throws(() => safeProjectName('CON'));
  } finally {
    await rm(root, { recursive: true, force: true });
  }
});

test('installation tokens are scoped to exactly one repository and explicit privileges', async () => {
  const { privateKey } = generateKeyPairSync('rsa', { modulusLength: 2048 });
  const requests = [];
  const broker = new GitHubBroker({
    appId: 23, clientId: 'Iv1.test', clientSecret: 'mock', privateKey,
    fetchImpl: async (url, options) => {
      requests.push({ url, options });
      return new Response(JSON.stringify({
        token: 'test-installation-token', expires_at: new Date(Date.now() + 3600_000).toISOString(),
      }), { status: 201, headers: { 'content-type': 'application/json' } });
    },
  });
  const token = await broker.installationToken(42, 88, 'contents');
  assert.equal(token, 'test-installation-token');
  assert.equal(requests.length, 1);
  assert.match(requests[0].url, /\/app\/installations\/42\/access_tokens$/);
  assert.deepEqual(JSON.parse(requests[0].options.body), {
    repository_ids: [88], permissions: { contents: 'write' },
  });
  await broker.installationToken(42, 88, 'write');
  assert.equal(requests.length, 1, 'short-lived installation token should be cached');
});

const authenticatedUser = { login: 'approved-owner', id: 1 };
const fakeBroker = {
  configured: true,
  clientId: 'Iv1.mock',
  async exchangeCode() { return 'user-token'; },
  async user() { return authenticatedUser; },
  async installations() { return [{ id: 17, account: 'approved-owner' }]; },
  async repositories() { return [{ id: 41, name: 'repo', full_name: 'approved-owner/repo', default_branch: 'main' }]; },
  async authorizedRepo(_userToken, id, repoId) {
    if (Number(id) !== 17 || Number(repoId) !== 41) throw Object.assign(new Error('forbidden'), { status: 403 });
    return { id: 41, name: 'repo', full_name: 'approved-owner/repo', default_branch: 'main' };
  },
  async view() { return []; },
  executionCount: 0,
  async execute(_token, proposal) {
    this.executionCount++;
    return { action: proposal.operation, accepted: true };
  },
};

const server = createRyokoServer({
  broker: fakeBroker,
  frontendPath: new URL('../../../apps/desktop/', import.meta.url).pathname,
  config: {
    origin: 'http://127.0.0.1:4173',
    appSlug: 'ryoko',
    allowedUsers: ['approved-owner'],
    webhookSecret: 'super-secret',
  },
});
let base;
before(async () => {
  await new Promise((resolve, reject) => {
    server.once('error', reject);
    server.listen(0, '127.0.0.1', resolve);
  });
  base = 'http://127.0.0.1:' + server.address().port;
});
after(async () => {
  await new Promise((resolve) => server.close(resolve));
});

async function req(path, { method = 'GET', sessionCookie, csrf, body, headers = {} } = {}) {
  return fetch(base + path, {
    method,
    redirect: 'manual',
    headers: {
      ...(sessionCookie ? { Cookie: sessionCookie } : {}),
      ...(csrf ? { Origin: 'http://127.0.0.1:4173', 'X-Ryoko-CSRF': csrf } : {}),
      ...(body === undefined ? {} : { 'Content-Type': 'application/json' }),
      ...headers,
    },
    ...(body === undefined ? {} : { body: JSON.stringify(body) }),
  });
}

async function login() {
  const start = await req('/auth/github/start');
  assert.equal(start.status, 302);
  const cookie = start.headers.get('set-cookie').split(';')[0];
  const oauth = new URL(start.headers.get('location'));
  const callback = await req('/auth/github/callback?code=approved-code&state=' + oauth.searchParams.get('state'), {
    sessionCookie: cookie,
  });
  assert.equal(callback.status, 302);
  const session = await (await req('/api/session', { sessionCookie: cookie })).json();
  assert.equal(session.connected, true);
  return { cookie, csrf: session.csrf };
}

test('server rejects unauthenticated APIs and forged OAuth callbacks', async () => {
  assert.equal((await req('/api/repositories?installationId=17')).status, 401);
  assert.equal((await req('/auth/github/callback?code=fake&state=invalid')).status, 403);
  const health = await (await req('/health')).json();
  assert.equal(health.status, 'ok');
});

test('authenticated approvals require CSRF, validate ownership, and execute only once', async () => {
  const { cookie, csrf } = await login();
  const proposal = {
    installationId: 17, repoId: 41, operation: 'create_branch',
    params: { branch: 'ryoko/example' },
  };
  const unauth = await req('/api/proposals', { method: 'POST', sessionCookie: cookie, body: proposal });
  assert.equal(unauth.status, 403);
  const wrongRepo = await req('/api/proposals', { method: 'POST', sessionCookie: cookie, csrf, body: { ...proposal, repoId: 999 } });
  assert.equal(wrongRepo.status, 403);
  const pending = await req('/api/proposals', { method: 'POST', sessionCookie: cookie, csrf, body: proposal });
  assert.equal(pending.status, 201);
  const entry = await pending.json();
  assert.equal(entry.status, 'pending');
  assert.equal(fakeBroker.executionCount, 0);
  const approved = await req('/api/proposals/' + entry.id + '/approve', { method: 'POST', sessionCookie: cookie, csrf });
  assert.equal(approved.status, 200);
  assert.equal(fakeBroker.executionCount, 1);
  const repeat = await req('/api/proposals/' + entry.id + '/approve', { method: 'POST', sessionCookie: cookie, csrf });
  assert.equal(repeat.status, 409);
  assert.equal(fakeBroker.executionCount, 1);
  const disconnected = await req('/api/disconnect', { method: 'POST', sessionCookie: cookie, csrf });
  assert.equal(disconnected.status, 200);
  assert.equal((await req('/api/proposals', { sessionCookie: cookie })).status, 401);
});

test('webhook delivers only signature-verified requests and deduplicates event IDs', async () => {
  const payload = JSON.stringify({
    action: 'opened', installation: { id: 17 },
    repository: { id: 41, full_name: 'approved-owner/repo' },
    pull_request: { title: 'new work' },
  });
  const signature = 'sha256=' + createHmac('sha256', 'super-secret').update(payload).digest('hex');
  const headers = { 'X-GitHub-Delivery': '12345678-1234-1234-1234-123456789012', 'X-GitHub-Event': 'pull_request' };
  const invalid = await req('/webhooks/github', {
    method: 'POST', headers: { ...headers, 'X-Hub-Signature-256': 'sha256=' + '0'.repeat(64) },
    body: JSON.parse(payload),
  });
  assert.equal(invalid.status, 401);
  const valid = await req('/webhooks/github', {
    method: 'POST', headers: { ...headers, 'X-Hub-Signature-256': signature }, body: JSON.parse(payload),
  });
  assert.equal(valid.status, 202);
  const duplicate = await req('/webhooks/github', {
    method: 'POST', headers: { ...headers, 'X-Hub-Signature-256': signature }, body: JSON.parse(payload),
  });
  assert.equal(duplicate.status, 202);
  assert.equal((await duplicate.json()).duplicate, true);
});
