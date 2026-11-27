import { describe, expect, it } from 'vitest';
import { isToastWarning } from './toasts';

describe('toasts warning classification', () => {
  it('does not classify file paths containing error as error warnings', () => {
    expect(isToastWarning('crates/ryoko-core/src/error.rs')).toBe(false);
    expect(isToastWarning('src/errors.ts')).toBe(false);
    expect(isToastWarning('error_handler.py')).toBe(false);
    expect(isToastWarning('C:\\repo\\error.rs')).toBe(false);
  });

  it('classifies actual error messages as error warnings', () => {
    expect(isToastWarning('Save failed: disk full')).toBe(true);
    expect(isToastWarning('Error: file not found')).toBe(true);
    expect(isToastWarning('cannot open workspace: permission denied')).toBe(true);
    expect(isToastWarning('Required binary is missing')).toBe(true);
  });

  it('treats routine status updates as normal status', () => {
    expect(isToastWarning('Ready')).toBe(false);
    expect(isToastWarning('Opening src/main.rs…')).toBe(false);
    expect(isToastWarning('Saved src/main.rs')).toBe(false);
    expect(isToastWarning('Layout reset')).toBe(false);
  });
});
