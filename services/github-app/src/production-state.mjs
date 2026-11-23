import { DatabaseSync } from 'node:sqlite';
import {
  createCipheriv, createDecipheriv, createHash, createHmac, randomBytes, randomUUID,
} from 'node:crypto';
import { closeSync, existsSync, lstatSync, mkdirSync, openSync } from 'node:fs';
import { dirname, isAbsolute } from 'node:path';
import { validateProposal } from './security.mjs';

const conflict = (message) => Object.assign(new Error(message), { status: 409 });
const MAX_GOAL = 4000;
const MAX_JOBS = 10000;
const sha = (value) => createHash('sha256').update(String(value)).digest('hex');
const nowMs = () => Date.now();
function privateDatabaseFile(file) {
  if (!isAbsolute(file)) throw new Error('RYOKO_STATE_DB must be an absolute path');
  const dir = dirname(file);
  mkdirSync(dir, { recursive: true, mode: 0o700 });
  const info = lstatSync(dir);
  if (!info.isDirectory() || (process.platform !== 'win32' && (info.mode & 0o077))) {
    throw new Error('database directory must be private (chmod 700)');
  }
  if (!existsSync(file)) {
    try { closeSync(openSync(file, 'wx', 0o600)); }
    catch (error) { if (error.code !== 'EEXIST') throw error; }
  }
  const stat = lstatSync(file);
  if (!stat.isFile() || (process.platform !== 'win32' && (stat.mode & 0o077))) {
    throw new Error('database must be an owner-only regular file (chmod 600)');
  }
}
function checkedKey(raw) {
  const value = String(raw || '');
  if (!/^[A-Za-z0-9_-]{43}=?$/.test(value)) throw new Error('RYOKO_STATE_KEY must be 32 random base64url bytes');
  const key = Buffer.from(value, 'base64url');
  if (key.length !== 32) throw new Error('invalid RYOKO_STATE_KEY');
  return key;
}
class EncryptedVault {
  constructor(key) { this.key = key; }
  seal(value) {
    const iv = randomBytes(12);
    const cipher = createCipheriv('aes-256-gcm', this.key, iv);
    const payload = Buffer.concat([cipher.update(JSON.stringify(value)), cipher.final()]);
    return Buffer.concat([iv, cipher.getAuthTag(), payload]).toString('base64url');
  }
  open(text) {
    const bytes = Buffer.from(text, 'base64url');
    if (bytes.length < 29 || bytes.length > 250000) throw new Error('encrypted database record malformed');
    const dec = createDecipheriv('aes-256-gcm', this.key, bytes.subarray(0, 12));
    dec.setAuthTag(bytes.subarray(12, 28));
    return JSON.parse(Buffer.concat([dec.update(bytes.subarray(28)), dec.final()]).toString('utf8'));
  }
}

/** Hashes opaque bearer IDs and encrypts each record before saving. */
class DurableMap {
  constructor(store, kind) { this.store = store; this.kind = kind; }
  get(key) {
    const row = this.store.db.prepare('SELECT value,expires FROM secrets WHERE kind=? AND key_hash=?')
      .get(this.kind, sha(key));
    if (!row || row.expires <= nowMs()) return undefined;
    return this.store.vault.open(row.value);
  }
  set(key, value) {
    if (!Number.isSafeInteger(value?.expires) || value.expires <= 0) throw new Error('invalid session expiry');
    const data = this.store.vault.seal(value);
    this.store.db.prepare(`INSERT INTO secrets(kind,key_hash,value,expires)
      VALUES(?,?,?,?) ON CONFLICT(kind,key_hash) DO UPDATE SET value=excluded.value,expires=excluded.expires`)
      .run(this.kind, sha(key), data, value.expires);
    return this;
  }
  delete(key) {
    return this.store.db.prepare('DELETE FROM secrets WHERE kind=? AND key_hash=?')
      .run(this.kind, sha(key)).changes > 0;
  }
  get size() {
    return Number(this.store.db.prepare('SELECT count(*) AS n FROM secrets WHERE kind=? AND expires>?')
      .get(this.kind, nowMs()).n);
  }
  *values() {
    const rows = this.store.db.prepare('SELECT value FROM secrets WHERE kind=? AND expires>? LIMIT 501')
      .all(this.kind, nowMs());
    for (const row of rows) yield this.store.vault.open(row.value);
  }
  has(key) { return this.get(key) !== undefined; }
}

class DurableProposals {
  constructor(store) { this.store = store; }
  get(id) {
    const row = this.store.db.prepare('SELECT payload,status,lease_id,expires FROM approvals WHERE id=?').get(id);
    if (!row) return undefined;
    const data = this.store.vault.open(row.payload);
    return { ...data, status: row.status, leaseId: row.lease_id || undefined, expires: row.expires };
  }
  *values() {
    const rows = this.store.db.prepare('SELECT id FROM approvals ORDER BY created_at DESC LIMIT 301').all();
    for (const row of rows) yield this.get(row.id);
  }
  get size() {
    return Number(this.store.db.prepare('SELECT count(*) AS n FROM approvals').get().n);
  }
}

class DurableJobs {
  constructor(store) { this.store = store; }
  get(key) {
    const row = this.store.db.prepare('SELECT * FROM jobs WHERE job_key=?').get(key);
    return row ? this.store.jobView(row) : undefined;
  }
  *entries() {
    const rows = this.store.db.prepare('SELECT * FROM jobs ORDER BY created_at DESC LIMIT 500').all();
    for (const row of rows) yield [row.job_key, this.store.jobView(row)];
  }
  get size() {
    return Number(this.store.db.prepare('SELECT count(*) AS n FROM jobs').get().n);
  }
}

function validateJobRequest(request) {
  if (!request || typeof request !== 'object'
    || !Number.isSafeInteger(request.repoId) || request.repoId <= 0
    || !Number.isSafeInteger(request.installationId) || request.installationId <= 0
    || !/^[A-Za-z0-9_.-]+\/[A-Za-z0-9_.-]+$/.test(request.fullName || '')
    || !request.owner || !request.name || request.owner + '/' + request.name !== request.fullName) {
    throw conflict('invalid repository-scoped job');
  }
  if (request.kind === 'goal') {
    if (typeof request.goal !== 'string' || !request.goal.trim()
      || request.goal.length > MAX_GOAL || request.goal.includes('\0')
      || !request.title || request.title.length > 180) {
      throw conflict('invalid engineering goal');
    }
    return;
  }
  if (!Number.isSafeInteger(request.number) || request.number <= 0
    || typeof request.title !== 'string' || !request.title) {
    throw conflict('invalid issue job');
  }
}
function jobKey(request) {
  return request.kind === 'goal'
    ? `${request.repoId}:goal:${sha(request.goal.trim()).slice(0, 32)}`
    : `${request.repoId}:${request.number}`;
}

/**
 * One private SQLite WAL database shared by multiple processes on one host.
 * BEGIN IMMEDIATE serializes scheduling, leases and one-shot approvals.
 * Never deploy over NFS; use PostgreSQL for a multi-host cluster.
 */
export class ProductionState {
  constructor(file, rawKey) {
    privateDatabaseFile(file);
    this.vault = new EncryptedVault(checkedKey(rawKey));
    this.db = new DatabaseSync(file);
    this.db.exec(`PRAGMA journal_mode=WAL; PRAGMA synchronous=FULL;
      PRAGMA busy_timeout=5000; PRAGMA foreign_keys=ON;`);
    this.db.exec(`
      CREATE TABLE IF NOT EXISTS meta(
        key TEXT PRIMARY KEY, value TEXT NOT NULL
      );
      CREATE TABLE IF NOT EXISTS secrets(
        kind TEXT NOT NULL, key_hash TEXT NOT NULL, value TEXT NOT NULL,
        expires INTEGER NOT NULL, PRIMARY KEY(kind,key_hash)
      );
      CREATE INDEX IF NOT EXISTS secrets_exp ON secrets(expires);
      CREATE TABLE IF NOT EXISTS approvals(
        id TEXT PRIMARY KEY, owner TEXT NOT NULL, repo_id INTEGER NOT NULL,
        installation_id INTEGER NOT NULL, payload TEXT NOT NULL, status TEXT NOT NULL,
        expires INTEGER NOT NULL, created_at INTEGER NOT NULL,
        lease_id TEXT, lease_until INTEGER
      );
      CREATE INDEX IF NOT EXISTS approvals_repo ON approvals(repo_id,created_at);
      CREATE TABLE IF NOT EXISTS deliveries(
        delivery_id TEXT PRIMARY KEY, received_at INTEGER NOT NULL
      );
      CREATE INDEX IF NOT EXISTS deliveries_time ON deliveries(received_at);
      CREATE TABLE IF NOT EXISTS workers(
        id TEXT PRIMARY KEY, heartbeat_at INTEGER NOT NULL
      );
      CREATE TABLE IF NOT EXISTS jobs(
        id TEXT PRIMARY KEY, job_key TEXT NOT NULL UNIQUE, installation_id INTEGER NOT NULL,
        repo_id INTEGER NOT NULL, kind TEXT NOT NULL,
        payload TEXT NOT NULL, status TEXT NOT NULL, created_at INTEGER NOT NULL,
        started_at INTEGER, finished_at INTEGER,
        lease_owner TEXT, lease_id TEXT, lease_until INTEGER,
        result TEXT, attempt INTEGER NOT NULL DEFAULT 0
      );
      CREATE INDEX IF NOT EXISTS jobs_claim ON jobs(status,created_at);
      CREATE INDEX IF NOT EXISTS jobs_repo ON jobs(repo_id,created_at);
    `);
    const version = this.db.prepare('PRAGMA user_version').get().user_version;
    if (version > 1) throw new Error('database schema is newer than this broker');
    const verifier = createHmac('sha256', this.vault.key)
      .update('ryoko-state-key-verifier-v1').digest('hex');
    try {
      this.transaction(() => {
        const stored = this.db.prepare("SELECT value FROM meta WHERE key='state_key_check'").get();
        if (stored && stored.value !== verifier) {
          throw new Error('RYOKO_STATE_KEY does not match the existing encrypted database');
        }
        if (!stored) this.db.prepare('INSERT INTO meta(key,value) VALUES(?,?)')
          .run('state_key_check', verifier);
        if (version === 0) this.db.exec('PRAGMA user_version=1');
      });
    } catch (error) {
      this.db.close();
      throw error;
    }
    this.sessions = new DurableMap(this, 'session');
    this.desktopPairings = new DurableMap(this, 'pairing');
    this.desktopSessions = new DurableMap(this, 'desktop');
    this.proposals = new DurableProposals(this);
    this.jobs = new DurableJobs(this);
    this.deliveries = { has: (delivery) => Boolean(this.db.prepare(
      'SELECT 1 AS yes FROM deliveries WHERE delivery_id=?').get(delivery)) };
    this.approvalStore = {
      proposals: this.proposals,
      prune: () => this.prune(),
      save: (item) => this.saveApproval(item),
      claim: (id) => this.claimApproval(id),
    };
  }

  transaction(fn) {
    this.db.exec('BEGIN IMMEDIATE');
    try {
      const result = fn();
      this.db.exec('COMMIT');
      return result;
    } catch (error) {
      this.db.exec('ROLLBACK');
      throw error;
    }
  }
  prune() {
    const now = nowMs();
    this.transaction(() => {
      this.db.prepare('DELETE FROM secrets WHERE expires<=?').run(now);
      this.db.prepare(`DELETE FROM approvals WHERE status!='pending'
        AND status!='executing' AND expires+86400000<=?`).run(now);
      this.db.prepare(`DELETE FROM approvals WHERE status='pending' AND expires<=?`).run(now);
      this.db.prepare('DELETE FROM deliveries WHERE received_at<?').run(now - 7 * 86400000);
      this.db.prepare('DELETE FROM workers WHERE heartbeat_at<?').run(now - 60000);
    });
  }
  touchWorker(workerId) {
    this.db.prepare(`INSERT INTO workers(id,heartbeat_at) VALUES(?,?)
      ON CONFLICT(id) DO UPDATE SET heartbeat_at=excluded.heartbeat_at`)
      .run(workerId, nowMs());
  }
  releaseWorker(workerId) {
    this.db.prepare('DELETE FROM workers WHERE id=?').run(workerId);
  }
  activeWorkerCount() {
    return Number(this.db.prepare('SELECT count(*) AS n FROM workers WHERE heartbeat_at>?')
      .get(nowMs() - 45_000).n);
  }
  pendingJobCount() {
    return Number(this.db.prepare("SELECT count(*) AS n FROM jobs WHERE status='queued'")
      .get().n);
  }
  pruneJobs() {
    this.db.prepare(`DELETE FROM jobs WHERE status IN ('completed','failed','interrupted')
      AND finished_at<?`).run(nowMs() - 30 * 86400000);
  }
  takePairing(code) {
    return this.transaction(() => {
      const item = this.desktopPairings.get(code);
      this.desktopPairings.delete(code);
      return item;
    });
  }
  saveApproval(raw) {
    const proposal = validateProposal(raw.proposal);
    const item = { ...raw, proposal };
    if (!/^[a-f0-9-]{36}$/i.test(item.id)
      || !['user','desktop','agent'].includes(item.source)
      || !/^[A-Za-z0-9-]{1,80}$/.test(item.owner)
      || !Number.isSafeInteger(item.expires) || !Number.isFinite(Date.parse(item.createdAt))) {
      throw conflict('invalid approval metadata');
    }
    return this.transaction(() => {
      const row = this.db.prepare('SELECT status,lease_id FROM approvals WHERE id=?').get(item.id);
      if (!row && item.status !== 'pending') throw conflict('new approval must be pending');
      if (row) {
        const allowed = (row.status === 'pending' && item.status === 'rejected')
          || (row.status === 'executing' && ['completed','failed'].includes(item.status)
            && row.lease_id && row.lease_id === item.leaseId);
        if (!allowed) throw conflict('proposal is already decided or no longer owned');
      }
      const payload = this.vault.seal(item);
      if (row) {
        this.db.prepare(`UPDATE approvals SET status=?,payload=?,lease_id=NULL,lease_until=NULL WHERE id=?`)
          .run(item.status, payload, item.id);
      } else {
        this.db.prepare(`INSERT INTO approvals(
          id,owner,repo_id,installation_id,payload,status,expires,created_at
        ) VALUES(?,?,?,?,?,?,?,?)`).run(
          item.id, item.owner, proposal.repoId, proposal.installationId,
          payload, item.status, item.expires, Date.parse(item.createdAt),
        );
      }
      return item;
    });
  }
  claimApproval(id) {
    return this.transaction(() => {
      const item = this.proposals.get(id);
      if (!item || item.status !== 'pending' || item.expires <= nowMs()) {
        throw conflict('approval is expired or already claimed');
      }
      const leaseId = randomUUID();
      this.db.prepare(`UPDATE approvals SET status='executing',lease_id=?,lease_until=?
        WHERE id=? AND status='pending'`).run(leaseId, nowMs() + 15 * 60000, id);
      return { ...item, status: 'executing', leaseId };
    });
  }
  recordDelivery(delivery) {
    if (!/^[A-Za-z0-9-]{16,80}$/.test(delivery)) throw conflict('invalid delivery identifier');
    return this.transaction(() => this.db.prepare(
      'INSERT OR IGNORE INTO deliveries(delivery_id,received_at) VALUES(?,?)'
    ).run(delivery, nowMs()).changes === 1);
  }
  enqueueInTransaction(request, retry = false) {
    validateJobRequest(request);
    const key = jobKey(request);
    const existing = this.db.prepare('SELECT * FROM jobs WHERE job_key=?').get(key);
    if (retry ? !existing || !['failed','interrupted'].includes(existing.status) : Boolean(existing)) {
      throw conflict('job already recorded; inspect and reconcile before retrying');
    }
    if (!existing && this.jobs.size >= MAX_JOBS) throw conflict('job capacity exceeded');
    const created = nowMs();
    if (existing) {
      this.db.prepare(`UPDATE jobs SET status='queued',payload=?,created_at=?,
        started_at=NULL,finished_at=NULL,lease_owner=NULL,lease_id=NULL,lease_until=NULL,
        result=NULL WHERE job_key=?`).run(this.vault.seal(request), created, key);
    } else {
      this.db.prepare(`INSERT INTO jobs(
        id,job_key,installation_id,repo_id,kind,payload,status,created_at
      ) VALUES(?,?,?,?,?,?,'queued',?)`).run(
        randomUUID(), key, request.installationId, request.repoId,
        request.kind === 'goal' ? 'goal' : 'issue', this.vault.seal(request), created,
      );
    }
    return { status: 'queued', kind: request.kind || 'issue',
      issue: request.number || 0, title: request.title, createdAt: created };
  }
  enqueueJob(request, retry = false) {
    return this.transaction(() => this.enqueueInTransaction(request, retry));
  }
  acceptWebhook(delivery, request, retry = false) {
    if (!/^[A-Za-z0-9-]{16,80}$/.test(delivery)) throw conflict('invalid delivery identifier');
    return this.transaction(() => {
      const added = this.db.prepare(
        'INSERT OR IGNORE INTO deliveries(delivery_id,received_at) VALUES(?,?)'
      ).run(delivery, nowMs()).changes;
      if (!added) return { duplicate: true };
      if (!request) return { duplicate: false, queued: false };
      try {
        const job = this.enqueueInTransaction(request, retry);
        return { duplicate: false, queued: true, job };
      } catch (error) {
        if (error?.status !== 409) throw error;
        return { duplicate: false, queued: false, refusal: error.message };
      }
    });
  }
  jobView(row) {
    const request = this.vault.open(row.payload);
    const result = row.result ? this.vault.open(row.result) : {};
    return { status: row.status, kind: row.kind, issue: request.number || 0,
      title: request.title || 'Engineering goal', startedAt: row.started_at,
      finishedAt: row.finished_at, number: result?.number, branch: result?.branch,
      createdAt: row.created_at, attempt: row.attempt };
  }
  listJobs(repoId, limit = 50) {
    return this.db.prepare('SELECT * FROM jobs WHERE repo_id=? ORDER BY created_at DESC LIMIT ?')
      .all(repoId, limit).map((row) => this.jobView(row));
  }
  recoverStale() {
    const now = nowMs();
    this.transaction(() => {
      this.db.prepare(`UPDATE jobs SET status='interrupted',finished_at=?,lease_owner=NULL,
        lease_id=NULL,lease_until=NULL WHERE status='running' AND lease_until<?`)
        .run(now, now);
      this.db.prepare(`UPDATE approvals SET status='interrupted',lease_id=NULL,lease_until=NULL
        WHERE status='executing' AND lease_until<?`).run(now);
    });
  }
  claimJob(workerId, leaseMs = 120_000) {
    return this.transaction(() => {
      const row = this.db.prepare(`SELECT * FROM jobs WHERE status='queued'
        ORDER BY created_at LIMIT 1`).get();
      if (!row) return null;
      const leaseId = randomUUID();
      const started = nowMs();
      const updated = this.db.prepare(`UPDATE jobs SET status='running',started_at=?,
        lease_owner=?,lease_id=?,lease_until=?,attempt=attempt+1
        WHERE id=? AND status='queued'`).run(started, workerId, leaseId, started + leaseMs, row.id);
      if (updated.changes !== 1) return null;
      return { id: row.id, key: row.job_key, kind: row.kind,
        request: this.vault.open(row.payload), workerId, leaseId };
    });
  }
  heartbeat(job, leaseMs = 120_000) {
    return this.db.prepare(`UPDATE jobs SET lease_until=? WHERE id=?
      AND status='running' AND lease_owner=? AND lease_id=?`).run(
      nowMs() + leaseMs, job.id, job.workerId, job.leaseId,
    ).changes === 1;
  }
  finishJob(job, status, result = {}) {
    if (!['completed','failed'].includes(status)) throw conflict('invalid terminal job status');
    const encoded = this.vault.seal(result);
    const updated = this.db.prepare(`UPDATE jobs SET status=?,result=?,finished_at=?,
      lease_owner=NULL,lease_id=NULL,lease_until=NULL
      WHERE id=? AND status='running' AND lease_owner=? AND lease_id=?`)
      .run(status, encoded, nowMs(), job.id, job.workerId, job.leaseId);
    if (updated.changes !== 1) throw conflict('job lease expired or was recovered; manual reconciliation required');
  }
  close() { this.db.close(); }
}
