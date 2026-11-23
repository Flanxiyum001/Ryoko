import {
  closeSync, existsSync, fsyncSync, lstatSync, mkdirSync, openSync,
  readFileSync, renameSync, unlinkSync, writeSync,
} from 'node:fs';
import { dirname, isAbsolute, join } from 'node:path';
import { randomUUID } from 'node:crypto';
import { validateProposal } from './security.mjs';

const MAX_FILE = 4 * 1024 * 1024;
const MAX_RECORDS = 300;
const STATES = new Set(['pending', 'rejected', 'executing', 'interrupted', 'completed', 'failed']);

function validateRecord(item) {
  if (!item || typeof item !== 'object' || Array.isArray(item)
    || !/^[a-f0-9-]{36}$/i.test(item.id || '')
    || !/^[A-Za-z0-9-]{1,80}$/.test(item.owner || '')
    || !['user', 'desktop', 'agent'].includes(item.source)
    || !STATES.has(item.status) || !Number.isSafeInteger(item.expires)
    || item.expires <= 0 || typeof item.createdAt !== 'string'
    || Number.isNaN(Date.parse(item.createdAt))) {
    throw new Error('invalid GitHub approval record');
  }
  // Revalidate on reload. A modified ledger must never smuggle an unsafe
  // path, branch, operation or repository into a later approval.
  const proposal = validateProposal(item.proposal);
  return { ...item, proposal };
}

/** Private, single-process approval ledger. Does not store OAuth/App tokens. */
export class ApprovalStore {
  constructor(file = '') {
    this.file = file;
    this.proposals = new Map();
    if (!file) return;
    if (!isAbsolute(file)) throw new Error('RYOKO_APPROVAL_STATE_FILE must be an absolute path');
    mkdirSync(dirname(file), { recursive: true, mode: 0o700 });
    const parent = lstatSync(dirname(file));
    if (!parent.isDirectory() || (process.platform !== 'win32' && (parent.mode & 0o077))) {
      throw new Error('GitHub approval directory must be private (chmod 700)');
    }
    if (!existsSync(file)) return;
    const info = lstatSync(file);
    if (!info.isFile() || info.size > MAX_FILE
      || (process.platform !== 'win32' && (info.mode & 0o077))) {
      throw new Error('GitHub approval ledger must be an owner-only regular file');
    }
    const data = JSON.parse(readFileSync(file, 'utf8'));
    if (data.version !== 1 || !Array.isArray(data.proposals)
      || data.proposals.length > MAX_RECORDS) {
      throw new Error('invalid GitHub approval ledger version or size');
    }
    for (const raw of data.proposals) {
      const item = validateRecord(raw);
      if (this.proposals.has(item.id)) throw new Error('duplicate GitHub approval record');
      const restored = item.status === 'executing'
        ? { ...item, status: 'interrupted', error: 'Review remote state before any new request' }
        : item;
      this.proposals.set(item.id, restored);
    }
    this.prune();
    this.flush();
  }

  flush() {
    if (!this.file) return;
    const bytes = JSON.stringify({ version: 1, proposals: [...this.proposals.values()] });
    if (Buffer.byteLength(bytes) > MAX_FILE) {
      throw new Error('GitHub approval ledger exceeded storage budget');
    }
    const temp = join(dirname(this.file), '.ryoko-approvals-' + randomUUID());
    let descriptor;
    try {
      descriptor = openSync(temp, 'wx', 0o600);
      writeSync(descriptor, bytes);
      fsyncSync(descriptor);
      closeSync(descriptor);
      descriptor = undefined;
      renameSync(temp, this.file);
    } finally {
      if (descriptor !== undefined) closeSync(descriptor);
      try { unlinkSync(temp); } catch { /* renamed */ }
    }
  }

  prune() {
    const now = Date.now();
    for (const [id, item] of this.proposals) {
      if (item.expires <= now && item.status !== 'executing') this.proposals.delete(id);
    }
    if (this.proposals.size > MAX_RECORDS) {
      for (const [id, item] of this.proposals) {
        if (item.status !== 'pending' && item.status !== 'executing') this.proposals.delete(id);
        if (this.proposals.size <= MAX_RECORDS) break;
      }
    }
  }

  save(raw) {
    const item = validateRecord(raw);
    const before = this.proposals.get(item.id);
    this.prune();
    if (!before && this.proposals.size >= MAX_RECORDS) {
      throw new Error('GitHub approval ledger is full');
    }
    this.proposals.set(item.id, item);
    try {
      this.flush();
    } catch (error) {
      if (before) this.proposals.set(item.id, before);
      else this.proposals.delete(item.id);
      throw error;
    }
    return item;
  }

  get(id) { return this.proposals.get(id); }
}
