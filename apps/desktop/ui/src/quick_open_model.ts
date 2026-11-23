/** Pure, deterministic path ranking for the Phase 4 quick-open palette. */
export interface QuickOpenMatch {
  path: string;
  score: number;
}

/** Subsequence match with filename, path-segment and adjacency bonuses. */
export function scorePath(path: string, rawQuery: string): number | null {
  const query = rawQuery.trim().toLowerCase();
  if (!query) return 0;
  if (query.length > 120) return null;
  const lower = path.toLowerCase();
  const base = lower.slice(lower.lastIndexOf('/') + 1);
  let offset = 0;
  let previous = -1;
  let score = 0;
  for (const letter of query) {
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

/** Ranked quick-open matches; recent files break ties without hiding better matches. */
export function rankFiles(
  paths: readonly string[],
  query: string,
  recent: readonly string[] = [],
  limit = 40,
): QuickOpenMatch[] {
  const cappedLimit = Math.max(1, Math.min(100, Math.trunc(limit) || 40));
  const recentPositions = new Map(recent.slice(0, 20).map((path, index) => [path, index]));
  const matches: QuickOpenMatch[] = [];
  const seen = new Set<string>();
  for (const path of paths) {
    if (!path || seen.has(path)) continue;
    seen.add(path);
    const score = scorePath(path, query);
    if (score === null) continue;
    const recentIndex = recentPositions.get(path);
    const recentBonus = recentIndex === undefined ? 0 : 20 - recentIndex;
    const emptyBonus = !query.trim() && recentIndex !== undefined ? 1000 : 0;
    matches.push({ path, score: score + recentBonus + emptyBonus });
  }
  matches.sort((a, b) => b.score - a.score || a.path.length - b.path.length || a.path.localeCompare(b.path));
  return matches.slice(0, cappedLimit);
}

export function nextQuickOpenSelection(current: number, count: number, movement: number): number {
  if (count <= 0) return -1;
  return ((Math.max(0, current) + movement) % count + count) % count;
}
