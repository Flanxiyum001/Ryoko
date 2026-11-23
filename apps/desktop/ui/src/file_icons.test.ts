import { describe, expect, it } from 'vitest';
import { DEFAULT_FILE_ICON, iconForPath } from './file_icons';

describe('iconForPath', () => {
  it('uses extensions case-insensitively', () => {
    expect(iconForPath('src/ui/main.ts').label).toBe('TypeScript');
    expect(iconForPath('LIB.RS').label).toBe('Rust');
  });
  it('prefers well-known file names over extensions', () => {
    expect(iconForPath('crates/x/Cargo.toml').label).toBe('Cargo manifest');
    expect(iconForPath('README.md').label).toBe('Readme');
  });
  it('falls back for unknown, extensionless and dotfiles', () => {
    expect(iconForPath('bin/tool')).toBe(DEFAULT_FILE_ICON);
    expect(iconForPath('notes.unknownext')).toBe(DEFAULT_FILE_ICON);
    expect(iconForPath('.env')).toBe(DEFAULT_FILE_ICON);
  });
});
