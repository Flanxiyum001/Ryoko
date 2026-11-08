import { cloneWithInstallationToken } from './clone.mjs';
import { positiveId, READ_PERMISSIONS, WRITE_PERMISSIONS, safeBranch, safeGitHubApiPath, safePath, signAppJwt } from './security.mjs';

const bad = (message, status = 400) => Object.assign(new Error(message), { status });

async function limitedJson(response, limit = 3_000_000) {
  const reader = response.body?.getReader();
  if (!reader) return {};
  const chunks = [];
  let length = 0;
  for (;;) {
    const { done, value } = await reader.read();
    if (done) break;
    length += value.byteLength;
    if (length > limit) {
      await reader.cancel();
      throw bad('GitHub response exceeds size limit', 502);
    }
    chunks.push(value);
  }
  let text = '';
  try { text = Buffer.concat(chunks).toString('utf8'); } catch { throw bad('invalid response', 502); }
  try { return text ? JSON.parse(text) : {}; } catch { throw bad('GitHub returned invalid JSON', 502); }
}

function repoPath(repo) {
  const [owner, name] = String(repo.full_name || '').split('/');
  if (!owner || !name || !/^[A-Za-z0-9_.-]+$/.test(owner) || !/^[A-Za-z0-9_.-]+$/.test(name)) {
    throw bad('invalid GitHub repository metadata', 502);
  }
  return '/repos/' + encodeURIComponent(owner) + '/' + encodeURIComponent(name);
}

function readFilePath(path) {
  if (typeof path !== 'string' || !path || path.length > 250 || path.startsWith('/')
    || path.includes('\\') || path.includes(':') || path.includes('\0')
    || path.split('/').some((segment) => !segment || segment === '.' || segment === '..')) {
    throw bad('unsafe file path');
  }
  return path.split('/').map(encodeURIComponent).join('/');
}

export class GitHubBroker {
  constructor({ appId, clientId, clientSecret, privateKey, fetchImpl = fetch }) {
    this.appId = String(appId || '');
    this.clientId = clientId || '';
    this.clientSecret = clientSecret || '';
    this.privateKey = privateKey || '';
    this.fetch = fetchImpl;
    this.tokenCache = new Map();
  }

  get configured() {
    return Boolean(this.appId && this.clientId && this.clientSecret && this.privateKey);
  }

  async request(path, token, { method = 'GET', body, oauth = false } = {}) {
    if (!oauth) safeGitHubApiPath(path);
    const url = (oauth ? 'https://github.com' : 'https://api.github.com') + path;
    const response = await this.fetch(url, {
      method,
      redirect: 'error',
      signal: AbortSignal.timeout(20_000),
      headers: {
        Accept: oauth ? 'application/json' : 'application/vnd.github+json',
        'User-Agent': 'Ryoko-GitHub-App',
        'X-GitHub-Api-Version': '2022-11-28',
        ...(token ? { Authorization: 'Bearer ' + token } : {}),
        ...(body !== undefined ? { 'Content-Type': 'application/json' } : {}),
      },
      ...(body !== undefined ? { body: JSON.stringify(body) } : {}),
    });
    const data = await limitedJson(response);
    if (!response.ok) {
      // Avoid including arbitrary GitHub payloads or headers in client errors.
      const error = bad('GitHub request failed (' + response.status + ')', response.status === 404 ? 404
        : response.status === 401 || response.status === 403 ? 403
          : response.status === 409 || response.status === 422 ? 409 : 502);
      throw error;
    }
    return data;
  }

  async exchangeCode(code, redirectUri) {
    if (!this.configured || typeof code !== 'string' || code.length > 200) throw bad('OAuth is not configured', 503);
    const token = await this.request('/login/oauth/access_token', null, {
      method: 'POST',
      oauth: true,
      body: {
        client_id: this.clientId,
        client_secret: this.clientSecret,
        code,
        redirect_uri: redirectUri,
      },
    });
    if (!token.access_token || token.token_type?.toLowerCase() !== 'bearer') {
      throw bad('GitHub OAuth exchange did not return a usable token', 502);
    }
    return token.access_token;
  }

  async user(accessToken) {
    const response = await this.request('/user', accessToken);
    if (!response.login || !response.id) throw bad('invalid GitHub user response', 502);
    return { id: response.id, login: response.login, avatar_url: response.avatar_url };
  }

  async installations(userToken) {
    const response = await this.request('/user/installations?per_page=100', userToken);
    if (!Array.isArray(response.installations)) throw bad('could not inspect GitHub App installations', 502);
    return response.installations
      .filter((item) => String(item.app_id) === this.appId)
      .map((item) => ({
        id: item.id,
        account: item.account?.login || 'Unknown account',
        repository_selection: item.repository_selection,
        permissions: item.permissions,
      }));
  }

  async repositories(userToken, installationId) {
    const id = positiveId(installationId, 'installation');
    const owned = await this.installations(userToken);
    if (!owned.some((item) => item.id === id)) throw bad('installation not authorized for this user', 403);
    const response = await this.request('/user/installations/' + id + '/repositories?per_page=100', userToken);
    if (!Array.isArray(response.repositories)) throw bad('GitHub returned no repository list', 502);
    return response.repositories.map((item) => ({
      id: item.id,
      name: item.name,
      full_name: item.full_name,
      default_branch: item.default_branch,
      private: Boolean(item.private),
      html_url: item.html_url,
      description: item.description || '',
    }));
  }

  async authorizedRepo(userToken, installationId, repoId) {
    const id = positiveId(repoId, 'repository');
    const repos = await this.repositories(userToken, installationId);
    const repo = repos.find((entry) => entry.id === id);
    if (!repo) throw bad('repository not available to this user or App installation', 403);
    return repo;
  }

  async installationToken(installationId, repoId, mode = 'read') {
    const install = positiveId(installationId, 'installation');
    const repository = positiveId(repoId, 'repository');
    const scope = mode === 'write' ? WRITE_PERMISSIONS : READ_PERMISSIONS;
    const cacheKey = install + ':' + repository + ':' + mode;
    const cached = this.tokenCache.get(cacheKey);
    if (cached && cached.expires > Date.now() + 90_000) return cached.token;
    if (!this.privateKey) throw bad('GitHub App private key is missing', 503);
    const jwt = signAppJwt(this.appId, this.privateKey);
    const result = await this.request('/app/installations/' + install + '/access_tokens', jwt, {
      method: 'POST',
      body: { repository_ids: [repository], permissions: scope },
    });
    if (!result.token || !result.expires_at) throw bad('installation token not granted', 502);
    const expires = Date.parse(result.expires_at);
    if (!Number.isFinite(expires) || expires <= Date.now()) throw bad('invalid installation token expiration', 502);
    this.tokenCache.set(cacheKey, { token: result.token, expires });
    if (this.tokenCache.size > 200) this.tokenCache.clear();
    return result.token;
  }

  async view(userToken, installationId, repoId, view, options = {}) {
    const repo = await this.authorizedRepo(userToken, installationId, repoId);
    const token = await this.installationToken(installationId, repoId, 'read');
    const root = repoPath(repo);
    switch (view) {
      case 'branches': {
        const data = await this.request(root + '/branches?per_page=30', token);
        return data.map((item) => ({ name: item.name, sha: item.commit?.sha, protected: item.protected }));
      }
      case 'issues': {
        const data = await this.request(root + '/issues?state=open&per_page=30', token);
        return data.filter((item) => !item.pull_request)
          .map((item) => ({ number: item.number, title: item.title, state: item.state, html_url: item.html_url }));
      }
      case 'pulls': {
        const data = await this.request(root + '/pulls?state=open&per_page=30', token);
        return data.map((item) => ({
          number: item.number, title: item.title, draft: item.draft,
          html_url: item.html_url, head: item.head?.ref, state: item.state,
        }));
      }
      case 'checks': {
        const data = await this.request(root + '/actions/runs?per_page=15', token);
        return (data.workflow_runs || []).map((item) => ({
          id: item.id, name: item.name, status: item.status, conclusion: item.conclusion,
          html_url: item.html_url, branch: item.head_branch,
        }));
      }
      case 'tree': {
        const ref = options.ref || repo.default_branch;
        if (typeof ref !== 'string' || ref.length > 120 || /[\s\\\0]/.test(ref)) throw bad('invalid ref');
        const data = await this.request(root + '/git/trees/' + encodeURIComponent(ref) + '?recursive=1', token);
        return { truncated: Boolean(data.truncated), files: (data.tree || [])
          .filter((item) => item.type === 'blob')
          .slice(0, 1000)
          .map((item) => ({ path: item.path, size: item.size })) };
      }
      case 'file': {
        const path = readFilePath(options.path);
        const ref = options.ref || repo.default_branch;
        if (typeof ref !== 'string' || ref.length > 120 || /[\s\\\0]/.test(ref)) throw bad('invalid ref');
        const data = await this.request(root + '/contents/' + path + '?ref=' + encodeURIComponent(ref), token);
        if (data.type !== 'file' || data.encoding !== 'base64' || data.size > 200_000) throw bad('file is too large or not text', 413);
        const bytes = Buffer.from(String(data.content || '').replace(/\s/g, ''), 'base64');
        if (bytes.byteLength > 200_000 || bytes.includes(0)) throw bad('file is too large or binary', 413);
        return { path: data.path, sha: data.sha, content: bytes.toString('utf8') };
      }
      default: throw bad('unsupported repository view');
    }
  }

  async clone(userToken, proposal, projectsRoot) {
    const repo = await this.authorizedRepo(userToken, proposal.installationId, proposal.repoId);
    const token = await this.installationToken(proposal.installationId, proposal.repoId, 'read');
    return cloneWithInstallationToken({
      repo, token, projectsRoot, projectName: proposal.params.projectName,
    });
  }

  async execute(userToken, proposal) {
    const { installationId, repoId, operation, params } = proposal;
    const repo = await this.authorizedRepo(userToken, installationId, repoId);
    const token = await this.installationToken(installationId, repoId, 'write');
    const root = repoPath(repo);
    const repoInfo = await this.request(root, token);
    const base = repoInfo.default_branch;
    if (!base) throw bad('repository has no default branch', 409);

    switch (operation) {
      case 'create_branch': {
        const baseRef = await this.request(root + '/git/ref/heads/' + encodeURIComponent(base), token);
        const result = await this.request(root + '/git/refs', token, {
          method: 'POST',
          body: { ref: 'refs/heads/' + safeBranch(params.branch), sha: baseRef.object?.sha },
        });
        return { action: 'branch_created', ref: result.ref };
      }
      case 'commit_file': {
        const branch = safeBranch(params.branch);
        await this.request(root + '/branches/' + encodeURIComponent(branch), token);
        const path = safePath(params.path).split('/').map(encodeURIComponent).join('/');
        const result = await this.request(root + '/contents/' + path, token, {
          method: 'PUT',
          body: {
            branch,
            message: params.message,
            content: Buffer.from(params.content).toString('base64'),
            ...(params.expectedSha ? { sha: params.expectedSha } : {}),
          },
        });
        return { action: 'file_committed', path: params.path, sha: result.commit?.sha };
      }
      case 'open_pull_request': {
        const result = await this.request(root + '/pulls', token, {
          method: 'POST',
          body: { head: safeBranch(params.head), base, title: params.title, body: params.body },
        });
        return { action: 'pull_request_opened', number: result.number, html_url: result.html_url };
      }
      case 'review_comment': {
        const num = positiveId(params.number, 'pull request');
        await this.request(root + '/pulls/' + num, token);
        const result = await this.request(root + '/issues/' + num + '/comments', token, {
          method: 'POST', body: { body: params.comment },
        });
        return { action: 'review_comment_added', html_url: result.html_url };
      }
      default: throw bad('unsupported operation');
    }
  }
}
