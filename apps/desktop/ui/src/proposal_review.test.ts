import { describe, expect, it } from 'vitest';
import { previewAccepted, proposalHunks } from './proposal_review';

describe('reviewable proposed changes', () => {
  it('applies exactly the approved hunks, not the entire model answer', () => {
    const original = 'one\ntwo\nthree\nfour\nfive\n';
    const replacement = 'ONE\ntwo\nthree\nFOUR\nfive\n';
    const hunks = proposalHunks(original, replacement);
    expect(hunks).toHaveLength(2);
    expect(previewAccepted(original, 0, original.length, hunks, new Set([hunks[0].id])))
      .toBe('ONE\ntwo\nthree\nfour\nfive\n');
    expect(previewAccepted(original, 0, original.length, hunks, new Set()))
      .toBe(original);
  });
  it('preserves unrelated text around a selection', () => {
    const source = 'header\nbefore\nend\nfooter\n';
    const start = source.indexOf('before');
    const hunks = proposalHunks('before\n', 'after\n');
    expect(previewAccepted(source, start, start + 7, hunks, new Set([1])))
      .toBe('header\nafter\nend\nfooter\n');
  });
  it('refuses corrupt offsets and stale hunks', () => {
    const hunks = proposalHunks('old\n', 'new\n');
    expect(() => previewAccepted('other\n', 0, 6, hunks, new Set([1]))).toThrow();
    expect(() => previewAccepted('old\n', -1, 4, hunks, new Set([1]))).toThrow();
  });
  it('handles insertion, deletion and unchanged code', () => {
    expect(proposalHunks('same\n', 'same\n')).toEqual([]);
    expect(previewAccepted('', 0, 0, proposalHunks('', 'added\n'), new Set([1])))
      .toBe('added\n');
    expect(previewAccepted('delete\n', 0, 7, proposalHunks('delete\n', ''), new Set([1])))
      .toBe('');
  });
});
