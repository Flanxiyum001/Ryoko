import { test } from 'node:test';
import assert from 'node:assert/strict';
import { spawnSync } from 'node:child_process';
import { fileURLToPath } from 'node:url';

const guard = fileURLToPath(new URL('../publication_guard.sh', import.meta.url));

function run(scenario, ref = 'refs/heads/ryoko') {
  return spawnSync('bash', ['-c', `
    set -euo pipefail
    source "$1"
    head=expected
    gh() {
      printf '%s\\n' "$head"
    }
    publish() {
      echo published
      if [[ "$SCENARIO" == drift ]]; then head=newer; fi
      if [[ "$SCENARIO" == failed ]]; then return 1; fi
    }
    compensate() { echo compensated; }
    if [[ "$SCENARIO" == stale ]]; then head=newer; fi
    guarded_preview_publish publish compensate
  `, 'test', guard], {
    encoding: 'utf8',
    env: { ...process.env, GITHUB_REF: ref, GITHUB_SHA: 'expected', GH_REPO: 'owner/repo', SCENARIO: scenario },
  });
}

test('stale branch does not publish', () => {
  const result = run('stale');
  assert.equal(result.status, 1);
  assert.equal(result.stdout, '');
});

test('branch advance during publication triggers compensation and failure', () => {
  const result = run('drift');
  assert.equal(result.status, 1);
  assert.equal(result.stdout, 'published\ncompensated\n');
});

test('publication failure triggers compensation', () => {
  const result = run('failed');
  assert.equal(result.status, 1);
  assert.match(result.stdout, /compensated/);
});

test('unchanged branch and explicit tag can publish', () => {
  assert.equal(run('current').status, 0);
  assert.equal(run('stale', 'refs/tags/desktop-preview-v0.1.0-alpha.42').status, 0);
});
