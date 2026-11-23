import { describe, expect, it } from 'vitest';
import { accountSummary, describeSync, syncHint, type GitHubStatus, type SyncStatus } from './github_account';

const repo: SyncStatus = {
  isRepository: true, branch: 'main', upstream: 'origin/main', ahead: 0, behind: 0, dirty: false, remoteUrl: 'https://github.com/acme/app.git',
};

describe('GitHub sync labels', () => {
  it('names the branch, both counters and pending work', () => {
    expect(describeSync(repo)).toBe('main · ↑0 ↓0');
    expect(describeSync({ ...repo, ahead: 2, behind: 1, dirty: true }))
      .toBe('main · ↑2 ↓1 · uncommitted changes');
    expect(describeSync({ ...repo, branch: null })).toContain('detached HEAD');
    expect(describeSync({ ...repo, isRepository: false })).toBe('Not a Git repository');
    expect(describeSync({ ...repo, upstream: null })).toBe('main · no upstream');
  });

  it('explains the next safe action and never suggests forcing', () => {
    expect(syncHint({ ...repo, isRepository: false }, true)).toContain('Open a Git repository');
    expect(syncHint(repo, false)).toContain('Connect GitHub');
    expect(syncHint({ ...repo, upstream: null }, true)).toContain('origin');
    expect(syncHint({ ...repo, ahead: 1, behind: 1 }, true)).toContain('diverged');
    expect(syncHint({ ...repo, behind: 3 }, true)).toContain('fast-forwards');
    expect(syncHint({ ...repo, ahead: 3 }, true)).toContain('Push');
  });
});

describe('GitHub account summary', () => {
  const base: GitHubStatus = {
    connected: false, login: '', source: 'none', persisted: false,
    signInConfigured: true, storedCredentialAvailable: false,
  };

  it('distinguishes a stored credential from a live session', () => {
    expect(accountSummary(base)).toBe('not connected');
    expect(accountSummary({ ...base, storedCredentialAvailable: true })).toBe('credential available');
    expect(accountSummary({ ...base, connected: true, login: 'octocat', persisted: true }))
      .toBe('@octocat');
    expect(accountSummary({ ...base, connected: true, login: 'octocat', persisted: false }))
      .toBe('@octocat · this session');
  });
});
