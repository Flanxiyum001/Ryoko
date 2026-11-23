import { describe, expect, it } from 'vitest';
import { THEMES, THEME_STORAGE, loadTheme, nextTheme, themeById } from './themes';

describe('themes', () => {
  it('maps every theme to a built-in Monaco theme', () => {
    expect(THEMES.map((theme) => theme.monaco)).toEqual(['vs-dark', 'vs', 'hc-black']);
  });
  it('falls back to dark for unknown stored values', () => {
    expect(themeById('neon').id).toBe('dark');
    expect(loadTheme({ getItem: () => '<script>' }).id).toBe('dark');
    expect(loadTheme({ getItem: (key) => (key === THEME_STORAGE ? 'light' : null) }).id).toBe('light');
  });
  it('cycles through themes', () => {
    expect(nextTheme('high-contrast').id).toBe('dark');
  });
});
