#!/usr/bin/env node
import { backup, DatabaseSync } from 'node:sqlite';
import { randomUUID } from 'node:crypto';
import { chmodSync, existsSync, lstatSync, mkdirSync, renameSync, rmSync } from 'node:fs';
import { dirname, isAbsolute, resolve } from 'node:path';
import { ProductionState } from './production-state.mjs';

/**
 * Online SQLite snapshot, including concurrent WAL transactions.
 * The snapshot remains encrypted using the SAME external RYOKO_STATE_KEY.
 */
export async function backupBrokerDatabase({
  source, destination, key, backupImpl = backup,
}) {
  if (!isAbsolute(source || '') || !isAbsolute(destination || '')
    || resolve(source) === resolve(destination)) {
    throw new Error('source and backup destination must be distinct absolute paths');
  }
  if (!existsSync(source) || !lstatSync(source).isFile()) {
    throw new Error('database does not exist');
  }
  if (existsSync(destination)) throw new Error('refusing to overwrite an existing backup');
  const dir = dirname(destination);
  mkdirSync(dir, { recursive: true, mode: 0o700 });
  const parent = lstatSync(dir);
  if (!parent.isDirectory() || (process.platform !== 'win32' && (parent.mode & 0o077))) {
    throw new Error('backup directory must be private (chmod 700)');
  }
  const temporary = destination + '.partial-' + randomUUID();
  let state;
  try {
    state = new ProductionState(source, key); // validates key and schema
    await backupImpl(state.db, temporary);
    chmodSync(temporary, 0o600);
    const checkDb = new DatabaseSync(temporary);
    try {
      const result = checkDb.prepare('PRAGMA integrity_check').get();
      if (result?.integrity_check !== 'ok') {
        throw new Error('SQLite backup failed integrity check');
      }
    } finally {
      checkDb.close();
    }
    renameSync(temporary, destination);
    return { destination };
  } finally {
    state?.close();
    rmSync(temporary, { force: true });
  }
}

if (process.argv[1] && import.meta.url === new URL('file://' + resolve(process.argv[1])).href) {
  const destination = process.argv[2];
  if (process.argv.length !== 3 || !destination) {
    console.error('Usage: node src/backup-state.mjs /absolute/private/backup.sqlite3');
    process.exitCode = 2;
  } else {
    backupBrokerDatabase({
      source: process.env.RYOKO_STATE_DB,
      destination,
      key: process.env.RYOKO_STATE_KEY,
    }).then(() => {
      console.log('Encrypted Ryoko SQLite backup created. Protect the encryption key separately.');
    }).catch((error) => {
      console.error('Ryoko backup failed: ' + error.message);
      process.exitCode = 1;
    });
  }
}
