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
