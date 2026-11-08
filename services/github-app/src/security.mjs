import { createHmac, sign, timingSafeEqual } from 'node:crypto';

export const READ_PERMISSIONS = Object.freeze({
  contents: 'read', issues: 'read', pull_requests: 'read', checks: 'read', actions: 'read',
});
export const WRITE_PERMISSIONS = Object.freeze({
  contents: 'write', issues: 'write', pull_requests: 'write',
});

const unsafe = (message) => Object.assign(new Error(message), { status: 400 });

export function verifyWebhookSignature(payload, header, secret) {
  if (!secret || !Buffer.isBuffer(payload) || typeof header !== 'string' || !/^sha256=[a-f0-9]{64}$/i.test(header)) {
    return false;
  }
  const expected = Buffer.from('sha256=' + createHmac('sha256', secret).update(payload).digest('hex'), 'utf8');
  const actual = Buffer.from(header, 'utf8');
  return actual.length === expected.length && timingSafeEqual(actual, expected);
}

export function signAppJwt(appId, privateKey, now = Math.floor(Date.now() / 1000)) {
  if (!/^[1-9]\d*$/.test(String(appId)) || !privateKey) throw unsafe('GitHub App ID and private key are required');
  const encode = (value) => Buffer.from(JSON.stringify(value)).toString('base64url');
  const input = encode({ alg: 'RS256', typ: 'JWT' }) + '.' + encode({
    iat: now - 60, exp: now + 540, iss: String(appId),
  });
  return input + '.' + sign('RSA-SHA256', Buffer.from(input), privateKey).toString('base64url');
}

export function positiveId(input, label = 'id') {
  const number = Number(input);
  if (!Number.isSafeInteger(number) || number <= 0 || String(number) !== String(input)) {
    throw unsafe('invalid ' + label);
  }
  return number;
}

export function safeBranch(branch) {
  if (typeof branch !== 'string' || branch.length > 120
    || !/^ryoko\/[A-Za-z0-9][A-Za-z0-9._/-]*$/.test(branch)
    || branch.includes('..') || branch.includes('//') || branch.endsWith('/')
    || branch.endsWith('.') || branch.endsWith('.lock')) {
    throw unsafe('agent branch must start with ryoko/ and contain safe Git ref characters');
  }
  return branch;
}

export function safePath(path) {
  if (typeof path !== 'string' || path.length > 240 || !path
    || path.startsWith('/') || path.includes('\\') || path.includes(':')
    || path.includes('\0') || path.includes('//')) {
    throw unsafe('unsafe repository path');
  }
  const segments = path.split('/');
  if (segments.some((segment) => !segment || segment === '.' || segment === '..'
    || segment.toLowerCase() === '.git' || segment.toLowerCase() === '.ryoko'
    || segment.toLowerCase() === '.github'
    || /^\.env(\.|$)/i.test(segment)
    || /\.(pem|p12|pfx|key)$/i.test(segment))) {
    throw unsafe('repository metadata or private credentials cannot be changed');
  }
  return path;
}

function safeText(value, label, limit = 5000) {
  if (typeof value !== 'string' || !value.trim() || value.length > limit || value.includes('\0')) {
    throw unsafe('invalid ' + label);
  }
  return value;
}

export function validateProposal(input) {
  if (!input || typeof input !== 'object' || Array.isArray(input)) throw unsafe('invalid proposal');
  const installationId = positiveId(input.installationId, 'installation ID');
  const repoId = positiveId(input.repoId, 'repository ID');
  const operation = input.operation;
  const params = input.params;
  if (!params || typeof params !== 'object' || Array.isArray(params)) throw unsafe('proposal parameters must be an object');
  let validated;
  switch (operation) {
    case 'create_branch':
      validated = { branch: safeBranch(params.branch) };
      break;
    case 'commit_file':
      validated = {
        branch: safeBranch(params.branch),
        path: safePath(params.path),
        content: safeText(params.content, 'file content', 100_000),
        message: safeText(params.message, 'commit message', 200),
      };
      if (params.expectedSha != null) {
        if (typeof params.expectedSha !== 'string' || !/^[a-f0-9]{40}$/i.test(params.expectedSha)) {
          throw unsafe('invalid expected file SHA');
        }
        validated.expectedSha = params.expectedSha;
      }
      break;
    case 'open_pull_request':
      validated = {
        head: safeBranch(params.head),
        title: safeText(params.title, 'pull request title', 140),
        body: typeof params.body === 'string' && params.body.length <= 12_000
          ? params.body : (() => { throw unsafe('invalid pull request body'); })(),
      };
      break;
    case 'review_comment':
      validated = {
        number: positiveId(params.number, 'pull request number'),
        comment: safeText(params.comment, 'review comment', 8000),
      };
      break;
    default: throw unsafe('unsupported GitHub agent action');
  }
  return { installationId, repoId, operation, params: validated };
}

export function safeGitHubApiPath(path) {
  if (typeof path !== 'string' || !path.startsWith('/') || path.startsWith('//')
    || path.includes('\\') || path.includes('://') || path.includes('\0')
    || /(^|\/)\.\.?($|\/)/.test(path)) {
    throw unsafe('invalid GitHub API request path');
  }
  return path;
}

export function isAllowedOrigin(origin, configuredOrigin) {
  return typeof origin === 'string' && origin === configuredOrigin;
}
