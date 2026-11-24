import { describe, expect, it } from 'vitest';
import {
  IMPORTED_SETTINGS_STORAGE,
  IMPORTED_THEME_STORAGE,
  loadImportedSettings,
  loadImportedTheme,
  monacoOptionsFor,
  parseVscodeSettings,
  parseVscodeTheme,
  saveImportedSettings,
  saveImportedTheme,
  stripJsonComments,
} from './vscode_import';

function storage(initial: Record<string, string> = {}) {
  const values = new Map(Object.entries(initial));
  return {
    getItem: (key: string) => values.get(key) ?? null,
    setItem: (key: string, value: string) => { values.set(key, value); },
  };
}

describe('VS Code theme import', () => {
  it('maps supported colors and token rules', () => {
    const theme = parseVscodeTheme(JSON.stringify({
      name: 'Midnight',
      type: 'dark',
      colors: {
        'editor.background': '#101820',
        'editor.foreground': '#f0f0f0',
        'sideBar.background': '#151515',
        'editor.selectionBackground': '#264f78',
        'not.supported': '#ffffff',
      },
      tokenColors: [
        { scope: ['keyword', 'storage'], settings: { foreground: '#ff79c6', fontStyle: 'italic' } },
        { scope: 'comment', settings: { foreground: '#6272a4' } },
      ],
    }));
    expect(theme.name).toBe('Midnight');
    expect(theme.monacoTheme.base).toBe('vs-dark');
    expect(theme.monacoTheme.colors?.['editor.background']).toBe('#101820');
    expect(theme.chrome['--editor-bg']).toBe('#101820');
    expect(theme.chrome['--chrome-bg']).toBe('#151515');
    expect(theme.monacoTheme.rules[0].token).toBe('keyword,storage');
    expect(theme.monacoTheme.rules[0].foreground).toBe('ff79c6');
  });

  it('rejects non-hex colors and oversized files', () => {
    const theme = parseVscodeTheme(JSON.stringify({
      colors: { 'editor.background': 'url(evil)' },
    }));
    expect(theme.warnings).toContain('No supported VS Code colors were found.');
    expect(() => parseVscodeTheme('x'.repeat(512 * 1024 + 1))).toThrow(/512 KiB/);
  });

  it('persists and reloads a theme', () => {
    const area = storage();
    const theme = parseVscodeTheme('{"name":"T","colors":{"editor.background":"#000000"}}');
    saveImportedTheme(area, theme);
    expect(area.getItem(IMPORTED_THEME_STORAGE)).toBeTruthy();
    expect(loadImportedTheme(area)?.name).toBe('T');
  });
});

describe('VS Code settings import', () => {
  it('accepts comments and trailing commas', () => {
    expect(stripJsonComments('{\n // comment\n "a": 1,\n}')).toBe('{\n \n "a": 1\n}');
    expect(stripJsonComments('{"a":"x,}"}')).toBe('{"a":"x,}"}');
    const parsed = parseVscodeSettings(`{
      // editor
      "editor.fontSize": 15,
      "editor.tabSize": 4,
      "editor.insertSpaces": false,
      "editor.wordWrap": "on",
      "editor.minimap.enabled": false,
    }`);
    expect(parsed.settings).toEqual({
      fontSize: 15,
      tabSize: 4,
      insertSpaces: false,
      wordWrap: 'on',
      lineHeight: undefined,
      minimap: false,
      formatOnSave: undefined,
    });
    expect(parsed.warnings).toEqual([]);
    expect(monacoOptionsFor(parsed.settings).fontSize).toBe(15);
  });

  it('bounds values and reports ignored keys', () => {
    const parsed = parseVscodeSettings(JSON.stringify({
      'editor.fontSize': 400,
      'editor.tabSize': 'four',
      'editor.wordWrap': 'sometimes',
    }));
    expect(parsed.settings.fontSize).toBeUndefined();
    expect(parsed.settings.tabSize).toBeUndefined();
    expect(parsed.settings.wordWrap).toBeUndefined();
    expect(parsed.warnings).toHaveLength(3);
  });

  it('persists and reloads settings', () => {
    const area = storage();
    saveImportedSettings(area, { fontSize: 14 });
    expect(area.getItem(IMPORTED_SETTINGS_STORAGE)).toBeTruthy();
    expect(loadImportedSettings(area)).toEqual({ fontSize: 14 });
  });
});
