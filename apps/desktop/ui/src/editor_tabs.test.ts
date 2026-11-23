import { describe, expect, it } from 'vitest';
import { selectionAfterClose } from './editor_tabs';

describe('editor tab selection', () => {
  it('selects the next tab, then the previous one at the end', () => {
    expect(selectionAfterClose(['a', 'b', 'c'], 'b', 'b')).toBe('c');
    expect(selectionAfterClose(['a', 'b', 'c'], 'c', 'c')).toBe('b');
  });
  it('keeps the active buffer when a background tab closes', () => {
    expect(selectionAfterClose(['a', 'b', 'c'], 'b', 'a')).toBe('b');
    expect(selectionAfterClose(['a', 'b'], 'b', 'missing')).toBe('b');
  });
  it('returns to the welcome screen after the last tab closes', () => {
    expect(selectionAfterClose(['a'], 'a', 'a')).toBeNull();
    expect(selectionAfterClose([], null, 'a')).toBeNull();
  });
});
