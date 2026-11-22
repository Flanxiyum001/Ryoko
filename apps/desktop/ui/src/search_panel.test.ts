import { describe, expect, it } from 'vitest';
import { annotatePreview, groupHits, summarizeMatches, type SearchHit } from './search_panel';

function hit(overrides: Partial<SearchHit>): SearchHit {
  return { path: 'a.rs', line: 1, start: 0, end: 3, preview: 'abc def', ...overrides };
}

describe('groupHits', () => {
  it('groups per path while preserving order', () => {
    const groups = groupHits([
      hit({ path: 'a.rs', line: 1 }),
      hit({ path: 'b.rs', line: 4 }),
      hit({ path: 'a.rs', line: 9 }),
    ]);
    expect(groups.map((group) => group.path)).toEqual(['a.rs', 'b.rs']);
    expect(groups[0].hits.map((item) => item.line)).toEqual([1, 9]);
  });
});

describe('annotatePreview', () => {
  it('splits the preview around the reported span', () => {
    const segments = annotatePreview(hit({ preview: 'fn main() {}', start: 3, end: 7 }));
    expect(segments.map((segment) => segment.text)).toEqual(['fn ', 'main', '() {}']);
    expect(segments.map((segment) => segment.match)).toEqual([false, true, false]);
  });

  it('skips the leading ellipsis marker', () => {
    const segments = annotatePreview(hit({ preview: '\u2026bc def', start: 1, end: 3 }));
    expect(segments.map((segment) => segment.text)).toEqual(['\u2026', 'bc', ' def']);
  });

  it('returns the whole preview for empty spans', () => {
    const segments = annotatePreview(hit({ preview: 'abc', start: 2, end: 2 }));
    expect(segments).toEqual([{ text: 'abc', match: false }]);
  });
});

describe('summarizeMatches', () => {
  it('describes counts and truncation', () => {
    expect(summarizeMatches(0, false)).toBe('No matches.');
    expect(summarizeMatches(1, false)).toBe('1 match');
    expect(summarizeMatches(12, true)).toBe('12 matches (truncated)');
  });
});
