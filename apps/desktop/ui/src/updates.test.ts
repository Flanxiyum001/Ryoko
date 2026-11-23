import { describe, expect, it } from 'vitest';
import {
  AUTO_CHECK_INTERVAL_MS, DEFAULT_UPDATE_PREFS, UPDATE_PREFS_STORAGE, loadPrefs, progressLabel,
  sanitizePrefs, shouldAnnounce, shouldAutoCheck, type UpdateInfo,
} from './updates';

const info: UpdateInfo = { version: '0.1.0-alpha.9', currentVersion: '0.1.0-alpha.8', notes: '', date: null };

describe('update preferences', () => {
  it('defaults and sanitizes stored values', () => {
    expect(loadPrefs({ getItem: () => null })).toEqual(DEFAULT_UPDATE_PREFS);
    expect(loadPrefs({ getItem: () => '{bad' })).toEqual(DEFAULT_UPDATE_PREFS);
    expect(sanitizePrefs({ autoCheck: 'no', skippedVersion: 'x'.repeat(100), lastCheck: -1 })).toEqual(DEFAULT_UPDATE_PREFS);
    expect(loadPrefs({ getItem: (key) => (key === UPDATE_PREFS_STORAGE ? '{"autoCheck":false}' : null) }).autoCheck).toBe(false);
  });
});

describe('when to check', () => {
  const prefs = { ...DEFAULT_UPDATE_PREFS, lastCheck: 1_000 };
  it('checks at most every interval', () => {
    expect(shouldAutoCheck(prefs, 1_000 + AUTO_CHECK_INTERVAL_MS - 1)).toBe(false);
    expect(shouldAutoCheck(prefs, 1_000 + AUTO_CHECK_INTERVAL_MS)).toBe(true);
  });
  it('never auto-checks when disabled', () => {
    expect(shouldAutoCheck({ ...prefs, autoCheck: false }, 10 ** 13)).toBe(false);
  });
  it('recovers from a clock that moved backwards', () => {
    expect(shouldAutoCheck({ ...prefs, lastCheck: 10 ** 13 }, 5_000)).toBe(true);
  });
});

describe('when to announce', () => {
  it('hides a skipped version from automatic checks only', () => {
    const prefs = { ...DEFAULT_UPDATE_PREFS, skippedVersion: '0.1.0-alpha.9' };
    expect(shouldAnnounce(info, prefs, false)).toBe(false);
    expect(shouldAnnounce(info, prefs, true)).toBe(true);
    expect(shouldAnnounce({ ...info, version: '0.1.0-alpha.10' }, prefs, false)).toBe(true);
    expect(shouldAnnounce(null, DEFAULT_UPDATE_PREFS, true)).toBe(false);
  });
});

describe('progress', () => {
  it('reports percent when the size is known', () => {
    expect(progressLabel(5 * 1024 * 1024, 10 * 1024 * 1024)).toBe('Downloading update… 50% (5.0 MB of 10.0 MB)');
    expect(progressLabel(2048, null)).toBe('Downloading update… 2 KB');
    expect(progressLabel(20, 10)).toContain('100%');
  });
});
