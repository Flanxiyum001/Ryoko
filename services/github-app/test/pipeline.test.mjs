import assert from 'node:assert/strict';
import { mkdtemp, readFile, rm, writeFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { test } from 'node:test';
import { buildPullRequestBody, issueAgentRequest, runIssuePipeline } from '../src/pipeline.mjs';

const payload = {
  action: 'labeled',
  installation: { id: 17 },
  repository: { id: 41, full_name: 'approved-owner/repo', default_branch: 'main' },
  issue: {
    number: 9,
    title: 'Add a health endpoint',
    body: 'Expose GET /health.',
    labels: [{ name: 'ryoko:agent' }],
  },
};

test('either agent label starts an issue pipeline', () => {
  for (const name of ['ryoko-fix', 'ryoko:agent']) {
    const labeled = { ...payload, issue: { ...payload.issue, labels: [{ name }] } };
    assert.equal(issueAgentRequest('issues', labeled)?.number, 9, name);
  }
  assert.equal(issueAgentRequest('issues', {
    ...payload, issue: { ...payload.issue, labels: [{ name: 'ryoko' }] },
  }), null);
});

test('only a human agent label starts an issue pipeline', () => {
  const request = issueAgentRequest('issues', payload);
  assert.equal(request.repoId, 41);
  assert.equal(request.number, 9);
  assert.equal(issueAgentRequest('pull_request', payload), null);
  assert.equal(issueAgentRequest('issues', { ...payload, action: 'closed' }), null);
  assert.equal(issueAgentRequest('issues', {
    ...payload, issue: { ...payload.issue, labels: [{ name: 'bug' }] },
  }), null);
  assert.equal(issueAgentRequest('issues', {
    ...payload, repository: { ...payload.repository, full_name: '../escape/repo' },
  }), null);
});

test('pull request body carries bounded ledger and validation evidence', () => {
  const body = buildPullRequestBody(payload.issue, {
    ledger: { total: { requests: 4, cost_usd: 0.01 } },
    validation: [{ passed: true, summary: 'cargo test passed' }],
  });
  assert.match(body, /Closes #9/);
  assert.match(body, /"requests": 4/);
  assert.match(body, /cargo test passed/);
  assert.ok(body.length <= 12_000);
  const huge = buildPullRequestBody(payload.issue, {
    ledger: { value: 'x'.repeat(100_000) },
    validation: [{ value: 'y'.repeat(100_000) }],
  });
  assert.ok(huge.length <= 12_000);
});

test('pull request body attaches the pre-merge security audit', () => {
  const body = buildPullRequestBody(payload.issue, {
    ledger: {},
    validation: [{
      passed: true,
      logs: [
        'spec test files: tests/api.rs',
        'security audit: passed',
        'security audit: cargo audit skipped (no manifest)',
      ],
    }],
  });
  assert.match(body, /## Security audit/);
  assert.match(body, /cargo audit skipped/);
  // Only audit lines belong in the dedicated section, not the raw log dump.
  const section = body.split('## Security audit')[1].split('## Validation report')[0];
  assert.match(section, /```text\npassed\ncargo audit skipped \(no manifest\)\n```/);
  assert.doesNotMatch(section, /spec test files/);

  const empty = buildPullRequestBody(payload.issue, {
    ledger: {}, validation: [{ passed: true, logs: [] }],
  });
  assert.match(empty, /no security audit output was recorded/);
});

test('pipeline clones, runs the team, pushes the branch and opens a draft PR', async () => {
  const root = await mkdtemp(join(tmpdir(), 'ryoko-pipeline-test-'));
  const requests = [];
  const pushes = [];
  let runArgs;
  const broker = {
    async installationToken(_installationId, _repoId, mode) {
      assert.ok(['clone', 'contents', 'pull_requests'].includes(mode));
      return 'token-' + mode;
    },
    async request(path, token, options) {
      requests.push({ path, token, options });
      return { number: 55, html_url: 'https://github.com/approved-owner/repo/pull/55' };
    },
  };
  try {
    const result = await runIssuePipeline({
      broker,
      request: issueAgentRequest('issues', payload),
      settings: {
        projectsRoot: root, ryokoBinary: '/trusted/ryoko', agentRepoIds: [41],
      },
      logger: { log() {} },
      clone: async ({ projectName }) => ({ path: await mkdtemp(join(root, projectName + '-')) }),
      runCli: async ({ reportPath, goal }) => {
        runArgs = { reportPath, goal };
        await writeFile(reportPath, JSON.stringify({
          run: { succeeded: [{ branch: 'ryoko/issue-9', task_id: 'task-1' }] },
          ledger: { total: { requests: 3 } },
          validation: [{ passed: true }],
        }));
      },
      pushBranch: async (args) => pushes.push(args),
    });
    assert.equal(result.number, 55);
    assert.match(runArgs.goal, /Add a health endpoint/);
    assert.equal(pushes.length, 1);
    assert.equal(pushes[0].branch, 'ryoko/issue-9');
    assert.equal(requests.length, 1);
    assert.equal(requests[0].options.body.draft, true);
    assert.equal(requests[0].options.body.head, 'ryoko/issue-9');
    assert.match(requests[0].options.body.body, /Token ledger/);
    assert.equal(JSON.parse(await readFile(runArgs.reportPath, 'utf8')).ledger.total.requests, 3);
  } finally {
    await rm(root, { recursive: true, force: true });
  }
});

test('pipeline refuses repositories outside the agent allowlist', async () => {
  await assert.rejects(() => runIssuePipeline({
    broker: {},
    request: issueAgentRequest('issues', payload),
    settings: { projectsRoot: '/tmp', ryokoBinary: '/trusted/ryoko', agentRepoIds: [] },
    clone: async () => { throw Error('must not clone'); },
  }), /allowlisted/);
});
