import { describe, expect, it } from 'vitest';
import { safeGithubUrl, proposalParams } from './github_queue';

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
    expect(safeGithubUrl('https://person:secret@github.com/owner/repo/pull/1')).toBeNull();
  });
});

describe('native GitHub approval parameters', () => {
  const input = {
    branch: ' ryoko/editor-fix ', path: ' src/main.ts ', message: ' fix ', number: '17',
    title: ' New editor ', content: 'complete content',
  };
  it('sends only the fields required for the reviewed operation', () => {
    expect(proposalParams('create_branch', input)).toEqual({ branch: 'ryoko/editor-fix' });
    expect(proposalParams('commit_file', input)).toEqual({
      branch: 'ryoko/editor-fix', path: 'src/main.ts',
      content: 'complete content', message: 'fix',
    });
    expect(proposalParams('open_pull_request', input)).toEqual({
      head: 'ryoko/editor-fix', title: 'New editor', body: 'complete content',
    });
    expect(proposalParams('mark_ready_for_review', input)).toEqual({ number: 17 });
    expect(proposalParams('review_comment', input)).toEqual({
      number: 17, comment: 'complete content',
    });
    expect(() => proposalParams('merge_pull_request', input)).toThrow('Unsupported');
  });
});
