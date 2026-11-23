import assert from 'node:assert/strict';
import { randomBytes, randomUUID } from 'node:crypto';
import { mkdtempSync, readFileSync, rmSync, statSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { test } from 'node:test';
import { ProductionState } from '../src/production-state.mjs';

const key = () => randomBytes(32).toString('base64url');
const request = () => ({
  installationId: 17, repoId: 41, owner: 'approved-owner',
  name: 'repo', fullName: 'approved-owner/repo', defaultBranch: 'main',
  number: 9, title: 'Improve search', body: 'Make it fast',
});
const approval = () => ({
  id: randomUUID(), owner: 'approved-owner', source: 'agent',
  status: 'pending', createdAt: new Date().toISOString(),
  expires: Date.now() + 60000,
  proposal: { installationId: 17, repoId: 41, operation: 'create_branch',
    params: { branch: 'ryoko/stage' } },
});

test('encrypted sessions and one-time desktop codes persist across broker processes', () => {
  const dir = mkdtempSync(join(tmpdir(), 'ryoko-production-'));
  const file = join(dir, 'broker.sqlite3');
  const secret = key();
  let a, b;
  try {
    a = new ProductionState(file, secret);
    const rawToken = 'top-secret-oauth-token-must-not-appear-on-disk';
    a.sessions.set('cookie-id', {
      user: { login: 'approved-owner', id: 12 }, token: rawToken,
      csrf: 'csrf', expires: Date.now() + 120000,
    });
    a.desktopPairings.set('single-use-code', {
      sessionId: 'cookie-id', expires: Date.now() + 60000,
    });
    assert.equal(statSync(file).mode & 0o077, 0);
    b = new ProductionState(file, secret);
    assert.equal(b.sessions.get('cookie-id').token, rawToken);
    assert.equal(readFileSync(file).includes(Buffer.from(rawToken)), false);
    assert.equal(b.takePairing('single-use-code').sessionId, 'cookie-id');
    assert.equal(a.takePairing('single-use-code'), undefined);
    a.sessions.delete('cookie-id');
    assert.equal(b.sessions.get('cookie-id'), undefined);
  } finally {
    b?.close(); a?.close(); rmSync(dir, { recursive: true, force: true });
  }
});

test('atomic webhook delivery and job enrollment dedupe across processes', () => {
  const dir = mkdtempSync(join(tmpdir(), 'ryoko-production-'));
  const file = join(dir, 'jobs.sqlite3');
  const secret = key();
  let a, b;
  try {
    a = new ProductionState(file, secret);
    b = new ProductionState(file, secret);
    const delivery = 'deadbeef-1234-1234-1234-123456789012';
    assert.equal(a.acceptWebhook(delivery, request()).queued, true);
    assert.equal(b.acceptWebhook(delivery, request()).duplicate, true);
    assert.equal(b.jobs.get('41:9').status, 'queued');
    assert.throws(() => b.enqueueJob(request()), /already recorded/);
    const claimed = a.claimJob('worker-A', 120000);
    assert.equal(claimed.request.title, 'Improve search');
    assert.equal(b.claimJob('worker-B'), null, 'second worker cannot claim leased work');
    assert.equal(b.heartbeat(claimed), true);
    b.finishJob(claimed, 'completed', { number: 22, branch: 'ryoko/stage' });
    assert.equal(a.jobs.get('41:9').number, 22);
    assert.throws(() => a.enqueueJob(request(), true), /already recorded/);
  } finally {
    b?.close(); a?.close(); rmSync(dir, { recursive: true, force: true });
  }
});

test('an expired worker lease is interrupted and never auto-resumed', async () => {
  const dir = mkdtempSync(join(tmpdir(), 'ryoko-production-'));
  const file = join(dir, 'recover.sqlite3');
  const secret = key();
  let a, b;
  try {
    a = new ProductionState(file, secret);
    b = new ProductionState(file, secret);
    a.enqueueJob(request());
    const claim = b.claimJob('dead-worker', 5);
    await new Promise((resolve) => setTimeout(resolve, 20));
    a.recoverStale();
    assert.equal(b.jobs.get('41:9').status, 'interrupted');
    assert.equal(a.claimJob('live-worker'), null);
    assert.throws(() => b.finishJob(claim, 'completed', { number: 12 }), /lease expired/);
    b.enqueueJob(request(), true); // only after explicit operator retry
    assert.equal(a.claimJob('live-worker').request.number, 9);
  } finally {
    b?.close(); a?.close(); rmSync(dir, { recursive: true, force: true });
  }
});

test('approval claims are atomic between two API replicas and recover stale writes', () => {
  const dir = mkdtempSync(join(tmpdir(), 'ryoko-production-'));
  const file = join(dir, 'approvals.sqlite3');
  const secret = key();
  let a, b;
  try {
    a = new ProductionState(file, secret);
    b = new ProductionState(file, secret);
    const item = approval();
    a.approvalStore.save(item);
    assert.equal(b.proposals.get(item.id).status, 'pending');
    const owned = a.approvalStore.claim(item.id);
    assert.equal(owned.status, 'executing');
    assert.throws(() => b.approvalStore.claim(item.id), /already claimed/);
    b.approvalStore.save({ ...owned, status: 'completed', result: { action: 'branch_created' } });
    assert.equal(a.proposals.get(item.id).status, 'completed');
    assert.throws(() => a.approvalStore.claim(item.id), /already claimed/);
    const second = approval();
    b.approvalStore.save(second);
    a.approvalStore.claim(second.id);
    a.db.prepare('UPDATE approvals SET lease_until=? WHERE id=?').run(Date.now() - 10, second.id);
    b.recoverStale();
    assert.equal(a.proposals.get(second.id).status, 'interrupted');
    assert.throws(() => b.approvalStore.claim(second.id), /already claimed/);
  } finally {
    b?.close(); a?.close(); rmSync(dir, { recursive: true, force: true });
  }
});

test('standalone engineering goals are repository-bound and deduplicated', () => {
  const dir = mkdtempSync(join(tmpdir(), 'ryoko-production-'));
  const file = join(dir, 'goals.sqlite3');
  let state;
  try {
    state = new ProductionState(file, key());
    const goal = { ...request(), kind: 'goal', number: undefined, title: 'Build editor tests',
      goal: 'Build editor tests without touching the default branch' };
    assert.equal(state.enqueueJob(goal).kind, 'goal');
    assert.throws(() => state.enqueueJob({ ...goal }), /already recorded/);
    assert.equal(state.listJobs(41)[0].issue, 0);
    const claimed = state.claimJob('worker-A');
    assert.equal(claimed.request.kind, 'goal');
    state.finishJob(claimed, 'completed', { number: 91 });
    assert.equal(state.listJobs(41)[0].number, 91);
    assert.throws(() => state.enqueueJob({ ...goal, goal: '' }), /invalid engineering goal/);
  } finally {
    state?.close(); rmSync(dir, { recursive: true, force: true });
  }
});

test('production store requires a private DB file and an explicit 256-bit key', () => {
  const dir = mkdtempSync(join(tmpdir(), 'ryoko-production-'));
  try {
    assert.throws(() => new ProductionState('relative.sqlite3', key()), /absolute path/);
    assert.throws(() => new ProductionState(join(dir, 'db.sqlite3'), 'weak'), /32 random/);
  } finally {
    rmSync(dir, { recursive: true, force: true });
  }
});
