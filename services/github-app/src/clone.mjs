import { execFile } from 'node:child_process';
import { mkdtemp, realpath, rm, writeFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { isAbsolute, join } from 'node:path';
import { promisify } from 'node:util';
import { safeProjectName } from './security.mjs';

const exec = promisify(execFile);
const unsafe = (message, status = 400) => Object.assign(new Error(message), { status });

// Git asks a server-local helper for the ephemeral installation credential.
// The token is never embedded in the clone URL, argv, repo metadata or frontend.
const ASKPASS = '#!/bin/sh\ncase "$1" in\n  *Username*) printf "%s\\n" "x-access-token";;\n  *Password*) printf "%s\\n" "$RYOKO_GIT_CLONE_TOKEN";;\n  *) exit 1;;\nesac\n';

export async function cloneWithInstallationToken({ repo, token, projectsRoot, projectName, runner = exec }) {
  const slug = safeProjectName(projectName);
  if (!isAbsolute(projectsRoot || '')) throw unsafe('RYOKO_PROJECTS_ROOT must be an absolute directory', 503);
  const root = await realpath(projectsRoot);
  if (!/^[A-Za-z0-9_.-]+\/[A-Za-z0-9_.-]+$/.test(repo.full_name || '')) {
    throw unsafe('invalid repository metadata', 502);
  }
  if (!token || typeof token !== 'string') throw unsafe('missing installation token', 503);
  const askpassDir = await mkdtemp(join(tmpdir(), 'ryoko-askpass-'));
  let folder = '';
  try {
    const helper = join(askpassDir, 'askpass.sh');
    await writeFile(helper, ASKPASS, { mode: 0o700, flag: 'wx' });
    folder = await mkdtemp(join(root, slug + '-'));
    const url = 'https://github.com/' + repo.full_name + '.git';
    const env = { ...process.env };
    for (const key of Object.keys(env)) {
      if (/^GIT_CONFIG|^GIT_ASKPASS$|^GIT_SSH|^GIT_TEMPLATE_DIR$|^GIT_TRACE/.test(key)) delete env[key];
    }
    Object.assign(env, {
      GIT_ASKPASS: helper,
      GIT_TERMINAL_PROMPT: '0',
      GIT_CONFIG_NOSYSTEM: '1',
      GIT_CONFIG_GLOBAL: '/dev/null',
      RYOKO_GIT_CLONE_TOKEN: token,
    });
    try {
      await runner('git', [
        '-c', 'credential.helper=',
        '-c', 'http.followRedirects=false',
        '-c', 'core.hooksPath=/dev/null',
        '-c', 'protocol.file.allow=never',
        '-c', 'protocol.ext.allow=never',
        'clone', '--depth=1', '--no-tags', '--', url, folder,
      ], {
        cwd: root, env, timeout: 120_000, maxBuffer: 1_000_000, shell: false,
      });
    } catch {
      // Git errors can contain network details; do not echo stderr or auth.
      throw unsafe('Git clone failed; check installation permissions, network access and repository size', 502);
    }
    return { action: 'repository_cloned', path: folder, repository: repo.full_name };
  } catch (error) {
    if (folder) await rm(folder, { recursive: true, force: true }).catch(() => {});
    throw error;
  } finally {
    await rm(askpassDir, { recursive: true, force: true }).catch(() => {});
  }
}
