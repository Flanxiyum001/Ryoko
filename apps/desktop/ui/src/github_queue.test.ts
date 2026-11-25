import { describe, expect, it } from 'vitest';
import { safeGithubUrl } from './github_queue';

describe('native GitHub queue links', () => {
  it('permits only canonical GitHub PR and issue detail URLs', () => {
    expect(safeGithubUrl('https://github.com/owner/repo/pull/17'))
      .toBe('https://github.com/owner/repo/pull/17');
    expect(safeGithubUrl('https://github.com/owner/repo/issues/3?token=hidden'))
      .toBe('https://github.com/owner/repo/issues/3');
  });
  it('rejects offsite links, embedded credentials and javascript', () => {
    expect(safeGithubUrl('https://github.com.evil.example/owner/repo/pull/1')).toBeNull();
    expect(safeGithubUrl('javascript:alert(1)')).toBeNull();
    expect(safeGithubUrl('http://github.com/owner/repo/pull/1')).toBeNull();
    expect(safeGithubUrl('https://github.com@evil.example/owner/repo/pull/1')).toBeNull();
    expect(safeGithubUrl('https://github.com/owner/repo/../../settings')).toBeNull();
  });
});

import { checkPresentation, mergePresentation, type PullDetail } from './github_pull_card';
const detail: PullDetail = {
  author: 'agent', agent: true, created_at: '', head: 'feature', base: 'main',
  changed_files: 6, additions: 76, deletions: 23, mergeable: true, mergeable_state: 'clean', merged: false,
  files: [], checks: [{ name: 'ci', status: 'completed', conclusion: 'success' }], checks_complete: true, reviews: [],
};
describe('PR detail truthfulness', () => {
  it('never claims required checks passed based on check runs alone', () => {
    expect(checkPresentation(detail).label).toBe('Reported checks passed');
    expect(checkPresentation({ ...detail, checks_complete: false }).tone).toBe('unknown');
    expect(checkPresentation({ ...detail, checks: [] }).tone).toBe('unknown');
    expect(checkPresentation({ ...detail, checks: [{ name: 'ci', status: 'completed', conclusion: 'failure' }] }).tone).toBe('bad');
  });
  it('only labels a clean, open non-draft PR ready', () => {
    const pull = { number: 1, title: 'PR', state: 'open', detail };
    expect(mergePresentation(pull).label).toBe('Ready to merge');
    expect(mergePresentation({ ...pull, draft: true }).tone).toBe('unknown');
    expect(mergePresentation({ ...pull, detail: { ...detail, mergeable: null } }).tone).toBe('unknown');
    expect(mergePresentation({ ...pull, detail: { ...detail, mergeable: false } }).tone).toBe('bad');
  });
  it('rejects credentials and non-default ports in GitHub links', () => {
    expect(safeGithubUrl('https://user:password@github.com/owner/repo/pull/1')).toBeNull();
    expect(safeGithubUrl('https://github.com:444/owner/repo/pull/1')).toBeNull();
  });
});
