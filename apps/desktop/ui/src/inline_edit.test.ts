import { describe, expect, it } from 'vitest';
import {
  applyReplacement,
  replacementIsCurrent,
  splitContext,
} from './inline_edit';

const TEXT = '_alpha_ beta gamma';

describe('splitContext', () => {
  it('returns the selection with surrounding context', () => {
    const context = splitContext(TEXT, 8, 4);
    expect(context.selected).toBe('beta');
    expect(context.before).toBe('_alpha_ ');
    expect(context.after).toBe(' gamma');
    expect(context.selectionEmpty).toBe(false);
  });

  it('marks empty selections as cursor insertions', () => {
    const context = splitContext(TEXT, 4, 0);
    expect(context.selectionEmpty).toBe(true);
    expect(context.selected).toBe('');
  });

  it('ellipsizes oversized context while keeping the selection verbatim', () => {
    const long = 'x'.repeat(20_000);
    const context = splitContext(`${long}|SELECTED|${long}`, 20_000, 10);
    expect(context.selected).toBe('|SELECTED|');
    expect(context.before.startsWith('\u2026')).toBe(true);
    expect(context.before.length).toBeLessThanOrEqual(3001);
    expect(context.after.endsWith('\u2026')).toBe(true);
    expect(context.after.length).toBeLessThanOrEqual(6001);
  });

  it('clamps offsets outside the buffer', () => {
    const context = splitContext(TEXT, 99, 50);
    expect(context.selected).toBe('');
    expect(context.selectionEmpty).toBe(true);
  });
});

describe('replacementIsCurrent', () => {
  it('only accepts unchanged buffers', () => {
    expect(replacementIsCurrent('abc', 'abc')).toBe(true);
    expect(replacementIsCurrent('abc', 'abd')).toBe(false);
  });
});

describe('applyReplacement', () => {
  it('splices the replacement over the captured span', () => {
    expect(applyReplacement(TEXT, 8, 4, 'delta')).toBe('_alpha_ delta gamma');
    expect(applyReplacement(TEXT, 0, 0, '#')).toBe('#_alpha_ beta gamma');
    expect(applyReplacement(TEXT, 99, 4, 'x')).toBe(TEXT + 'x');
  });
});
