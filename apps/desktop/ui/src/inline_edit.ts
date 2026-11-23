//! Pure logic behind the Cmd+K inline edit flow: bounded context extraction
//! from a Monaco selection and replacement application back onto the model.

export const MAX_SELECTION_CHARS = 12_000;
export const MAX_CONTEXT_CHARS = 6_000;

export interface EditorTextContext {
  before: string;
  selected: string;
  after: string;
  selectionEmpty: boolean;
}

export function splitContext(text: string, offset: number, length: number): EditorTextContext {
  const boundedOffset = Math.max(0, Math.min(text.length, offset));
  const boundedLength = Math.max(0, Math.min(text.length - boundedOffset, length));
  const selected = text.slice(boundedOffset, boundedOffset + boundedLength);
  const beforeHalf = Math.floor(MAX_CONTEXT_CHARS / 2);
  let before = text.slice(0, boundedOffset);
  if (before.length > beforeHalf) before = '\u2026' + before.slice(before.length - beforeHalf);
  const afterStart = boundedOffset + boundedLength;
  let after = text.slice(afterStart);
  if (after.length > MAX_CONTEXT_CHARS) after = after.slice(0, MAX_CONTEXT_CHARS) + '\u2026';
  return {
    before,
    selected,
    after: after.length > 0 ? after : '',
    selectionEmpty: boundedLength === 0,
  };
}

export function contextSummary(context: EditorTextContext): string {
  if (context.selected.length > 0) {
    return `${context.selected.length} selected chars`;
  }
  return 'inserting at cursor';
}

/**
 * Whether a validated model replacement may be applied. Whole-buffer
 * replacements (empty selection) are only allowed when the model has not
 * changed since the context was captured, which callers enforce by comparing
 * these digests.
 */
export function replacementIsCurrent(contextText: string, currentText: string): boolean {
  return contextText === currentText;
}

export function applyReplacement(
  text: string,
  offset: number,
  length: number,
  replacement: string,
): string {
  const boundedOffset = Math.max(0, Math.min(text.length, offset));
  const boundedLength = Math.max(0, Math.min(text.length - boundedOffset, length));
  return text.slice(0, boundedOffset) + replacement + text.slice(boundedOffset + boundedLength);
}
