import {
  closeSync, existsSync, fsyncSync, lstatSync, mkdirSync,
  openSync, readFileSync, renameSync, unlinkSync, writeSync,
} from 'node:fs';
import { dirname, isAbsolute, join } from 'node:path';
import { randomUUID } from 'node:crypto';

function validJob(job) {
  return job && typeof job === 'object' && !Array.isArray(job)
    && ['running', 'interrupted', 'failed', 'completed'].includes(job.status);
}

/**
 * Durable, single-process safety ledger. Restarted running jobs become
 * interrupted; they are NEVER resumed or retried automatically. Operators
 * must reconcile remote branches/PRs before an explicit retry.
 */
export class BotJobStore {
  constructor(file = '') {
    this.file = file;
    this.jobs = new Map();
    this.deliveries = new Map();
    if (!file) return; // Tests/development: memory-only, documented as unsafe for hosting.
    if (!isAbsolute(file)) throw new Error('RYOKO_BOT_STATE_FILE must be an absolute path');
    const dir = dirname(file);
    mkdirSync(dir, { recursive: true, mode: 0o700 });
    const parent = lstatSync(dir);
    if (!parent.isDirectory() || (process.platform !== 'win32' && (parent.mode & 0o077))) {
      throw new Error('bot state directory must be private (chmod 700)');
    }
    if (existsSync(file)) {
      const info = lstatSync(file);
      if (!info.isFile() || info.size > 1_000_000
        || (process.platform !== 'win32' && (info.mode & 0o077))) {
        throw new Error('bot state file must be an owner-only regular file (chmod 600)');
      }
      const value = JSON.parse(readFileSync(file, 'utf8'));
      if (value.version !== 1 || !Array.isArray(value.jobs) || !Array.isArray(value.deliveries)
        || value.jobs.length > 300 || value.deliveries.length > 1000) {
        throw new Error('unsupported or oversized bot state');
      }
      for (const [key, job] of value.jobs) {
        if (!/^\d+:\d+$/.test(key) || !validJob(job)) throw new Error('invalid persisted bot job');
        this.jobs.set(key, job.status === 'running'
          ? { ...job, status: 'interrupted', finishedAt: Date.now() } : job);
      }
      for (const [id, at] of value.deliveries) {
        if (!/^[A-Za-z0-9-]{16,80}$/.test(id) || !Number.isFinite(at)) {
          throw new Error('invalid persisted webhook delivery');
        }
        this.deliveries.set(id, at);
      }
      this.flush(); // Persist interrupted transitions before accepting requests.
    }
  }

  flush() {
    if (!this.file) return;
    const json = JSON.stringify({
      version: 1, jobs: [...this.jobs], deliveries: [...this.deliveries],
    });
    if (Buffer.byteLength(json) > 1_000_000) throw new Error('bot job ledger exceeds limit');
    const tmp = join(dirname(this.file), '.ryoko-state-' + randomUUID());
    let fd;
    try {
      fd = openSync(tmp, 'wx', 0o600);
      writeSync(fd, json);
      fsyncSync(fd);
      closeSync(fd);
      fd = undefined;
      renameSync(tmp, this.file);
    } finally {
      if (fd !== undefined) closeSync(fd);
      try { unlinkSync(tmp); } catch { /* already renamed */ }
    }
  }

  setJob(key, job) {
    if (!/^\d+:\d+$/.test(key) || !validJob(job)) throw new Error('invalid job update');
    const previous = this.jobs.get(key);
    this.jobs.set(key, job);
    try { this.flush(); } catch (error) {
      if (previous) this.jobs.set(key, previous);
      else this.jobs.delete(key);
      throw error;
    }
  }

  recordDelivery(id) {
    if (this.deliveries.has(id)) return false;
    if (this.deliveries.size >= 1000) this.deliveries.delete(this.deliveries.keys().next().value);
    this.deliveries.set(id, Date.now());
    this.flush();
    return true;
  }

  pruneJobs() {
    if (this.jobs.size < 300) return;
    for (const [key, job] of this.jobs) {
      if (job.status !== 'running' && Date.now() - (job.finishedAt || 0) > 86_400_000) {
        this.jobs.delete(key);
      }
    }
    this.flush();
  }
}
