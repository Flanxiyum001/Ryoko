import { describe, expect, it } from 'vitest';
import { nextQuickOpenSelection, rankFiles, scorePath } from './quick_open_model';

describe('quick-open path ranking', () => {
  it('finds case-insensitive subsequences and rejects nonmatches', () => {
    expect(scorePath('src/RemoteClient.ts', 'rct')).not.toBeNull();
    expect(scorePath('src/RemoteClient.ts', 'missing')).toBeNull();
  });

  it('prefers exact basenames and contiguous filename prefixes over deep fuzzy matches', () => {
    const paths = ['src/components/client-view.ts', 'docs/client-guide.md', 'client'];
    expect(rankFiles(paths, 'client').map((match) => match.path)[0]).toBe('client');
  });

  it('uses recent files when the palette first opens', () => {
    const paths = ['a.ts', 'b.ts', 'c.ts'];
    expect(rankFiles(paths, '', ['c.ts', 'a.ts']).map((match) => match.path)).toEqual([
      'c.ts', 'a.ts', 'b.ts',
    ]);
  });

  it('does not let recent unrelated files defeat a better query match', () => {
    const paths = ['z/index.ts', 'src/index.test.ts'];
    expect(rankFiles(paths, 'index', ['src/index.test.ts'])[0].path).toBe('z/index.ts');
  });

  it('limits results and deduplicates incoming paths', () => {
    const matches = rankFiles(['a.ts', 'a.ts', 'b.ts', 'c.ts'], 'ts', [], 2);
    expect(matches).toHaveLength(2);
    expect(new Set(matches.map((match) => match.path)).size).toBe(2);
  });

  it('cycles keyboard navigation safely when there are no results', () => {
    expect(nextQuickOpenSelection(0, 0, 1)).toBe(-1);
    expect(nextQuickOpenSelection(2, 3, 1)).toBe(0);
    expect(nextQuickOpenSelection(0, 3, -1)).toBe(2);
  });
});
