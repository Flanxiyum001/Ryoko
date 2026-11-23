import { describe, expect, it } from 'vitest';
import {
  buildQuickOpenIndex,
  createQuickOpenIndexCache,
  nextQuickOpenSelection,
  rankFiles,
  rankIndex,
  scorePath,
  type QuickOpenMatch,
} from './quick_open_model';
import { BENCH_QUERIES, buildWorkspacePaths, recentPaths } from '../bench/quick_open_fixture';

/**
 * Reference ranking: score everything, sort everything, then cut. It is
 * deliberately the simple version the palette used before the bounded top-K
 * selection, and the tests below require the fast path to produce exactly this
 * order.
 */
function referenceRank(
  paths: readonly string[],
  query: string,
  recent: readonly string[] = [],
  limit = 40,
): QuickOpenMatch[] {
  const capped = Math.max(1, Math.min(100, Math.trunc(limit) || 40));
  const trimmed = query.trim().toLowerCase();
  if (trimmed.length > 120) return [];
  const positions = new Map(recent.slice(0, 20).map((path, index) => [path, index]));
  const seen = new Set<string>();
  const matches: QuickOpenMatch[] = [];
  for (const path of paths) {
    if (!path || seen.has(path)) continue;
    seen.add(path);
    const score = scorePath(path, query);
    if (score === null) continue;
    const recentIndex = positions.get(path);
    const recentBonus = recentIndex === undefined ? 0 : 20 - recentIndex;
    const emptyBonus = !trimmed && recentIndex !== undefined ? 1000 : 0;
    matches.push({ path, score: score + recentBonus + emptyBonus });
  }
  matches.sort((left, right) => right.score - left.score
    || left.path.length - right.path.length
    || (left.path < right.path ? -1 : left.path > right.path ? 1 : 0));
  return matches.slice(0, capped);
}

describe('quick-open path ranking', () => {
  it('finds case-insensitive subsequences and rejects nonmatches', () => {
    expect(scorePath('src/RemoteClient.ts', 'rct')).not.toBeNull();
    expect(scorePath('src/RemoteClient.ts', 'missing')).toBeNull();
  });

  it('prefers exact basenames and contiguous filename prefixes over deep fuzzy matches', () => {
    const paths = ['src/components/client-view.ts', 'docs/client-guide.md', 'client'];
    expect(rankFiles(paths, 'client').map((match) => match.path)[0]).toBe('client');
  });

  it('ranks an exact path match first, then a basename prefix, then a path match', () => {
    const paths = ['src/parser/readme-client.ts', 'client-notes.md', 'client'];
    expect(rankFiles(paths, 'client').map((match) => match.path)).toEqual([
      'client', 'client-notes.md', 'src/parser/readme-client.ts',
    ]);
  });

  it('matches fuzzy subsequences across directory and file segments', () => {
    const paths = ['src/quick_open_view.ts', 'crates/ryoko-cli/src/main.rs', 'docs/README.md'];
    const matches = rankFiles(paths, 'qov').map((match) => match.path);
    expect(matches).toContain('src/quick_open_view.ts');
    expect(matches).not.toContain('docs/README.md');
  });

  it('matches astral characters as single letters', () => {
    const paths = ['docs/🎯 plan.md', 'src/target.ts'];
    expect(rankFiles(paths, '🎯').map((match) => match.path)).toEqual(['docs/🎯 plan.md']);
    expect(scorePath('src/日本語.ts', '本')).not.toBeNull();
  });

  it('uses recent files when the palette first opens', () => {
    const paths = ['a.ts', 'b.ts', 'c.ts'];
    expect(rankFiles(paths, '', ['c.ts', 'a.ts']).map((match) => match.path)).toEqual([
      'c.ts', 'a.ts', 'b.ts',
    ]);
  });

  it('does not let recent unrelated files defeat a better query match', () => {
    const paths = ['index', 'src/index.test.ts'];
    expect(rankFiles(paths, 'index', ['src/index.test.ts'])[0].path).toBe('index');
  });

  it('returns no matches for an unmatched query or an empty workspace', () => {
    expect(rankFiles(['a.ts', 'b.ts'], 'zzz')).toEqual([]);
    expect(rankFiles([], 'a')).toEqual([]);
    expect(rankFiles([], '')).toEqual([]);
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

describe('prepared quick-open index', () => {
  it('deduplicates, drops empty paths and exposes membership', () => {
    const index = buildQuickOpenIndex(['a.ts', '', 'a.ts', 'src/b.ts']);
    expect([...index.paths]).toEqual(['a.ts', 'src/b.ts']);
    expect([...index.lowered]).toEqual(['a.ts', 'src/b.ts']);
    expect([...index.bases]).toEqual(['a.ts', 'b.ts']);
    expect(index.pathSet.has('src/b.ts')).toBe(true);
    expect(index.pathSet.has('missing.ts')).toBe(false);
  });

  it('rebuilds only when the loaded file list changes', () => {
    const cache = createQuickOpenIndexCache();
    const files = ['a.ts', 'src/b.ts'];
    const first = cache(files);
    // Same array identity: a keystroke reuses the prepared index instead of
    // rescanning or re-lowercasing 10,000 paths.
    expect(cache(files)).toBe(first);
    const copy = [...files];
    const rebuilt = cache(copy);
    expect(rebuilt).not.toBe(first);
    expect(cache(copy)).toBe(rebuilt);
  });

  it('is not mutated by ranking, so one index serves every keystroke', () => {
    const index = buildQuickOpenIndex(['src/RemoteClient.ts', 'main', 'docs/guide.md']);
    const before = JSON.stringify(index.paths);
    const first = rankIndex(index, 'rc');
    const second = rankIndex(index, 'rc');
    expect(first).toEqual(second);
    expect(JSON.stringify(index.paths)).toBe(before);
  });
});

describe('10k quick-open ranking', () => {
  const paths = buildWorkspacePaths();
  const recents = recentPaths(paths);

  it('produces exactly the full-sort order on a 10,000-path workspace', () => {
    for (const { query } of BENCH_QUERIES) {
      expect(rankFiles(paths, query, recents))
        .toEqual(referenceRank(paths, query, recents));
    }
    expect(rankFiles(paths, 'src', recents, 7))
      .toEqual(referenceRank(paths, 'src', recents, 7));
  });

  it('is deterministic, bounded and cheap enough to stay in one keystroke', () => {
    const first = rankIndex(buildQuickOpenIndex(paths), 'ma', recents);
    const second = rankIndex(buildQuickOpenIndex(paths), 'ma', recents);
    expect(first).toEqual(second);
    expect(first.length).toBeLessThanOrEqual(40);
    expect(paths).toHaveLength(10_000);
  });
});
