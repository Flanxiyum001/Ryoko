import assert from 'node:assert/strict';
import { mkdtempSync, chmodSync, rmSync, readFileSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { test } from 'node:test';
import { ApprovalStore } from '../src/approval-store.mjs';

function proposal(status = 'pending', id = '12345678-1234-1234-1234-123456789012') {
  return {
    id, owner: 'approved-owner', source: 'agent', status,
    createdAt: new Date().toISOString(), expires: Date.now() + 60_000,
    proposal: {
      installationId: 17, repoId: 41, operation: 'create_branch',
      params: { branch: 'ryoko/agent-work' },
    },
  };
}

test('approvals remain pending across restart, but executing writes are never replayed', () => {
  const dir = mkdtempSync(join(tmpdir(), 'ryoko-approvals-'));
  const path = join(dir, 'approvals.json');
  try {
    const store = new ApprovalStore(path);
    store.save(proposal());
    store.save(proposal('executing', '77777777-7777-7777-7777-777777777777'));
    const restarted = new ApprovalStore(path);
    assert.equal(restarted.get('12345678-1234-1234-1234-123456789012').status, 'pending');
    assert.equal(restarted.get('77777777-7777-7777-7777-777777777777').status, 'interrupted');
    assert.ok(!JSON.stringify(restarted.proposals).includes('broker-secret'));
    assert.equal(new ApprovalStore(path).get('77777777-7777-7777-7777-777777777777').status, 'interrupted');
  } finally {
    rmSync(dir, { recursive: true, force: true });
  }
});

test('approval ledger fails closed on invalid configuration, unsafe records and tampering', () => {
  const dir = mkdtempSync(join(tmpdir(), 'ryoko-approvals-'));
  const path = join(dir, 'approvals.json');
  try {
    assert.throws(() => new ApprovalStore('relative.json'), /absolute/);
    const store = new ApprovalStore(path);
    assert.throws(() => store.save({ ...proposal(), proposal: {
      installationId: 17, repoId: 41, operation: 'commit_file',
      params: { branch: 'main', path: '.env', message: 'bad', content: 'SECRET' },
    } }), /branch/);
    store.save(proposal());
    assert.deepEqual(JSON.parse(readFileSync(path, 'utf8')).proposals.length, 1);
    chmodSync(path, 0o644);
    if (process.platform !== 'win32') assert.throws(() => new ApprovalStore(path), /owner-only/);
    chmodSync(path, 0o600);
    const data = JSON.parse(readFileSync(path, 'utf8'));
    data.proposals[0].proposal.params.branch = 'main';
    writeFileSync(path, JSON.stringify(data), { mode: 0o600 });
    assert.throws(() => new ApprovalStore(path), /branch/);
  } finally {
    rmSync(dir, { recursive: true, force: true });
  }
});

test('expired proposals cannot be silently renewed by a restart', () => {
  const dir = mkdtempSync(join(tmpdir(), 'ryoko-approvals-'));
  const path = join(dir, 'approvals.json');
  try {
    const store = new ApprovalStore(path);
    store.save({ ...proposal(), expires: Date.now() - 100 });
    assert.equal(new ApprovalStore(path).proposals.size, 0);
  } finally {
    rmSync(dir, { recursive: true, force: true });
  }
});
