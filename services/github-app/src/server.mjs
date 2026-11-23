import http from 'node:http';
import { randomBytes, randomUUID, timingSafeEqual } from 'node:crypto';
import { readFile, stat } from 'node:fs/promises';
import { resolve, isAbsolute } from 'node:path';
import { execFile } from 'node:child_process';
import { promisify } from 'node:util';
import { GitHubBroker } from './github.mjs';
import { issueAgentRequest, runIssuePipeline } from './pipeline.mjs';
import { issueBotCommand, postBotComment, botStatus } from './bot.mjs';
import { BotJobStore } from './durable-queue.mjs';
import { ApprovalStore } from './approval-store.mjs';
import { ProductionState } from './production-state.mjs';
import { startGithubWorker } from './worker-loop.mjs';
import { isAllowedOrigin, positiveId, validateProposal, verifyWebhookSignature } from './security.mjs';

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
async function jsonBody(req) {
  const bytes = await readBody(req);
  try { return JSON.parse(bytes.toString('utf8')); }
  catch { throw bad('invalid JSON request body'); }
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

export function createRyokoServer({ broker, config, frontendPath, fetchImpl, runPipeline = runIssuePipeline } = {}) {
  const settings = {
    appSlug: config?.appSlug || '',
    origin: config?.origin || 'http://127.0.0.1:4173',
    allowedUsers: (config?.allowedUsers || []).map((value) => String(value).toLowerCase()),
    webhookSecret: config?.webhookSecret || '',
    agentToken: config?.agentToken || '',
    agentRepoIds: (config?.agentRepoIds || []).map(Number),
    ryokoBinary: config?.ryokoBinary || '',
    projectRoot: config?.projectRoot || '',
    projectsRoot: config?.projectsRoot || '',
    botStateFile: config?.botStateFile || '',
    approvalStateFile: config?.approvalStateFile || '',
    stateDbFile: config?.stateDbFile || '',
    stateKey: config?.stateKey || '',
    workerMode: config?.workerMode || 'all',
    pipelineTimeoutMs: config?.pipelineTimeoutMs || 15 * 60_000,
    // Operator-owned MCP settings for pipeline runs. Absent means no MCP tools.
    mcp: config?.mcp && typeof config.mcp === 'object' ? config.mcp : null,
  };
  const client = broker || new GitHubBroker(config || {});
  const pagesRoot = frontendPath || resolve('apps/desktop');
  const stateDb = settings.stateDbFile
    ? new ProductionState(settings.stateDbFile, settings.stateKey) : null;
  const sessions = stateDb?.sessions || new Map();
  const approvalStore = stateDb?.approvalStore || new ApprovalStore(settings.approvalStateFile);
  const proposals = approvalStore.proposals;
  const events = [];
  const botState = stateDb || new BotJobStore(settings.botStateFile);
  const deliveries = botState.deliveries;
  const subscribers = new Set();
  const desktopPairings = stateDb?.desktopPairings || new Map();
  const desktopSessions = stateDb?.desktopSessions || new Map();
  const pipelineJobs = botState.jobs;
  if (stateDb && !['api','worker','all'].includes(settings.workerMode)) {
    throw new Error('invalid GitHub worker mode');
  }
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
    approvalStore.prune();
    if (proposals.size >= 300) throw bad('proposal queue full', 429);
    const id = randomUUID();
    const item = {
      id, source, owner: user.login, proposal, status: 'pending',
      createdAt: new Date().toISOString(), expires: Date.now() + PROPOSAL_TTL,
    };
    return approvalStore.save(item);
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

  // Bot notifications are best-effort. A GitHub API outage must never approve
  // or repeat a write, and no backend exception details reach public comments.
  async function notifyIssue(request, message) {
    try {
      await postBotComment(client, request, message);
    } catch {
      console.warn('Ryoko GitHub bot could not deliver an issue update');
    }
  }

  function jobKey(request) {
    return String(request.repoId) + ':' + String(request.number);
  }

  function startJob(request, retry = false) {
    if (!settings.agentRepoIds.includes(request.repoId)) throw bad('repository not allowlisted', 403);
    if (!settings.ryokoBinary || !settings.projectsRoot) {
      throw bad('configure RYOKO_CLI_BIN and RYOKO_PROJECTS_ROOT before starting jobs', 503);
    }
    const key = jobKey(request);
    const previous = pipelineJobs.get(key);
    if (stateDb) return stateDb.enqueueJob(request, retry);
    if (retry ? !['failed', 'interrupted'].includes(previous?.status)
      : Boolean(previous)) throw bad('job already recorded; inspect status before retrying', 409);
    botState.pruneJobs();
    if (pipelineJobs.size >= 300 && !previous) throw bad('bot job queue full', 429);
    const startedAt = Date.now();
    botState.setJob(key, { status: 'running', startedAt });
    const event = {
      id: randomUUID(), type: 'pipeline', action: 'started',
      installationId: request.installationId,
      repositoryId: request.repoId,
      repository: request.fullName,
      summary: `Issue #${request.number}: ${request.title}`.slice(0, 180),
      timestamp: new Date().toISOString(),
    };
    emit(event);
    void (async () => {
      await notifyIssue(request,
        `Ryoko started work on issue #${request.number}. A successful run creates a **draft PR** only; a human must review and merge.`);
      try {
        const result = await runPipeline({ broker: client, request, settings });
        const finished = { status: 'completed', ...result, startedAt, finishedAt: Date.now() };
        botState.setJob(key, finished);
        emit({ ...event, id: randomUUID(), action: 'completed',
          summary: `Draft PR #${result.number} opened`, timestamp: new Date().toISOString() });
        await notifyIssue(request, botStatus(request, finished));
      } catch (error) {
        // Do not surface untrusted provider, Git or filesystem errors to GitHub.
        const failed = { status: 'failed', startedAt, finishedAt: Date.now() };
        try { botState.setJob(key, failed); }
        catch { console.error('Ryoko failed to persist job failure'); }
        emit({ ...event, id: randomUUID(), action: 'failed',
          summary: 'Engineering run failed; inspect the broker logs',
          timestamp: new Date().toISOString() });
        await notifyIssue(request, botStatus(request, failed));
      }
    })();
    return { status: 'running', issue: request.number, startedAt };
  }

  // Native desktop pairing is a short-lived, loopback-only broker bridge. GitHub OAuth
  // credentials remain in the Node broker, never in Tauri's webview.
  function requireLoopback(req) {
    if (!['127.0.0.1', '::1', '::ffff:127.0.0.1'].includes(req.socket.remoteAddress)) {
      throw bad('desktop bridge accepts loopback connections only', 403);
    }
  }
  function requireDesktop(req) {
    requireLoopback(req);
    const auth = String(req.headers.authorization || '');
    const match = /^Bearer ([A-Za-z0-9_-]{32,128})$/.exec(auth);
    if (!match) throw bad('desktop pairing required', 401);
    const desktop = desktopSessions.get(match[1]);
    if (!desktop || Date.now() > desktop.expires) {
      desktopSessions.delete(match[1]);
      throw bad('desktop session expired', 401);
    }
    const session = sessions.get(desktop.sessionId);
    if (!session || Date.now() > session.expires || !session.user || !session.token) {
      desktopSessions.delete(match[1]);
      throw bad('desktop GitHub sign-in expired', 401);
    }
    if (!settings.allowedUsers.includes(session.user.login.toLowerCase())) {
      throw bad('desktop user not permitted', 403);
    }
    return session;
  }
  function expireDesktopEntries() {
    if (stateDb) { stateDb.prune(); return; }
    for (const [code, item] of desktopPairings) {
      if (item.expires <= Date.now() || !sessions.has(item.sessionId)) desktopPairings.delete(code);
    }
    for (const [token, item] of desktopSessions) {
      if (item.expires <= Date.now() || !sessions.has(item.sessionId)) desktopSessions.delete(token);
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
      return json(res, 200, { service: 'ryoko-github-app', status: 'ok',
        configured: Boolean(client.configured), durable: Boolean(stateDb),
        workerMode: stateDb ? settings.workerMode : 'legacy' });
    }
    if (req.method === 'POST' && path === '/webhooks/github') {
      if (!settings.webhookSecret) throw bad('webhook secret not configured', 503);
      const body = await readBody(req, 1_000_000);
      if (!verifyWebhookSignature(body, req.headers['x-hub-signature-256'], settings.webhookSecret)) {
        throw bad('invalid webhook signature', 401);
      }
      const delivery = String(req.headers['x-github-delivery'] || '');
      if (!/^[A-Za-z0-9-]{16,80}$/.test(delivery)) throw bad('missing webhook delivery identifier');
      if (!stateDb && deliveries.has(delivery)) return json(res, 202, { accepted: true, duplicate: true });
      let payload;
      try { payload = JSON.parse(body.toString('utf8')); } catch { throw bad('invalid webhook JSON'); }
      const type = String(req.headers['x-github-event'] || '');
      const allowed = ['installation', 'installation_repositories', 'issues', 'issue_comment',
        'pull_request', 'pull_request_review', 'check_run', 'check_suite', 'workflow_run', 'push'];
      const command = issueBotCommand(type, payload, settings.allowedUsers);
      const labelRequest = issueAgentRequest(type, payload);
      const trustedLabel = labelRequest && settings.allowedUsers.includes(
        String(payload.sender?.login || '').toLowerCase()) ? labelRequest : null;
      const pipelineRequest = command?.command === 'status'
        ? null : (command || trustedLabel);
      const allowedRequest = pipelineRequest && settings.agentRepoIds.includes(pipelineRequest.repoId)
        ? pipelineRequest : null;

      if (stateDb) {
        // The signed delivery ID and any job are committed in ONE transaction.
        // Multiple HTTP replicas cannot acknowledge the same event twice.
        const accepted = stateDb.acceptWebhook(
          delivery, allowedRequest, command?.command === 'retry',
        );
        if (accepted.duplicate) return json(res, 202, { accepted: true, duplicate: true });
        if (allowed.includes(type) && payload.installation?.id) emit(summarizeWebhook(type, payload));
        if (command?.command === 'status' && settings.agentRepoIds.includes(command.repoId)) {
          void notifyIssue(command, botStatus(command, pipelineJobs.get(jobKey(command))));
        }
        if (accepted.queued && allowedRequest) {
          emit({ ...summarizeWebhook('pipeline', payload), action: 'queued',
            summary: 'Engineering job queued for worker' });
        } else if (command && accepted.refusal) {
          void notifyIssue(command, botStatus(command, pipelineJobs.get(jobKey(command))));
        }
        return json(res, 202, { accepted: true, queued: Boolean(accepted.queued) });
      }
      botState.recordDelivery(delivery);
      if (allowed.includes(type) && payload.installation?.id) emit(summarizeWebhook(type, payload));
      if (command?.command === 'status' && settings.agentRepoIds.includes(command.repoId)) {
        void notifyIssue(command, botStatus(command, pipelineJobs.get(jobKey(command))));
      }
      if (allowedRequest) {
        try {
          startJob(allowedRequest, command?.command === 'retry');
        } catch {
          if (command) void notifyIssue(command,
            botStatus(command, pipelineJobs.get(jobKey(command)))
            + ' Check broker configuration and existing branches before retrying.');
        }
      }
      return json(res, 202, { accepted: true });
    }
    if (req.method === 'GET' && path === '/auth/github/start') {
      if (!client.configured || settings.allowedUsers.length === 0) throw bad('GitHub App has not been configured', 503);
      let current = sessionFor(req);
      let id = cookieId(req);
      if (!current) {
        if (sessions.size >= 500) {
          if (stateDb) stateDb.prune();
          else for (const [key, record] of sessions) {
            if (Date.now() > record.expires) sessions.delete(key);
          }
        }
        if (sessions.size >= 500) throw bad('too many pending sign-in sessions', 429);
        id = random();
        current = { csrf: random(), expires: Date.now() + TTL };
        sessions.set(id, current);
      }
      const state = random();
      current.oauth = { state, expires: Date.now() + 10 * 60 * 1000 };
      sessions.set(id, current);
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
      sessions.set(cookieId(req), session); // one-shot across all broker processes
      if (!code) throw bad('missing GitHub OAuth code');
      const token = await client.exchangeCode(code, settings.origin + '/auth/github/callback');
      const user = await client.user(token);
      if (!settings.allowedUsers.includes(user.login.toLowerCase())) throw bad('this GitHub account is not allowed', 403);
      session.user = user;
      session.token = token; // server-side memory only
      session.csrf = random();
      session.expires = Date.now() + TTL;
      sessions.set(cookieId(req), session);
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
    // Creating a pairing code requires the signed-in browser cookie AND
    // anti-CSRF header. Pairing codes never appear in URLs or logs.
    if (req.method === 'POST' && path === '/api/desktop/pair') {
      requireLoopback(req);
      const session = requireUser(req);
      csrf(req, session);
      expireDesktopEntries();
      if (desktopPairings.size >= 20) throw bad('too many pending desktop pairings', 429);
      const code = random();
      desktopPairings.set(code, { sessionId: cookieId(req), expires: Date.now() + 5 * 60_000 });
      return json(res, 201, { code, expiresSeconds: 300 });
    }
    if (req.method === 'POST' && path === '/api/desktop/exchange') {
      requireLoopback(req);
      const body = await jsonBody(req);
      if (typeof body?.code !== 'string' || !/^[A-Za-z0-9_-]{43}$/.test(body.code)) {
        throw bad('invalid desktop pairing code', 401);
      }
      expireDesktopEntries();
      const item = stateDb ? stateDb.takePairing(body.code) : desktopPairings.get(body.code);
      if (!stateDb) desktopPairings.delete(body.code); // consume exactly once
      if (!item || Date.now() > item.expires) throw bad('desktop code expired', 401);
      const session = sessions.get(item.sessionId);
      if (!session || !session.token || !session.user
        || Date.now() > session.expires
        || !settings.allowedUsers.includes(session.user.login.toLowerCase())) {
        throw bad('desktop session unavailable', 401);
      }
      if (desktopSessions.size >= 50) throw bad('desktop session limit reached', 429);
      const token = random();
      desktopSessions.set(token, { sessionId: item.sessionId, expires: Date.now() + 30 * 60_000 });
      return json(res, 200, { token, expiresSeconds: 1800 });
    }
    if (req.method === 'GET' && path === '/api/desktop/repositories') {
      const session = requireDesktop(req);
      const installations = (await client.installations(session.token)).slice(0, 10);
      const repositories = [];
      for (const installation of installations) {
        const owned = (await client.repositories(session.token, installation.id)).slice(0, 30);
        for (const repo of owned) {
          repositories.push({
            installationId: installation.id,
            repoId: repo.id,
            name: String(repo.full_name || '').slice(0, 180),
          });
        }
      }
      return json(res, 200, { repositories: repositories.slice(0, 100) });
    }
    if (req.method === 'GET' && path === '/api/desktop/queue') {
      const session = requireDesktop(req);
      const installationId = url.searchParams.get('installationId');
      const repoId = url.searchParams.get('repoId');
      const authorized = await client.authorizedRepo(session.token, installationId, repoId);
      const [pulls, issues] = await Promise.all([
        client.view(session.token, installationId, repoId, 'pulls'),
        client.view(session.token, installationId, repoId, 'issues'),
      ]);
      return json(res, 200, {
        repository: String(authorized.full_name || '').slice(0, 180),
        pulls: Array.isArray(pulls) ? pulls.slice(0, 30) : [],
        issues: Array.isArray(issues) ? issues.slice(0, 30) : [],
      });
    }
    // Native IDE: the paired desktop may inspect CI, queue proposals and
    // explicitly approve/reject them. The bearer credential stays in Rust.
    if (req.method === 'GET' && path === '/api/desktop/checks') {
      const session = requireDesktop(req);
      const installationId = positiveId(url.searchParams.get('installationId'), 'installation');
      const repoId = positiveId(url.searchParams.get('repoId'), 'repository');
      const number = positiveId(url.searchParams.get('number'), 'pull request');
      return json(res, 200, await client.draftChecks(session.token, installationId, repoId, number));
    }
    if (req.method === 'GET' && path === '/api/desktop/jobs') {
      const session = requireDesktop(req);
      const installationId = positiveId(url.searchParams.get('installationId'), 'installation');
      const repoId = positiveId(url.searchParams.get('repoId'), 'repository');
      await client.authorizedRepo(session.token, installationId, repoId);
      return json(res, 200, { jobs: stateDb ? stateDb.listJobs(repoId) : [...pipelineJobs.entries()]
        .filter(([key]) => key.startsWith(repoId + ':'))
        .slice(-50).map(([key, value]) => ({ issue: Number(key.split(':')[1]), ...value })) });
    }
    if (req.method === 'GET' && path === '/api/desktop/issue') {
      const session = requireDesktop(req);
      const installationId = positiveId(url.searchParams.get('installationId'), 'installation');
      const repoId = positiveId(url.searchParams.get('repoId'), 'repository');
      const number = positiveId(url.searchParams.get('number'), 'issue');
      if (!settings.agentRepoIds.includes(repoId)) throw bad('repository not allowed for agent jobs', 403);
      const issue = await client.issue(session.token, installationId, repoId, number);
      return json(res, 200, { repository: issue.fullName, number: issue.number, title: issue.title });
    }
    if (req.method === 'POST' && path === '/api/desktop/jobs') {
      const session = requireDesktop(req);
      const input = await jsonBody(req);
      const installationId = positiveId(input?.installationId, 'installation');
      const repoId = positiveId(input?.repoId, 'repository');
      const number = positiveId(input?.number, 'issue');
      if (!settings.agentRepoIds.includes(repoId)) throw bad('repository not allowed for agent jobs', 403);
      const issue = await client.issue(session.token, installationId, repoId, number);
      return json(res, 202, startJob(issue, false));
    }
    // An IDE task need not originate from a GitHub issue. A human chooses
    // the repository and exact goal; agents still publish draft PRs only.
    if (req.method === 'POST' && path === '/api/desktop/goals') {
      const session = requireDesktop(req);
      const input = await jsonBody(req);
      const installationId = positiveId(input?.installationId, 'installation');
      const repoId = positiveId(input?.repoId, 'repository');
      const goal = input?.goal;
      if (typeof goal !== 'string' || !goal.trim() || goal.length > 4000
        || goal.includes('\0')) throw bad('invalid engineering goal');
      if (!settings.agentRepoIds.includes(repoId)) throw bad('repository not allowed for agent jobs', 403);
      const repo = await client.authorizedRepo(session.token, installationId, repoId);
      const [owner, name] = String(repo.full_name || '').split('/');
      if (!owner || !name || !repo.default_branch) throw bad('invalid repository metadata', 502);
      const request = {
        kind: 'goal', installationId, repoId, owner, name,
        fullName: repo.full_name, defaultBranch: repo.default_branch,
        title: goal.trim().replace(/\s+/g, ' ').slice(0, 120),
        goal: goal.trim(),
      };
      return json(res, 202, startJob(request));
    }
    if (req.method === 'GET' && path === '/api/desktop/proposals') {
      const session = requireDesktop(req);
      const installationId = positiveId(url.searchParams.get('installationId'), 'installation');
      const repoId = positiveId(url.searchParams.get('repoId'), 'repository');
      await client.authorizedRepo(session.token, installationId, repoId);
      const list = [...proposals.values()]
        .filter((item) => item.owner === session.user.login
          && item.proposal.installationId === installationId && item.proposal.repoId === repoId
          && (item.status !== 'pending' || Date.now() < item.expires))
        .sort((left, right) => Date.parse(right.createdAt) - Date.parse(left.createdAt))
        .slice(0, 50);
      return json(res, 200, { proposals: list.map((item) => ({
        id: item.id, source: item.source, status: item.status,
        operation: item.proposal.operation, params: item.proposal.params,
        createdAt: item.createdAt,
      })) });
    }
    if (req.method === 'POST' && path === '/api/desktop/proposals') {
      const session = requireDesktop(req);
      const proposal = validateProposal(await jsonBody(req));
      await client.authorizedRepo(session.token, proposal.installationId, proposal.repoId);
      const item = addProposal(session.user, proposal, 'desktop');
      return json(res, 201, { id: item.id, status: item.status });
    }
    const desktopProposal = path.match(/^\/api\/desktop\/proposals\/([a-f0-9-]{36})$/);
    if (req.method === 'GET' && desktopProposal) {
      const session = requireDesktop(req);
      const item = proposals.get(desktopProposal[1]);
      if (!item || item.owner !== session.user.login
        || item.status !== 'pending' || Date.now() > item.expires) {
        throw bad('proposal not found or no longer pending', 404);
      }
      const repo = await client.authorizedRepo(session.token,
        item.proposal.installationId, item.proposal.repoId);
      return json(res, 200, {
        id: item.id, operation: item.proposal.operation,
        params: item.proposal.params, repository: repo.full_name,
        installationId: item.proposal.installationId,
        repoId: item.proposal.repoId,
      });
    }
    const desktopDecision = path.match(/^\/api\/desktop\/proposals\/([a-f0-9-]{36})\/(approve|reject)$/);
    if (req.method === 'POST' && desktopDecision) {
      const session = requireDesktop(req);
      const item = proposals.get(desktopDecision[1]);
      if (!item || item.owner !== session.user.login) throw bad('proposal not found', 404);
      if (item.status !== 'pending' || Date.now() > item.expires) {
        throw bad('proposal expired or already decided', 409);
      }
      // Never use a stale installation permission merely because a proposal
      // was valid when queued. Ownership and authorization are checked anew.
      await client.authorizedRepo(session.token,
        item.proposal.installationId, item.proposal.repoId);
      // Authorization may yield while another request approves the same
      // proposal. Re-read the CURRENT record before the one-shot transition.
      const current = proposals.get(item.id);
      if (!current || current.status !== 'pending' || Date.now() > current.expires) {
        throw bad('proposal already decided or expired', 409);
      }
      if (desktopDecision[2] === 'reject') {
        const rejected = approvalStore.save({ ...current, status: 'rejected' });
        return json(res, 200, { id: rejected.id, status: rejected.status });
      }
      const claimed = approvalStore.claim(current.id); // atomic even across processes
      try {
        const result = claimed.proposal.operation === 'clone_repository'
          ? await client.clone(session.token, claimed.proposal, settings.projectsRoot)
          : await client.execute(session.token, claimed.proposal);
        approvalStore.save({ ...claimed, status: 'completed', result });
        return json(res, 200, { id: claimed.id, status: 'completed', result });
      } catch (error) {
        approvalStore.save({ ...claimed, status: 'failed', error: safeError(error).message });
        throw error;
      }
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
      const proposal = validateProposal(await jsonBody(req));
      await client.authorizedRepo(session.token, proposal.installationId, proposal.repoId);
      return json(res, 201, addProposal(session.user, proposal));
    }
    if (req.method === 'POST' && path === '/api/agent/proposals') {
      const token = Buffer.from(String(req.headers.authorization || ''), 'utf8');
      const expected = Buffer.from('Bearer ' + settings.agentToken, 'utf8');
      if (!settings.agentToken || settings.agentToken.length < 32
        || token.length !== expected.length || !timingSafeEqual(token, expected)) {
        throw bad('agent broker unauthorized', 401);
      }
      const proposal = validateProposal(await jsonBody(req));
      if (!settings.agentRepoIds.includes(proposal.repoId)) throw bad('agent repo not allowlisted', 403);
      const owner = Array.from(sessions.values()).find(
        (s) => s.user && s.token && Date.now() <= s.expires
          && settings.allowedUsers.includes(s.user.login.toLowerCase()),
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
      // Web-based approvals must re-check current GitHub installation scope.
      await client.authorizedRepo(session.token,
        item.proposal.installationId, item.proposal.repoId);
      const current = proposals.get(item.id);
      if (!current || current.status !== 'pending' || Date.now() > current.expires) {
        throw bad('proposal already decided or expired', 409);
      }
      if (proposalMatch[2] === 'reject') {
        const rejected = approvalStore.save({ ...current, status: 'rejected' });
        return json(res, 200, { id: rejected.id, status: rejected.status });
      }
      const claimed = approvalStore.claim(current.id);
      try {
        const result = claimed.proposal.operation === 'clone_repository'
          ? await client.clone(session.token, claimed.proposal, settings.projectsRoot)
          : await client.execute(session.token, claimed.proposal);
        approvalStore.save({ ...claimed, status: 'completed', result });
        return json(res, 200, { id: claimed.id, status: 'completed', result });
      } catch (error) {
        approvalStore.save({ ...claimed, status: 'failed', error: safeError(error).message });
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
  const worker = stateDb && ['worker','all'].includes(settings.workerMode)
    ? startGithubWorker({ store: stateDb, broker: client, settings, runPipeline }) : null;
  server.stopGithubWorker = () => worker ? worker.stop() : Promise.resolve();
  server.on('close', () => { if (worker) void worker.stop(); });
  return server;
}
