import { describe, expect, it } from 'vitest';
import {
  RECOVERY_ENABLED_KEY,
  createRecoverySessionId,
  describeRecoverableEntry,
  loadRecoveryEnabled,
  saveRecoveryEnabled,
} from './recovery';

describe('crash recovery preferences and formatting', () => {
  it('defaults to opt-in disabled and persists explicit toggle', () => {
    const store = new Map<string, string>();
    const storage = {
      getItem: (key: string) => store.get(key) ?? null,
      setItem: (key: string, value: string) => { store.set(key, value); },
    };
    expect(loadRecoveryEnabled(storage)).toBe(false);
    saveRecoveryEnabled(storage, true);
    expect(store.get(RECOVERY_ENABLED_KEY)).toBe('true');
    expect(loadRecoveryEnabled(storage)).toBe(true);
    saveRecoveryEnabled(storage, false);
    expect(loadRecoveryEnabled(storage)).toBe(false);
  });

  it('generates safe bounded session identifiers', () => {
    const id = createRecoverySessionId();
    expect(id).toMatch(/^[A-Za-z0-9_-]{8,64}$/);
  });

  it('labels disk-changed and deleted files clearly for consent review', () => {
    const base = {
      path: 'src/main.rs',
      fileUri: 'file:///repo/src/main.rs',
      content: 'new',
      diskContent: 'old',
      diskExists: true,
      diskChanged: false,
      revision: 2,
      sessionId: 'desktop-1',
      savedAtMs: 1_700_000_000_000,
    };
    expect(describeRecoverableEntry(base)).toContain('unsaved edits');
    expect(describeRecoverableEntry({ ...base, diskChanged: true })).toContain('changed on disk');
    expect(describeRecoverableEntry({ ...base, diskExists: false })).toContain('deleted on disk');
  });
});
