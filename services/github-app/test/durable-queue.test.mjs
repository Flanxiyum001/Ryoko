import assert from 'node:assert/strict';
import { mkdtempSync, readFileSync, rmSync, chmodSync, statSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { test } from 'node:test';
import { BotJobStore } from '../src/durable-queue.mjs';

test('durable bot ledger stores deliveries and never auto-resumes running work', () => {
  const dir = mkdtempSync(join(tmpdir(), 'ryoko-state-test-'));
  const file = join(dir, 'state.json');
  try {
    const original = new BotJobStore(file);
    original.recordDelivery('deadbeef-1234-1234-1234-123456789abc');
    original.setJob('41:9', { status: 'running', startedAt: 22 });
    assert.equal(statSync(file).mode & 0o077, 0);
    assert.equal(JSON.parse(readFileSync(file, 'utf8')).jobs.length, 1);
    const restarted = new BotJobStore(file);
    assert.equal(restarted.jobs.get('41:9').status, 'interrupted');
    assert.equal(restarted.recordDelivery('deadbeef-1234-1234-1234-123456789abc'), false);
    restarted.setJob('41:9', { status: 'completed', number: 55, finishedAt: Date.now() });
    const again = new BotJobStore(file);
    assert.equal(again.jobs.get('41:9').number, 55);
  } finally {
    rmSync(dir, { recursive: true, force: true });
  }
});

test('bot ledger fails closed on unsafe paths and malformed state', () => {
  const dir = mkdtempSync(join(tmpdir(), 'ryoko-state-test-'));
  try {
    assert.throws(() => new BotJobStore('relative-state.json'), /absolute path/);
    const file = join(dir, 'state.json');
    const state = new BotJobStore(file);
    assert.throws(() => state.setJob('wrong', { status: 'running' }), /invalid job/);
    assert.throws(() => state.setJob('1:2', { status: 'unknown' }), /invalid job/);
    const persisted = new BotJobStore(file);
    assert.deepEqual([...persisted.jobs.keys()], []);
    chmodSync(file, 0o644);
    if (process.platform !== 'win32') {
      assert.throws(() => new BotJobStore(file), /owner-only/);
    }
  } finally {
    rmSync(dir, { recursive: true, force: true });
  }
});
