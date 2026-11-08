import http from 'node:http';
import { randomBytes, randomUUID } from 'node:crypto';
import { readFile, stat } from 'node:fs/promises';
import { resolve, isAbsolute } from 'node:path';
import { execFile } from 'node:child_process';
import { promisify } from 'node:util';
import { GitHubBroker } from './github.mjs';
import { isAllowedOrigin, validateProposal, verifyWebhookSignature } from './security.mjs';

const execFileAsync = promisify(execFile);
const BODY_LIMIT = 150_000;
const TTL = 2 * 60 * 60 * 1000;
const PROPOSAL_TTL = 10 * 60 * 1000;
const bad = (message, status = 400) => Object.assign(new Error(message), { status });
const random = () => randomBytes(32).toString('base64url');

function json(res, status, data) {
  res.writeHead(status, { 'Content-Type': 'application/json; charset=utf-8', 'Cache-Control': 'no-store' });
  res.end(JSON.stringify(data));
}
function redirect(res, location, cookie) {
  res.writeHead(302, { Location: location, ...(cookie ? { 'Set-Cookie': cookie } : {}) });
  res.end();
}
function cookieFor(id, secure) {
  return 'ryoko_session=' + id + '; Path=/; HttpOnly; SameSite=Lax' + (secure ? '; Secure' : '');
}
function cookieId(req) {
  const match = String(req.headers.cookie || '').match(/(?:^|;\s*)ryoko_session=([A-Za-z0-9_-]+)/);
  return match?.[1] || '';
}
async function readBody(req, limit = BODY_LIMIT) {
  const chunks = [];
  let total = 0;
  for await (const chunk of req) {
    total += chunk.length;
    if (total > limit) throw bad('request body too large', 413);
    chunks.push(chunk);
  }
  return Buffer.concat(chunks);
}
function safeError(error) {
  const code = error?.status;
  if (!Number.isInteger(code) || code < 400 || code >= 600) return { status: 500, message: 'internal error' };
  return { status: code, message: String(error.message).slice(0, 200) };
}
function summarizeWebhook(type, payload) {
  return {
    id: randomUUID(),
    type,
    action: typeof payload.action === 'string' ? payload.action.slice(0, 70) : 'updated',
    installationId: payload.installation?.id ?? null,
    repositoryId: payload.repository?.id ?? null,
    repository: String(payload.repository?.full_name || '').slice(0, 160),
    summary: String(payload.pull_request?.title || payload.issue?.title || payload.workflow_run?.name || type).slice(0, 180),
    timestamp: new Date().toISOString(),
  };
}

export function createRyokoServer({ broker, config, frontendPath, fetchImpl } = {}) {
  const settings = {
    appSlug: config?.appSlug || '',
    origin: config?.origin || 'http://127.0.0.1:4173',
    allowedUsers: (config?.allowedUsers || []).map((value) => String(value).toLowerCase()),
    webhookSecret: config?.webhookSecret || '',
    agentToken: config?.agentToken || '',
    agentRepoIds: (config?.agentRepoIds || []).map(Number),
    ryokoBinary: config?.ryokoBinary || '',
    projectRoot: config?.projectRoot || '',
  };
  const client = broker || new GitHubBroker(config || {});
  const pagesRoot = frontendPath || resolve('apps/desktop');
  const sessions = new Map();
  const proposals = new Map();
  const events = [];
  const deliveries = new Map();
  const subscribers = new Set();
  const isSecure = new URL(settings.origin).protocol === 'https:';

  function sessionFor(req) {
    const id = cookieId(req);
    const item = sessions.get(id);
    if (!item || Date.now() > item.expires) {
      sessions.delete(id);
      return null;
    }
    return item;
  }
  function requireUser(req) {
    const session = sessionFor(req);
    if (!session?.user || !session.token) throw bad('Connect your GitHub account first', 401);
    if (!settings.allowedUsers.includes(session.user.login.toLowerCase())) throw bad('user not allowed', 403);
    return session;
  }
  function csrf(req, session) {
    if (!session || !isAllowedOrigin(req.headers.origin, settings.origin)
      || typeof req.headers['x-ryoko-csrf'] !== 'string'
      || req.headers['x-ryoko-csrf'] !== session.csrf) {
      throw bad('invalid origin or CSRF token', 403);
    }
  }
  function addProposal(user, proposal, source = 'user') {
    if (proposals.size > 300) {
      for (const [id, item] of proposals) {
        if (item.status !== 'pending' || Date.now() > item.expires) proposals.delete(id);
        if (proposals.size <= 250) break;
      }
    }
    if (proposals.size >= 300) throw bad('proposal queue full', 429);
    const id = randomUUID();
    const item = {
      id, source, owner: user.login, proposal, status: 'pending',
      createdAt: new Date().toISOString(), expires: Date.now() + PROPOSAL_TTL,
    };
    proposals.set(id, item);
    return item;
  }
  function emit(event) {
    events.push(event);
    if (events.length > 200) events.shift();
    for (const client of subscribers) {
      if (client.installations.has(event.installationId)) {
        client.res.write('event: github\n' + 'data: ' + JSON.stringify(event) + '\n\n');
      }
    }
  }

  async function localMission() {
    if (!settings.ryokoBinary || !settings.projectRoot
      || !isAbsolute(settings.ryokoBinary) || !isAbsolute(settings.projectRoot)) {
      return { connected: false, sessions: [], note: 'Configure RYOKO_CLI_BIN and RYOKO_PROJECT_ROOT for local session data.' };
    }
    try {
      const result = await execFileAsync(settings.ryokoBinary, [
        'sessions', '--repo', settings.projectRoot, '--json',
      ], { cwd: settings.projectRoot, timeout: 8000, maxBuffer: 1_000_000, shell: false });
      const parsed = JSON.parse(result.stdout);
      return {
        connected: true,
        sessions: Array.isArray(parsed) ? parsed.slice(0, 200) : [],
        note: 'Worktree lifecycle data only; not a live per-agent execution trace.',
      };
    } catch {
      return { connected: false, sessions: [], note: 'Ryoko CLI sessions unavailable. Check binary and project path.' };
    }
  }

  async function route(req, res) {
    const url = new URL(req.url || '/', settings.origin);
    const path = url.pathname;

    if (req.method === 'GET' && path === '/health') {
      return json(res, 200, { service: 'ryoko-github-app', status: 'ok', configured: Boolean(client.configured) });
    }
    if (req.method === 'POST' && path === '/webhooks/github') {
      if (!settings.webhookSecret) throw bad('webhook secret not configured', 503);
      const body = await readBody(req, 1_000_000);
      if (!verifyWebhookSignature(body, req.headers['x-hub-signature-256'], settings.webhookSecret)) {
        throw bad('invalid webhook signature', 401);
      }
      const delivery = String(req.headers['x-github-delivery'] || '');
      if (!/^[A-Za-z0-9-]{16,80}$/.test(delivery)) throw bad('missing webhook delivery identifier');
      if (deliveries.has(delivery)) return json(res, 202, { accepted: true, duplicate: true });
      let payload;
      try { payload = JSON.parse(body.toString('utf8')); } catch { throw bad('invalid webhook JSON'); }
      const type = String(req.headers['x-github-event'] || '');
      const allowed = ['installation', 'installation_repositories', 'issues', 'issue_comment',
        'pull_request', 'pull_request_review', 'check_run', 'check_suite', 'workflow_run', 'push'];
      deliveries.set(delivery, Date.now());
      if (deliveries.size > 1000) deliveries.delete(deliveries.keys().next().value);
      if (allowed.includes(type) && payload.installation?.id) emit(summarizeWebhook(type, payload));
      return json(res, 202, { accepted: true });
    }
    if (req.method === 'GET' && path === '/auth/github/start') {
      if (!client.configured || settings.allowedUsers.length === 0) throw bad('GitHub App has not been configured', 503);
      let current = sessionFor(req);
      let id = cookieId(req);
      if (!current) {
        id = random();
        current = { csrf: random(), expires: Date.now() + TTL };
        sessions.set(id, current);
      }
      const state = random();
      current.oauth = { state, expires: Date.now() + 10 * 60 * 1000 };
      const authorize = new URL('https://github.com/login/oauth/authorize');
      authorize.searchParams.set('client_id', client.clientId);
      authorize.searchParams.set('state', state);
      authorize.searchParams.set('redirect_uri', settings.origin + '/auth/github/callback');
      return redirect(res, authorize.toString(), cookieFor(id, isSecure));
    }
    if (req.method === 'GET' && path === '/auth/github/callback') {
      const session = sessionFor(req);
      const state = url.searchParams.get('state');
      const code = url.searchParams.get('code');
      if (!session?.oauth || !state || state !== session.oauth.state || Date.now() > session.oauth.expires) {
        throw bad('invalid OAuth state', 403);
      }
      session.oauth = null; // one-shot
      if (!code) throw bad('missing GitHub OAuth code');
      const token = await client.exchangeCode(code, settings.origin + '/auth/github/callback');
      const user = await client.user(token);
      if (!settings.allowedUsers.includes(user.login.toLowerCase())) throw bad('this GitHub account is not allowed', 403);
      session.user = user;
      session.token = token; // server-side memory only
      session.csrf = random();
      session.expires = Date.now() + TTL;
      return redirect(res, '/');
    }
    if (req.method === 'GET' && path === '/api/session') {
      const session = sessionFor(req);
      return json(res, 200, {
        connected: Boolean(session?.user),
        user: session?.user || null,
        csrf: session?.user ? session.csrf : null,
        configured: Boolean(client.configured && settings.allowedUsers.length),
        installUrl: /^[A-Za-z0-9-]{1,100}$/.test(settings.appSlug)
          ? 'https://github.com/apps/' + settings.appSlug + '/installations/new' : null,
      });
    }
    if (req.method === 'POST' && path === '/api/disconnect') {
      const session = requireUser(req);
      csrf(req, session);
      sessions.delete(cookieId(req));
      return json(res, 200, { connected: false });
    }
    if (req.method === 'GET' && path === '/api/installations') {
      const session = requireUser(req);
      return json(res, 200, await client.installations(session.token));
    }
    if (req.method === 'GET' && path === '/api/repositories') {
      const session = requireUser(req);
      return json(res, 200, await client.repositories(session.token, url.searchParams.get('installationId')));
    }
    if (req.method === 'GET' && path === '/api/repository') {
      const session = requireUser(req);
      const result = await client.view(
        session.token, url.searchParams.get('installationId'), url.searchParams.get('repoId'),
        url.searchParams.get('view'), {
          path: url.searchParams.get('path'), ref: url.searchParams.get('ref'),
        },
      );
      return json(res, 200, result);
    }
    if (req.method === 'GET' && path === '/api/mission') {
      requireUser(req);
      return json(res, 200, await localMission());
    }
    if (req.method === 'GET' && path === '/api/proposals') {
      const session = requireUser(req);
      return json(res, 200, Array.from(proposals.values())
        .filter((item) => item.owner === session.user.login && Date.now() <= item.expires)
        .map((item) => ({ ...item, expires: undefined })));
    }
    if (req.method === 'POST' && path === '/api/proposals') {
      const session = requireUser(req);
      csrf(req, session);
      const proposal = validateProposal(JSON.parse((await readBody(req)).toString('utf8')));
      await client.authorizedRepo(session.token, proposal.installationId, proposal.repoId);
      return json(res, 201, addProposal(session.user, proposal));
    }
    if (req.method === 'POST' && path === '/api/agent/proposals') {
      const token = String(req.headers.authorization || '');
      if (!settings.agentToken || settings.agentToken.length < 32
        || token !== 'Bearer ' + settings.agentToken) throw bad('agent broker unauthorized', 401);
      const proposal = validateProposal(JSON.parse((await readBody(req)).toString('utf8')));
      if (!settings.agentRepoIds.includes(proposal.repoId)) throw bad('agent repo not allowlisted', 403);
      const owner = Array.from(sessions.values()).find(
        (s) => s.user && settings.allowedUsers.includes(s.user.login.toLowerCase()),
      );
      if (!owner) throw bad('user must connect before agents may request proposals', 409);
      await client.authorizedRepo(owner.token, proposal.installationId, proposal.repoId);
      return json(res, 201, addProposal(owner.user, proposal, 'agent'));
    }
    const proposalMatch = path.match(/^\/api\/proposals\/([a-f0-9-]{36})\/(approve|reject)$/);
    if (req.method === 'POST' && proposalMatch) {
      const session = requireUser(req);
      csrf(req, session);
      const item = proposals.get(proposalMatch[1]);
      if (!item || item.owner !== session.user.login) throw bad('proposal not found', 404);
      if (Date.now() > item.expires || item.status !== 'pending') throw bad('proposal expired or already resolved', 409);
      if (proposalMatch[2] === 'reject') {
        item.status = 'rejected';
        return json(res, 200, { id: item.id, status: item.status });
      }
      item.status = 'executing';
      try {
        const result = await client.execute(session.token, item.proposal);
        item.status = 'completed';
        item.result = result;
        return json(res, 200, { id: item.id, status: item.status, result });
      } catch (error) {
        item.status = 'failed'; // never automatically re-execute an ambiguous write
        item.error = safeError(error).message;
        throw error;
      }
    }
    if (req.method === 'GET' && path === '/api/events') {
      const session = requireUser(req);
      const installations = new Set((await client.installations(session.token)).map((item) => item.id));
      res.writeHead(200, {
        'Content-Type': 'text/event-stream; charset=utf-8', 'Cache-Control': 'no-store',
        Connection: 'keep-alive', 'X-Accel-Buffering': 'no',
      });
      res.write(': connected\n\n');
      for (const event of events.filter((item) => installations.has(item.installationId)).slice(-50)) {
        res.write('event: github\n' + 'data: ' + JSON.stringify(event) + '\n\n');
      }
      const subscriber = { res, installations };
      subscribers.add(subscriber);
      const heartbeat = setInterval(() => res.write(': heartbeat\n\n'), 25_000);
      req.on('close', () => { subscribers.delete(subscriber); clearInterval(heartbeat); });
      return;
    }

    const staticPaths = new Map([
      ['/', 'index.html'], ['/index.html', 'index.html'], ['/app.js', 'app.js'], ['/styles.css', 'styles.css'],
    ]);
    if (req.method === 'GET' && staticPaths.has(path)) {
      const file = resolve(pagesRoot, staticPaths.get(path));
      const stats = await stat(file);
      if (stats.size > 700_000) throw bad('static resource too large', 503);
      const data = await readFile(file);
      res.writeHead(200, {
        'Content-Type': path.endsWith('.js') ? 'application/javascript; charset=utf-8'
          : path.endsWith('.css') ? 'text/css; charset=utf-8' : 'text/html; charset=utf-8',
        'Cache-Control': 'no-cache',
      });
      res.end(data);
      return;
    }
    throw bad('not found', 404);
  }

  const server = http.createServer((req, res) => {
    res.setHeader('X-Content-Type-Options', 'nosniff');
    res.setHeader('X-Frame-Options', 'DENY');
    res.setHeader('Referrer-Policy', 'no-referrer');
    res.setHeader('Content-Security-Policy',
      "default-src 'self'; script-src 'self'; style-src 'self'; img-src 'self' https://avatars.githubusercontent.com; connect-src 'self'; frame-ancestors 'none'; base-uri 'none'; object-src 'none'");
    route(req, res).catch((error) => {
      const { status, message } = safeError(error);
      if (!res.headersSent) json(res, status, { error: message });
      else res.destroy();
    });
  });
  return server;
}
