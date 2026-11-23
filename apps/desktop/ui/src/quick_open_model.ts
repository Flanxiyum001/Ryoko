/** Pure, deterministic path ranking for the Phase 4 quick-open palette. */
export interface QuickOpenMatch {
  path: string;
  score: number;
}

/**
 * The workspace path list prepared once per workspace version.
 *
 * Quick open must never rescan the filesystem per keystroke, and re-deriving
 * 10,000 lowercase paths on every keystroke was the measured bottleneck in
 * `npm run bench:quick-open`. The palette builds this once whenever the loaded
 * file list changes and then only scores against it.
 */
export interface QuickOpenIndex {
  /** Unique, non-empty paths in workspace order. */
  readonly paths: readonly string[];
  /** `paths[n].toLowerCase()`, computed once. */
  readonly lowered: readonly string[];
  /** Basename of `lowered[n]`, computed once. */
  readonly bases: readonly string[];
  /** Membership for recent-file filtering without a linear scan. */
  readonly pathSet: ReadonlySet<string>;
}

const MAX_QUERY = 120;
const MAX_RECENTS = 20;

function lowerBase(lower: string): string {
  return lower.slice(lower.lastIndexOf('/') + 1);
}

/** Deduplicates and precomputes everything a keystroke needs. */
export function buildQuickOpenIndex(paths: readonly string[]): QuickOpenIndex {
  const kept: string[] = [];
  const lowered: string[] = [];
  const bases: string[] = [];
  const seen = new Set<string>();
  for (const path of paths) {
    if (!path || seen.has(path)) continue;
    seen.add(path);
    kept.push(path);
    const lower = path.toLowerCase();
    lowered.push(lower);
    bases.push(lowerBase(lower));
  }
  return { paths: kept, lowered, bases, pathSet: seen };
}

/**
 * Reuses one index for as long as the loaded file list is the same array.
 *
 * The workspace loader replaces the array when the file list changes, so an
 * identity check is enough: a keystroke never rebuilds the index and never
 * rescans anything.
 */
export function createQuickOpenIndexCache(): (files: readonly string[]) => QuickOpenIndex {
  let current: readonly string[] | null = null;
  let index = buildQuickOpenIndex([]);
  return (files) => {
    if (files !== current) {
      current = files;
      index = buildQuickOpenIndex(files);
    }
    return index;
  };
}

/** Subsequence match with filename, path-segment and adjacency bonuses. */
function scoreLowered(lower: string, base: string, query: string): number | null {
  if (query.length > lower.length) return null;
  let offset = 0;
  let previous = -1;
  let score = 0;
  // Walk code points, not code units, so astral characters (emoji, CJK
  // extensions) match as one letter instead of two broken surrogates. Indexing
  // avoids allocating a string iterator per path on every keystroke.
  for (let position = 0; position < query.length; position += 1) {
    const code = query.charCodeAt(position);
    let letter = query[position];
    if (code >= 0xd800 && code <= 0xdbff && position + 1 < query.length) {
      letter = query.slice(position, position + 2);
      position += 1;
    }
    const index = lower.indexOf(letter, offset);
    if (index === -1) return null;
    score += 10;
    if (index === 0 || '/._- '.includes(lower[index - 1])) score += 24;
    if (previous >= 0) {
      if (index === previous + 1) score += 12;
      else score -= Math.min(12, index - previous - 1);
    } else {
      score -= Math.min(20, index);
    }
    previous = index;
    offset = index + 1;
  }
  if (base === query) score += 240;
  else if (base.startsWith(query)) score += 140;
  else if (base.includes(query)) score += 65;
  if (lower === query) score += 300;
  return score;
}

export function scorePath(path: string, rawQuery: string): number | null {
  const query = rawQuery.trim().toLowerCase();
  if (!query) return 0;
  if (query.length > MAX_QUERY) return null;
  const lower = path.toLowerCase();
  return scoreLowered(lower, lowerBase(lower), query);
}

/**
 * Total ordering used by the bounded top-K insertion.
 *
 * The final tie-break is a code-unit comparison rather than `localeCompare`:
 * ranking 10,000 matches called `localeCompare` tens of thousands of times and
 * that dominated the palette's latency. Ordering stays deterministic; only
 * non-ASCII collation differs from the previous locale-aware tie-break.
 */
function compareMatches(left: QuickOpenMatch, right: QuickOpenMatch): number {
  if (left.score !== right.score) return right.score - left.score;
  const leftLength = left.path.length;
  const rightLength = right.path.length;
  if (leftLength !== rightLength) return leftLength - rightLength;
  return left.path < right.path ? -1 : left.path > right.path ? 1 : 0;
}

function cappedLimit(limit: number): number {
  return Math.max(1, Math.min(100, Math.trunc(limit) || 40));
}

/**
 * Inserts a candidate into an already ordered best-`capped` list.
 *
 * Broad queries match almost every path; a full sort of 10,000 entries to show
 * 40 rows was the remaining measured bottleneck. Binary-search insertion into a
 * 40-element array is the same result for a fraction of the comparisons.
 */
function insertBounded(best: QuickOpenMatch[], candidate: QuickOpenMatch, capped: number): void {
  if (best.length === capped && compareMatches(candidate, best[capped - 1]) >= 0) return;
  let low = 0;
  let high = best.length;
  while (low < high) {
    const middle = (low + high) >>> 1;
    if (compareMatches(candidate, best[middle]) < 0) high = middle;
    else low = middle + 1;
  }
  best.splice(low, 0, candidate);
  if (best.length > capped) best.pop();
}

/**
 * Ranked matches against a prepared index.
 *
 * `recent` breaks ties without hiding better matches. When more than `limit`
 * files match, only the best `limit` are kept: the cheaper partial selection
 * avoids sorting a whole 10,000-entry result set to show 40 rows.
 */
export function rankIndex(
  index: QuickOpenIndex,
  query: string,
  recent: readonly string[] = [],
  limit = 40,
): QuickOpenMatch[] {
  const capped = cappedLimit(limit);
  const trimmed = query.trim().toLowerCase();
  if (trimmed.length > MAX_QUERY) return [];
  const recentPositions = new Map<string, number>();
  recent.slice(0, MAX_RECENTS).forEach((path, position) => recentPositions.set(path, position));
  const emptyQuery = trimmed.length === 0;
  const matches: QuickOpenMatch[] = [];
  for (let position = 0; position < index.paths.length; position += 1) {
    const path = index.paths[position];
    const score = emptyQuery ? 0 : scoreLowered(index.lowered[position], index.bases[position], trimmed);
    if (score === null) continue;
    const recentIndex = recentPositions.get(path);
    const recentBonus = recentIndex === undefined ? 0 : 20 - recentIndex;
    const emptyBonus = emptyQuery && recentIndex !== undefined ? 1000 : 0;
    insertBounded(matches, { path, score: score + recentBonus + emptyBonus }, capped);
  }
  return matches;
}

/** Ranked quick-open matches; convenience wrapper that builds a fresh index. */
export function rankFiles(
  paths: readonly string[],
  query: string,
  recent: readonly string[] = [],
  limit = 40,
): QuickOpenMatch[] {
  return rankIndex(buildQuickOpenIndex(paths), query, recent, limit);
}

export function nextQuickOpenSelection(current: number, count: number, movement: number): number {
  if (count <= 0) return -1;
  return ((Math.max(0, current) + movement) % count + count) % count;
}
