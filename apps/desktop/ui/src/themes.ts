//! Colour themes. The workbench chrome is driven by CSS variables keyed on
//! `<html data-theme>`, and each theme names the matching built-in Monaco
//! theme so the editor, diff view and chrome always change together.

export type ThemeId = 'dark' | 'light' | 'high-contrast';

export interface ThemeDefinition {
  id: ThemeId;
  label: string;
  monaco: 'vs-dark' | 'vs' | 'hc-black';
  colorScheme: 'dark' | 'light';
}

export const THEMES: readonly ThemeDefinition[] = [
  { id: 'dark', label: 'Dark', monaco: 'vs-dark', colorScheme: 'dark' },
  { id: 'light', label: 'Light', monaco: 'vs', colorScheme: 'light' },
  { id: 'high-contrast', label: 'High Contrast', monaco: 'hc-black', colorScheme: 'dark' },
];

export const THEME_STORAGE = 'ryoko.theme.v1';

export function themeById(id: unknown): ThemeDefinition {
  return THEMES.find((theme) => theme.id === id) ?? THEMES[0];
}

export function nextTheme(current: ThemeId): ThemeDefinition {
  const index = THEMES.findIndex((theme) => theme.id === current);
  return THEMES[(index + 1) % THEMES.length];
}

export function loadTheme(storage: Pick<Storage, 'getItem'>): ThemeDefinition {
  try {
    return themeById(storage.getItem(THEME_STORAGE));
  } catch {
    return THEMES[0];
  }
}

export function saveTheme(storage: Pick<Storage, 'setItem'>, id: ThemeId): void {
  try {
    storage.setItem(THEME_STORAGE, id);
  } catch {
    // Storage is optional.
  }
}

/** Applies the theme to the document; the caller updates Monaco with `theme.monaco`. */
export function applyThemeToDocument(theme: ThemeDefinition, root: HTMLElement = document.documentElement): void {
  root.dataset.theme = theme.id;
  root.style.colorScheme = theme.colorScheme;
  document.querySelector('meta[name="color-scheme"]')?.setAttribute('content', theme.colorScheme);
}
