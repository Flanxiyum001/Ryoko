import assert from 'node:assert/strict';
import { test } from 'node:test';
import { GitHubBroker } from '../src/github.mjs';
import { assertReadyForReview, inspectDraftChecks } from '../src/ci-gates.mjs';

const sha = 'a'.repeat(40);
const pr = {
  state: 'open', draft: true, node_id: 'PR_node_77',
  head: { sha, ref: 'ryoko/verified', repo: { id: 41 } },
};
const successCheck = { status: 'completed', conclusion: 'success' };
function fakeChecks({
  pull = pr, checkRuns = [successCheck], statuses = [],
  secondPull = pull,
} = {}) {
  const calls = [];
  let readPr = 0;
  const broker = {
    async request(path) {
      calls.push(path);
      if (path.endsWith('/pulls/77')) {
        readPr++;
        return readPr === 1 ? pull : secondPull;
      }
      if (path.includes('/check-runs')) {
        return { check_runs: checkRuns, total_count: checkRuns.length };
      }
      if (path.includes('/status?')) return { statuses };
      throw new Error('unexpected request: ' + path);
    },
  };
  return { broker, calls };
}
const args = (broker) => ({
  broker, root: '/repos/owner/repo', repoId: 41, number: 77, readToken: 'read-only',
});

test('CI gate permits only current, fully passing PR head checks', async () => {
  const { broker } = fakeChecks();
  const checked = await inspectDraftChecks(args(broker));
  assert.equal(checked.ready, true);
  assert.equal(checked.sha, sha);
  assert.equal((await assertReadyForReview(args(fakeChecks().broker))).node_id, pr.node_id);
});

test('CI gate rejects missing, pending and failing checks and legacy statuses', async () => {
  const cases = [
    { checkRuns: [], statuses: [] },
    { checkRuns: [{ status: 'in_progress', conclusion: null }] },
    { checkRuns: [{ status: 'completed', conclusion: 'failure' }] },
    { checkRuns: [successCheck], statuses: [{ context: 'build', state: 'pending' }] },
    { checkRuns: [successCheck], statuses: [{ context: 'lint', state: 'failure' }] },
    { pull: { ...pr, draft: false } },
    { pull: { ...pr, head: { ...pr.head, repo: { id: 999 } } } },
    { pull: { ...pr, head: { ...pr.head, ref: 'main' } } },
    { pull: { ...pr, head: { ...pr.head, sha: 'invalid' } } },
  ];
  for (const testcase of cases) {
    const { broker } = fakeChecks(testcase);
    const result = await inspectDraftChecks(args(broker));
    assert.equal(result.ready, false, JSON.stringify(testcase));
  }
  const { broker } = fakeChecks({ secondPull: {
    ...pr, head: { ...pr.head, sha: 'b'.repeat(40) },
  } });
  await assert.rejects(() => assertReadyForReview(args(broker)), /changed/);
});

test('GitHub broker refuses PR readiness until CI passes and uses a scoped GraphQL mutation', async () => {
  const broker = new GitHubBroker({ appId: '3', clientId: 'mock', clientSecret: 'mock', privateKey: 'mock' });
  broker.authorizedRepo = async () => ({ id: 41, full_name: 'owner/repo', default_branch: 'main' });
  const scopes = [];
  broker.installationToken = async (_installationId, _repoId, mode) => {
    scopes.push(mode);
    return mode + '-token';
  };
  const calls = [];
  broker.request = async (path, token, options) => {
    calls.push({ path, token, options });
    if (path === '/repos/owner/repo') return { default_branch: 'main' };
    if (path.endsWith('/pulls/77')) return pr;
    if (path.includes('/check-runs')) {
      return { check_runs: [successCheck], total_count: 1 };
    }
    if (path.includes('/status?')) return { statuses: [] };
    if (path === '/graphql') {
      assert.equal(token, 'pull_requests-token');
      assert.equal(options.method, 'POST');
      assert.equal(options.body.variables.id, pr.node_id);
      return { data: { markPullRequestReadyForReview: { pullRequest: { number: 77, isDraft: false } } } };
    }
    throw new Error('unexpected API ' + path);
  };
  const result = await broker.execute('user-token', {
    installationId: 17, repoId: 41,
    operation: 'mark_ready_for_review', params: { number: 77 },
  });
  assert.equal(result.action, 'pull_request_ready_for_review');
  assert.ok(scopes.includes('read') && scopes.includes('pull_requests'));
  assert.equal(calls.filter((item) => item.path === '/graphql').length, 1);
});
