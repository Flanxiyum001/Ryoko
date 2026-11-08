import { readFileSync, statSync } from 'node:fs';
import { resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
import { GitHubBroker } from './github.mjs';
import { createRyokoServer } from './server.mjs';

const port = Number(process.env.PORT || 4173);
const host = process.env.HOST || '127.0.0.1';
const origin = process.env.RYOKO_PUBLIC_ORIGIN || ('http://' + host + ':' + port);
const url = new URL(origin);
if (!Number.isInteger(port) || port <= 0 || port > 65535) throw new Error('invalid PORT');
if (url.origin !== origin || !['http:', 'https:'].includes(url.protocol)) throw new Error('invalid public origin');
if (url.protocol === 'http:' && !['127.0.0.1', 'localhost', '[::1]'].includes(url.hostname)) {
  throw new Error('HTTP may only be used for loopback development; configure HTTPS for hosted deployments');
}
if (!['127.0.0.1', 'localhost', '::1'].includes(host) && url.protocol !== 'https:') {
  throw new Error('public binds require HTTPS at the trusted proxy');
}

let privateKey = '';
if (process.env.GITHUB_PRIVATE_KEY_FILE) {
  const keyPath = process.env.GITHUB_PRIVATE_KEY_FILE;
  const info = statSync(keyPath);
  if (!info.isFile()) throw new Error('GitHub private key path is not a file');
  if (process.platform !== 'win32' && (info.mode & 0o077) !== 0) {
    throw new Error('GitHub App private key must be owner-only (chmod 600)');
  }
  privateKey = readFileSync(keyPath, 'utf8');
}
const broker = new GitHubBroker({
  appId: process.env.GITHUB_APP_ID,
  clientId: process.env.GITHUB_CLIENT_ID,
  clientSecret: process.env.GITHUB_CLIENT_SECRET,
  privateKey,
});
const frontendPath = resolve(fileURLToPath(new URL('../../../apps/desktop/', import.meta.url)));
const server = createRyokoServer({
  broker,
  frontendPath,
  config: {
    origin,
    appSlug: process.env.GITHUB_APP_SLUG,
    allowedUsers: (process.env.RYOKO_ALLOWED_USERS || '').split(',').map((s) => s.trim()).filter(Boolean),
    webhookSecret: process.env.GITHUB_WEBHOOK_SECRET || '',
    agentToken: process.env.RYOKO_AGENT_BROKER_TOKEN || '',
    agentRepoIds: (process.env.RYOKO_AGENT_ALLOWED_REPO_IDS || '').split(',').filter(Boolean),
    ryokoBinary: process.env.RYOKO_CLI_BIN || '',
    projectRoot: process.env.RYOKO_PROJECT_ROOT || '',
    projectsRoot: process.env.RYOKO_PROJECTS_ROOT || '',
  },
});

server.listen(port, host, () => {
  console.log('Ryoko workbench listening on ' + origin);
  console.log('GitHub sign-in: ' + (broker.configured ? 'configured' : 'not configured'));
});
function stop() { server.close(() => process.exit(0)); }
process.on('SIGTERM', stop);
process.on('SIGINT', stop);
