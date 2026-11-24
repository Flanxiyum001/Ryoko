import assert from 'node:assert/strict';
import { test } from 'node:test';
import { createBugReportHandler } from '../src/bug-report.mjs';

function request(ip = '127.0.0.1') {
  return { socket: { remoteAddress: ip } };
}
const valid = {
  title: 'Editor freezes on large file',
  description: 'Opening a large file freezes the editor.',
  expected: 'The editor remains responsive.',
  actual: 'The editor freezes.',
  steps: '1. Open a large file\n2. Wait',
  severity: 'high',
  version: '0.1.0',
  os: 'Linux',
};

test('bug report sends a bounded, structured issue to the configured repository', async () => {
  let sent;
  const submit = createBugReportHandler({
    token: 'test-token',
    repository: 'owner/repo',
    fetchImpl: async (url, options) => {
      sent = { url, options };
      return new Response(JSON.stringify({ number: 12, title: valid.title, html_url: 'https://github.com/owner/repo/issues/12' }), { status: 201 });
    },
  });
  const result = await submit(request(), valid);
  assert.deepEqual(result, { number: 12, url: 'https://github.com/owner/repo/issues/12', title: valid.title });
  assert.equal(sent.url, 'https://api.github.com/repos/owner/repo/issues');
  assert.equal(sent.options.method, 'POST');
  assert.equal(sent.options.redirect, 'error');
  assert.equal(sent.options.headers.Authorization, 'Bearer test-token');
  const body = JSON.parse(sent.options.body);
  assert.equal(body.labels[0], 'bug');
  assert.match(body.body, /Expected behavior/);
  assert.match(body.body, /Actual behavior/);
  assert.match(body.body, /Steps to reproduce/);
  assert.match(body.body, /Ryoko version: 0.1.0/);
  assert.doesNotMatch(body.body, /test-token/);
});

test('bug report validates required fields and bounds text', async () => {
  const submit = createBugReportHandler({ token: 'test-token', fetchImpl: async () => { throw Error('should not call GitHub'); } });
  await assert.rejects(() => submit(request(), { ...valid, title: ' ' }), /title is required/);
  await assert.rejects(() => submit(request(), { ...valid, description: '' }), /description is required/);
  await assert.rejects(() => submit(request(), { ...valid, title: 'x'.repeat(121) }), /title is too long/);
  await assert.rejects(() => submit(request(), { ...valid, diagnostics: 'x'.repeat(2001) }), /diagnostics is too long/);
});

test('bug reports are rate limited per client address', async () => {
  let calls = 0;
  const submit = createBugReportHandler({
    token: 'test-token',
    repository: 'owner/repo',
    fetchImpl: async () => {
      calls++;
      return new Response(JSON.stringify({ number: calls, title: valid.title, html_url: 'https://github.com/owner/repo/issues/' + calls }), { status: 201 });
    },
  });
  for (let i = 0; i < 5; i++) await submit(request('192.0.2.5'), valid);
  await assert.rejects(() => submit(request('192.0.2.5'), valid), /report limit reached/);
  assert.equal(calls, 5);
  await submit(request('192.0.2.6'), valid);
  assert.equal(calls, 6, 'another address has a separate limit');
});

test('bug reporter refuses to run without a credential and accepts only repository slugs', async () => {
  const backslash = createBugReportHandler({ token: 'token', repository: 'ow\\ner/repo' });
  await assert.rejects(() => backslash(request(), valid), /not configured/);
  const missing = createBugReportHandler({ token: '' });
  await assert.rejects(() => missing(request(), valid), /not configured/);
  const invalid = createBugReportHandler({ token: 'token', repository: '../bad' });
  await assert.rejects(() => invalid(request(), valid), /not configured/);
});
