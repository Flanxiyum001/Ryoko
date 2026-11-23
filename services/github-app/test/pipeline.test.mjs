import assert from 'node:assert/strict';
import { mkdtemp, readFile, rm, writeFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { test } from 'node:test';
import { buildPullRequestBody, buildRunArgs, issueAgentRequest, runIssuePipeline, selectIntegratedSuccess } from '../src/pipeline.mjs';

const payload = {
  action: 'labeled',
  label: { name: 'ryoko:agent' },
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
    const labeled = { ...payload, label: { name }, issue: { ...payload.issue, labels: [{ name }] } };
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
  assert.equal(issueAgentRequest('issues', { ...payload, action: 'opened' }), null);
  assert.equal(issueAgentRequest('issues', { ...payload, label: { name: 'bug' } }), null);
  assert.equal(issueAgentRequest('issues', { ...payload, issue: { ...payload.issue, state: 'closed' } }), null);
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

test('only the final Integrator result can be published as a GitHub draft PR', () => {
  const integrated = { role: 'integrator', branch: 'ryoko/final', task_id: 'final-id' };
  assert.equal(selectIntegratedSuccess({
    run: { succeeded: [{ role: 'architect', branch: 'ryoko/architect' }, integrated] },
  }), integrated);
  assert.throws(() => selectIntegratedSuccess({
    run: { succeeded: [{ role: 'architect', branch: 'ryoko/architect' }] },
  }), /integrated branch/);
  assert.throws(() => selectIntegratedSuccess({
    run: { succeeded: [integrated], failed: [{ role: 'qa_engineer' }] },
  }), /incomplete/);
  assert.throws(() => selectIntegratedSuccess({
    run: { succeeded: [integrated], skipped: [{ role: 'security_engineer' }] },
  }), /incomplete/);
  assert.throws(() => selectIntegratedSuccess({
    run: { succeeded: [integrated], needs_human_review: [{ task_id: 'x' }] },
  }), /incomplete/);
  assert.throws(() => selectIntegratedSuccess({
    run: { succeeded: [integrated, integrated] },
  }), /unique integrated branch/);
});

test('run arguments forward the operator MCP policy', () => {
  const args = buildRunArgs({
    cwd: '/repo',
    goal: 'Fix the bug',
    reportPath: '/repo/.ryoko/issue-report.json',
    mcp: {
      server: '/srv/ryoko-mcp',
      serverArgs: ['--stdio'],
      allowEnv: ['GITHUB_TOKEN'],
      allowTools: ['list_issues', 'get_issue'],
      headlessApprove: ['list_issues'],
      maxCalls: 8,
    },
  });
  assert.deepEqual(args.slice(0, 6), [
    'run', '--team', '--repo', '/repo', '--report', '/repo/.ryoko/issue-report.json',
  ]);
  assert.equal(args.at(-1), 'Fix the bug', 'the goal stays the last positional argument');
  const valuesFor = (flag) => args.filter((_value, index) => index > 0 && args[index - 1] === flag);
  assert.deepEqual(valuesFor('--mcp-headless-approve'), ['list_issues']);
  assert.deepEqual(valuesFor('--mcp-allow-tool'), ['list_issues', 'get_issue']);
  assert.deepEqual(valuesFor('--mcp-server'), ['/srv/ryoko-mcp']);
  assert.deepEqual(valuesFor('--mcp-max-calls'), ['8']);
});

test('run arguments default to no MCP configuration', () => {
  const args = buildRunArgs({ cwd: '/repo', goal: 'g', reportPath: '/p' });
  assert.ok(!args.some((value) => value.startsWith('--mcp-')));
  assert.deepEqual(args, ['run', '--team', '--repo', '/repo', '--report', '/p', 'g']);
});

test('a flag-like MCP value never becomes an argument', () => {
  const args = buildRunArgs({
    cwd: '/repo',
    goal: 'g',
    reportPath: '/p',
    mcp: { server: '/srv/mcp', serverArgs: ['--danger', '  real-arg  '] },
  });
  assert.ok(!args.includes('--danger'));
  assert.ok(args.includes('real-arg'));
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
          run: { succeeded: [{ role: 'architect', branch: 'ryoko/partial', task_id: 'task-0' },
            { role: 'integrator', branch: 'ryoko/issue-9', task_id: 'task-1' }] },
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
