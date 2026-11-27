/**
 * Opt-in crash recovery preference and typed recovery snapshot models.
 *
 * Crash recovery is opt-in by default so Ryoko never persists unsaved buffer
 * copies without user consent. Restoring a snapshot always loads into an
 * unsaved editor buffer after explicit review — never silently writes to disk.
 */

export const RECOVERY_ENABLED_KEY = 'ryoko.recovery.enabled.v1';

export interface RecoverableEntry {
  path: string;
  fileUri: string;
  content: string;
  diskContent: string;
  diskExists: boolean;
  diskChanged: boolean;
  revision: number;
  sessionId: string;
  savedAtMs: number;
}

export interface RecoveryListResult {
  entries: RecoverableEntry[];
  corruptedCount: number;
}

export function loadRecoveryEnabled(storage: Pick<Storage, 'getItem'>): boolean {
  try {
    return storage.getItem(RECOVERY_ENABLED_KEY) === 'true';
  } catch {
    return false;
  }
}

export function saveRecoveryEnabled(storage: Pick<Storage, 'setItem'>, enabled: boolean): void {
  try {
    storage.setItem(RECOVERY_ENABLED_KEY, enabled ? 'true' : 'false');
  } catch {
    // Ignore storage quota / privacy mode failures.
  }
}

export function createRecoverySessionId(): string {
  const random = typeof crypto !== 'undefined' && typeof crypto.randomUUID === 'function'
    ? crypto.randomUUID()
    : `${Date.now()}-${Math.random().toString(16).slice(2, 10)}`;
  return `desktop-${random}`.replace(/[^A-Za-z0-9_-]/g, '').slice(0, 64);
}

export function describeRecoverableEntry(entry: RecoverableEntry): string {
  const timestamp = new Date(entry.savedAtMs).toLocaleString();
  if (!entry.diskExists) return `${entry.path} · file deleted on disk · saved ${timestamp}`;
  if (entry.diskChanged) return `${entry.path} · file changed on disk since crash · saved ${timestamp}`;
  return `${entry.path} · unsaved edits · saved ${timestamp}`;
}
