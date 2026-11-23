import assert from 'node:assert/strict';
import { randomBytes } from 'node:crypto';
import { mkdtempSync, readFileSync, rmSync, statSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { test } from 'node:test';
import { ProductionState } from '../src/production-state.mjs';
import { backupBrokerDatabase } from '../src/backup-state.mjs';

test('online backup is complete, encrypted, private and directly restorable', async () => {
  const dir = mkdtempSync(join(tmpdir(), 'ryoko-backup-'));
  const source = join(dir, 'broker.sqlite3');
  const destination = join(dir, 'backup.sqlite3');
  const key = randomBytes(32).toString('base64url');
  let original, restored;
  try {
    original = new ProductionState(source, key);
    original.sessions.set('session-key', {
      token: 'sensitive-token-never-in-backup-plaintext',
      expires: Date.now() + 60000,
    });
    const info = await backupBrokerDatabase({ source, destination, key });
    assert.equal(info.destination, destination);
    assert.equal(statSync(destination).mode & 0o077, 0);
    assert.equal(readFileSync(destination).includes(Buffer.from('sensitive-token')), false);
    restored = new ProductionState(destination, key);
    assert.equal(restored.sessions.get('session-key').token,
      'sensitive-token-never-in-backup-plaintext');
    await assert.rejects(
      () => backupBrokerDatabase({ source, destination, key }), /overwrite/,
    );
  } finally {
    restored?.close(); original?.close();
    rmSync(dir, { recursive: true, force: true });
  }
});

test('backup refuses a wrong key before creating a backup destination', async () => {
  const dir = mkdtempSync(join(tmpdir(), 'ryoko-backup-'));
  const source = join(dir, 'broker.sqlite3');
  const destination = join(dir, 'backup.sqlite3');
  let original;
  try {
    original = new ProductionState(source, randomBytes(32).toString('base64url'));
    await assert.rejects(() => backupBrokerDatabase({
      source, destination, key: randomBytes(32).toString('base64url'),
    }), /does not match/);
  } finally {
    original?.close();
    rmSync(dir, { recursive: true, force: true });
  }
});
