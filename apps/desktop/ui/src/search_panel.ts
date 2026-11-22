//! Pure helpers for the workspace search panel: grouping flat hits per file,
//! splitting previews into matchable segments, and summarizing counts.

export interface SearchHit {
  path: string;
  line: number;
  start: number;
  end: number;
  preview: string;
}

export interface SearchGroup {
  path: string;
  hits: SearchHit[];
}

/** Groups hits by path, preserving workspace order and line order. */
export function groupHits(hits: readonly SearchHit[]): SearchGroup[] {
  const groups = new Map<string, SearchGroup>();
  for (const hit of hits) {
    let group = groups.get(hit.path);
    if (!group) {
      group = { path: hit.path, hits: [] };
      groups.set(hit.path, group);
    }
    group.hits.push(hit);
  }
  return [...groups.values()];
}

export interface PreviewSegment {
  text: string;
  match: boolean;
}

/**
 * Splits a hit's preview line into plain and highlighted segments around the
 * match that the Rust shell reported by byte offsets.
 */
export function annotatePreview(hit: SearchHit): PreviewSegment[] {
  const preview = hit.preview;
  if (hit.end <= hit.start || hit.start >= preview.length) {
    return [{ text: preview, match: false }];
  }
  const ellipsis = preview.startsWith('\u2026') ? 1 : 0;
  const start = Math.min(preview.length, Math.max(ellipsis, hit.start));
  const end = Math.min(preview.length, Math.max(start, hit.end));
  return [
    { text: preview.slice(0, start), match: false },
    { text: preview.slice(start, end), match: true },
    { text: preview.slice(end), match: false },
  ].filter((segment) => segment.text.length > 0);
}

export function summarizeMatches(totalHits: number, truncated: boolean): string {
  if (totalHits === 0) return 'No matches.';
  const suffix = truncated ? ' (truncated)' : '';
  return `${totalHits} match${totalHits === 1 ? '' : 'es'}${suffix}`;
}
