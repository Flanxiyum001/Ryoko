/** Pure line-diff review planner for selection and agent worktree proposals.
 * No filesystem writes occur here. Every hunk is opt-in.
 */
export interface ProposalHunk {
  id: number;
  start: number;
  end: number;
  before: string;
  after: string;
}

function lines(text: string): string[] {
  return text.match(/[^\n]*\n|[^\n]+$/g) ?? [];
}

export function proposalHunks(original: string, replacement: string): ProposalHunk[] {
  if (original === replacement) return [];
  const oldLines = lines(original);
  const newLines = lines(replacement);
  const n = oldLines.length;
  const m = newLines.length;
  // A large model answer remains reviewable as one bounded replacement.
  if (n * m > 100_000) return [{ id: 1, start: 0, end: original.length, before: original, after: replacement }];
  const width = m + 1;
  const table = new Uint32Array((n + 1) * (m + 1));
  const at = (i: number, j: number): number => table[i * width + j];
  for (let i = n - 1; i >= 0; i--) {
    for (let j = m - 1; j >= 0; j--) {
      table[i * width + j] = oldLines[i] === newLines[j]
        ? 1 + at(i + 1, j + 1)
        : Math.max(at(i + 1, j), at(i, j + 1));
    }
  }
  const hunks: ProposalHunk[] = [];
  let i = 0;
  let j = 0;
  let offset = 0;
  while (i < n || j < m) {
    if (i < n && j < m && oldLines[i] === newLines[j]) {
      offset += oldLines[i].length;
      i++;
      j++;
      continue;
    }
    const start = offset;
    let before = '';
    let after = '';
    while (i < n || j < m) {
      if (i < n && j < m && oldLines[i] === newLines[j]) break;
      if (i < n && (j >= m || at(i + 1, j) >= at(i, j + 1))) {
        before += oldLines[i];
        offset += oldLines[i].length;
        i++;
      } else if (j < m) {
        after += newLines[j];
        j++;
      }
    }
    hunks.push({ id: hunks.length + 1, start, end: offset, before, after });
  }
  return hunks;
}

export function previewAccepted(
  fullText: string,
  startOffset: number,
  endOffset: number,
  hunks: ProposalHunk[],
  accepted: ReadonlySet<number>,
): string {
  if (!Number.isInteger(startOffset) || !Number.isInteger(endOffset)
    || startOffset < 0 || endOffset < startOffset || endOffset > fullText.length) {
    throw new Error('Invalid selection offsets');
  }
  const selected = fullText.slice(startOffset, endOffset);
  let cursor = 0;
  let updated = '';
  for (const hunk of hunks) {
    if (hunk.start < cursor || hunk.end > selected.length
      || selected.slice(hunk.start, hunk.end) !== hunk.before) {
      throw new Error('Proposal is stale or not anchored to this selection');
    }
    updated += selected.slice(cursor, hunk.start);
    updated += accepted.has(hunk.id) ? hunk.after : hunk.before;
    cursor = hunk.end;
  }
  return fullText.slice(0, startOffset) + updated + selected.slice(cursor) + fullText.slice(endOffset);
}
