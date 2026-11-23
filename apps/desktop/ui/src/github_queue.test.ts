import { describe, expect, it } from 'vitest';
import {
  buildProposalParams,
  checkLabel,
  clipScope,
  describeProposalScope,
  safeGithubUrl,
} from './github_queue';

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

describe('approval-first proposal scope previews', () => {
  it('states the exact path, branch, message and content size of a commit', () => {
    const scope = describeProposalScope('commit_file', 'approved-owner/repo', {
      branch: 'ryoko/fix-1',
      path: 'src/app.ts',
      message: 'fix: trailing newline',
      content: 'abc',
    });
    expect(scope).toContain('Repository: approved-owner/repo');
    expect(scope).toContain('Commit file: src/app.ts');
    expect(scope).toContain('On branch: ryoko/fix-1');
    expect(scope).toContain('Commit message: fix: trailing newline');
    expect(scope).toContain('Content: 3 bytes of UTF-8 text');
    expect(scope.some((line) => line.includes('no auto-merge, default-branch push'))).toBe(true);
  });

  it('names the target branch and PR head without implying any merge', () => {
    const scope = describeProposalScope('open_pull_request', 'approved-owner/repo', {
      head: 'ryoko/fix-1',
      title: 'Fix the bug',
      body: 'Detailed body',
    });
    expect(scope).toContain('Pull request head branch: ryoko/fix-1');
    expect(scope).toContain('Target: the repository default branch; nothing merges automatically');
    expect(scope).toContain('Body: 13 characters');
    const branch = describeProposalScope('create_branch', 'approved-owner/repo', {
      branch: 'ryoko/fix-2',
    });
    expect(branch).toContain('Create Git branch: ryoko/fix-2');
    expect(branch.some((line) => line.includes('no code is modified'))).toBe(true);
  });

  it('bounds hostile multilineig injected text before it can reach any preview', () => {
    expect(clipScope('keep\n\nthis\tsafe')).toBe('keep this safe');
    expect(clipScope('a'.repeat(400)).length).toBe(160);
    expect(clipScope(undefined, 12)).toBe('');
  });
});

describe('CI check labels never fabricate success', () => {
  it('reports conclusions only when GitHub says the run is completed', () => {
    expect(checkLabel({ status: 'completed', conclusion: 'success' })).toBe('completed · success');
    expect(checkLabel({ status: 'completed', conclusion: 'failure' })).toBe('completed · failure');
    expect(checkLabel({ status: 'in_progress', conclusion: null })).toBe('in_progress · no conclusion yet');
    expect(checkLabel({ status: 'queued' })).toBe('queued · no conclusion yet');
    expect(checkLabel({})).toBe('unknown · no conclusion yet');
  });
});

describe('proposal parameter assembly', () => {
  const fields = (entries: Record<string, string>): Map<string, string> => new Map(Object.entries(entries));

  it('collects validated parameters for each supported operation', () => {
    const branch = buildProposalParams('create_branch', fields({ branch: 'ryoko/fix-1' }));
    expect(branch.params).toEqual({ branch: 'ryoko/fix-1' });

    const commit = buildProposalParams('commit_file', fields({
      branch: 'ryoko/fix-1', path: 'src/app.ts', message: 'fix: hi', content: 'export const x = 1;',
    }));
    expect(commit.params).toEqual({
      branch: 'ryoko/fix-1', path: 'src/app.ts', message: 'fix: hi', content: 'export const x = 1;',
    });

    const pr = buildProposalParams('open_pull_request', fields({
      head: 'ryoko/fix-1', title: 'Fix', body: 'Body',
    }));
    expect(pr.params).toEqual({ head: 'ryoko/fix-1', title: 'Fix', body: 'Body' });

    const comment = buildProposalParams('review_comment', fields({ number: '17', comment: 'Looks good' }));
    expect(comment.params).toEqual({ number: 17, comment: 'Looks good' });
  });

  it('rejects incomplete or oversized operations before anything is queued', () => {
    expect(buildProposalParams('create_branch', fields({})).error).toContain('branch');
    expect(buildProposalParams('commit_file', fields({ branch: 'ryoko/x' })).error)
      .toContain('path and commit message');
    expect(buildProposalParams('review_comment', fields({ number: 'zero', comment: 'c' })).error)
      .toContain('valid pull request number');
    const oversized = buildProposalParams('open_pull_request', fields({
      head: 'ryoko/fix-1', title: 'Fix', body: 'b'.repeat(12_001),
    }));
    expect(oversized.error).toContain('12,000 characters');
    expect(buildProposalParams('merge_pr', fields({})).error).toContain('unsupported');
  });
});
