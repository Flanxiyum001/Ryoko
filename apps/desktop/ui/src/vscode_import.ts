/** VS Code theme and settings import.
 *
 * Imported files are untrusted text. Only a small, typed subset is accepted:
 * hex colors, bounded token rules and a fixed list of editor settings. Nothing
 * is written outside the local workbench and nothing is uploaded.
 */
import type * as monaco from 'monaco-editor';

const MAX_FILE_CHARS = 512 * 1024;
const MAX_RULES = 500;
const MAX_SCOPES = 20;
const MAX_SCOPE_CHARS = 200;
const HEX_COLOR = /^#[0-9a-fA-F]{3,8}$/;

export const IMPORTED_THEME_STORAGE = 'ryoko.vscode.theme.v1';
export const IMPORTED_SETTINGS_STORAGE = 'ryoko.vscode.settings.v1';

export interface ImportedTheme {
  name: string;
  monacoTheme: monaco.editor.IStandaloneThemeData;
  /** Workbench CSS variables, keyed by variable name. */
  chrome: Record<string, string>;
  warnings: string[];
}

export interface ImportedEditorSettings {
  fontSize?: number;
  tabSize?: number;
  insertSpaces?: boolean;
  wordWrap?: 'on' | 'off' | 'wordWrapColumn' | 'bounded';
  lineHeight?: number;
  minimap?: boolean;
  formatOnSave?: boolean;
}

export interface ParsedSettings {
  settings: ImportedEditorSettings;
  warnings: string[];
}

const COLOR_MAP: Record<string, { chrome?: string[]; monaco?: string }> = {
  'editor.background': { chrome: ['--editor-bg', '--bg'], monaco: 'editor.background' },
  'editor.foreground': { chrome: ['--text'], monaco: 'editor.foreground' },
  'editor.selectionBackground': { chrome: ['--selection-bg'], monaco: 'editor.selectionBackground' },
  'editor.lineHighlightBackground': { monaco: 'editor.lineHighlightBackground' },
  'editorLineNumber.foreground': { monaco: 'editorLineNumber.foreground' },
  'editorCursor.foreground': { monaco: 'editorCursor.foreground' },
  'editorWidget.background': { monaco: 'editorWidget.background' },
  'editorWidget.border': { monaco: 'editorWidget.border' },
  'sideBar.background': { chrome: ['--chrome-bg', '--panel'] },
  'sideBar.foreground': { chrome: ['--text'] },
  'activityBar.background': { chrome: ['--panel-raised'] },
  'statusBar.background': { chrome: ['--panel'] },
  'titleBar.activeBackground': { chrome: ['--panel'] },
  'menu.background': { chrome: ['--menu-bg'] },
  'menu.foreground': { chrome: ['--text'] },
  'input.background': { chrome: ['--input-bg'] },
  'input.border': { chrome: ['--input-border'] },
  'focusBorder': { chrome: ['--accent'] },
  'button.background': { chrome: ['--accent'] },
  'list.hoverBackground': { chrome: ['--hover-bg'] },
  'terminal.background': { chrome: ['--panel'] },
};

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === 'object' && value !== null && !Array.isArray(value);
}

function boundedString(value: unknown, max: number): string | null {
  return typeof value === 'string' && value.length <= max ? value : null;
}

function color(value: unknown): string | null {
  return typeof value === 'string' && HEX_COLOR.test(value) ? value : null;
}

function baseFor(colorValue: string | undefined, type: unknown): monaco.editor.IStandaloneThemeData['base'] {
  if (type === 'light') return 'vs';
  if (type === 'hc' || type === 'hc-black') return 'hc-black';
  if (type === 'dark') return 'vs-dark';
  if (!colorValue) return 'vs-dark';
  const hex = colorValue.slice(1);
  const normalized = hex.length === 3
    ? hex.split('').map((part) => part + part).join('')
    : hex.slice(0, 6);
  const red = parseInt(normalized.slice(0, 2), 16);
  const green = parseInt(normalized.slice(2, 4), 16);
  const blue = parseInt(normalized.slice(4, 6), 16);
  return red + green + blue > 3 * 180 ? 'vs' : 'vs-dark';
}

function tokenRule(rule: unknown, warnings: string[]): monaco.editor.ITokenThemeRule | null {
  if (!isRecord(rule) || !isRecord(rule.settings)) {
    warnings.push('Skipped a token rule without settings.');
    return null;
  }
  const foreground = rule.settings.foreground === undefined
    ? undefined
    : color(rule.settings.foreground);
  if (rule.settings.foreground !== undefined && !foreground) {
    warnings.push('Skipped a token rule with a non-hex foreground.');
    return null;
  }
  const fontStyle = rule.settings.fontStyle === undefined
    ? undefined
    : boundedString(rule.settings.fontStyle, 80);
  if (fontStyle && !/^[a-z ]+$/.test(fontStyle)) {
    warnings.push('Skipped a token rule with an unsupported font style.');
    return null;
  }
  const rawScopes = Array.isArray(rule.scope) ? rule.scope : [rule.scope];
  const scopes = rawScopes
    .map((scope) => boundedString(scope, MAX_SCOPE_CHARS))
    .filter((scope): scope is string => Boolean(scope))
    .slice(0, MAX_SCOPES);
  if (scopes.length === 0) return null;
  return {
    token: scopes.join(','),
    ...(foreground ? { foreground: foreground.slice(1) } : {}),
    ...(fontStyle ? { fontStyle } : {}),
  };
}

export function parseVscodeTheme(text: string): ImportedTheme {
  if (text.length > MAX_FILE_CHARS) {
    throw new Error('theme JSON exceeds the 512 KiB import limit');
  }
  let parsed: unknown;
  try {
    parsed = JSON.parse(text);
  } catch (error) {
    throw new Error('theme is not valid JSON: ' + (error instanceof Error ? error.message : String(error)));
  }
  if (!isRecord(parsed)) throw new Error('theme JSON must be an object');
  const warnings: string[] = [];
  const name = boundedString(parsed.name, 120) ?? 'Imported VS Code theme';
  const colors: Record<string, string> = {};
  const chrome: Record<string, string> = {};
  if (isRecord(parsed.colors)) {
    for (const [key, value] of Object.entries(parsed.colors)) {
      const mapped = COLOR_MAP[key];
      const valid = color(value);
      if (!mapped || !valid) continue;
      if (mapped.monaco) colors[mapped.monaco] = valid;
      for (const variable of mapped.chrome ?? []) chrome[variable] = valid;
    }
  }
  if (Object.keys(colors).length === 0 && Object.keys(chrome).length === 0) {
    warnings.push('No supported VS Code colors were found.');
  }
  const rules: monaco.editor.ITokenThemeRule[] = [];
  if (Array.isArray(parsed.tokenColors)) {
    if (parsed.tokenColors.length > MAX_RULES) {
      warnings.push(`Only the first ${MAX_RULES} token rules were imported.`);
    }
    for (const rule of parsed.tokenColors.slice(0, MAX_RULES)) {
      const converted = tokenRule(rule, warnings);
      if (converted) rules.push(converted);
    }
  }
  return {
    name,
    monacoTheme: {
      base: baseFor(colors['editor.background'], parsed.type),
      inherit: true,
      rules,
      colors,
    },
    chrome,
    warnings,
  };
}

/** VS Code settings files allow // and block comments plus trailing commas. */
export function stripJsonComments(text: string): string {
  let output = '';
  let inString = false;
  let escaped = false;
  for (let index = 0; index < text.length; index += 1) {
    const character = text[index];
    const next = text[index + 1];
    if (inString) {
      output += character;
      if (escaped) escaped = false;
      else if (character === '\\') escaped = true;
      else if (character === '"') inString = false;
      continue;
    }
    if (character === '"') {
      inString = true;
      output += character;
    } else if (character === '/' && next === '/') {
      while (index < text.length && text[index] !== '\n') index += 1;
      output += '\n';
    } else if (character === '/' && next === '*') {
      index += 2;
      while (index < text.length && !(text[index] === '*' && text[index + 1] === '/')) index += 1;
      index += 1;
    } else {
      output += character;
    }
  }
  return stripTrailingCommas(output);
}

function stripTrailingCommas(text: string): string {
  let output = '';
  let inString = false;
  let escaped = false;
  for (let index = 0; index < text.length; index += 1) {
    const character = text[index];
    if (inString) {
      output += character;
      if (escaped) escaped = false;
      else if (character === '\\') escaped = true;
      else if (character === '"') inString = false;
      continue;
    }
    if (character === '"') {
      inString = true;
      output += character;
      continue;
    }
    if (character === ',') {
      let next = index + 1;
      while (next < text.length && /\s/.test(text[next])) next += 1;
      if (text[next] === '}' || text[next] === ']') continue;
    }
    output += character;
  }
  return output;
}

export function parseVscodeSettings(text: string): ParsedSettings {
  if (text.length > MAX_FILE_CHARS) {
    throw new Error('settings JSON exceeds the 512 KiB import limit');
  }
  let parsed: unknown;
  try {
    parsed = JSON.parse(stripJsonComments(text));
  } catch (error) {
    throw new Error('settings are not valid JSON: ' + (error instanceof Error ? error.message : String(error)));
  }
  if (!isRecord(parsed)) throw new Error('settings JSON must be an object');
  const settings: ImportedEditorSettings = {};
  const warnings: string[] = [];
  const number = (key: string, min: number, max: number): number | undefined => {
    const value = parsed[key];
    if (value === undefined) return undefined;
    if (typeof value !== 'number' || !Number.isFinite(value) || value < min || value > max) {
      warnings.push(`Ignored ${key}; expected a number from ${min} to ${max}.`);
      return undefined;
    }
    return value;
  };
  const boolean = (key: string): boolean | undefined => {
    const value = parsed[key];
    if (value === undefined) return undefined;
    if (typeof value !== 'boolean') {
      warnings.push(`Ignored ${key}; expected true or false.`);
      return undefined;
    }
    return value;
  };
  settings.fontSize = number('editor.fontSize', 8, 32);
  settings.tabSize = number('editor.tabSize', 1, 8);
  settings.lineHeight = number('editor.lineHeight', 0, 48);
  settings.insertSpaces = boolean('editor.insertSpaces');
  settings.minimap = boolean('editor.minimap.enabled');
  settings.formatOnSave = boolean('editor.formatOnSave');
  const wrap = parsed['editor.wordWrap'];
  if (wrap !== undefined) {
    if (wrap === 'on' || wrap === 'off' || wrap === 'wordWrapColumn' || wrap === 'bounded') {
      settings.wordWrap = wrap;
    } else {
      warnings.push('Ignored editor.wordWrap; unsupported value.');
    }
  }
  return { settings, warnings };
}

export function monacoOptionsFor(settings: ImportedEditorSettings): monaco.editor.IEditorOptions {
  return {
    ...(settings.fontSize !== undefined ? { fontSize: settings.fontSize } : {}),
    ...(settings.tabSize !== undefined ? { tabSize: settings.tabSize } : {}),
    ...(settings.insertSpaces !== undefined ? { insertSpaces: settings.insertSpaces } : {}),
    ...(settings.wordWrap !== undefined ? { wordWrap: settings.wordWrap } : {}),
    ...(settings.lineHeight !== undefined ? { lineHeight: settings.lineHeight } : {}),
    ...(settings.minimap !== undefined ? { minimap: { enabled: settings.minimap } } : {}),
  };
}

export function loadImportedTheme(storage: Pick<Storage, 'getItem'>): ImportedTheme | null {
  try {
    const raw = storage.getItem(IMPORTED_THEME_STORAGE);
    if (!raw) return null;
    const parsed = JSON.parse(raw) as ImportedTheme;
    if (!parsed || typeof parsed !== 'object' || !parsed.monacoTheme) return null;
    return parsed;
  } catch {
    return null;
  }
}

export function saveImportedTheme(storage: Pick<Storage, 'setItem'>, theme: ImportedTheme): void {
  try {
    storage.setItem(IMPORTED_THEME_STORAGE, JSON.stringify(theme));
  } catch {
    // Storage is optional.
  }
}

export function loadImportedSettings(storage: Pick<Storage, 'getItem'>): ImportedEditorSettings | null {
  try {
    const raw = storage.getItem(IMPORTED_SETTINGS_STORAGE);
    if (!raw) return null;
    const parsed = JSON.parse(raw) as ImportedEditorSettings;
    return parsed && typeof parsed === 'object' ? parsed : null;
  } catch {
    return null;
  }
}

export function saveImportedSettings(
  storage: Pick<Storage, 'setItem'>,
  settings: ImportedEditorSettings,
): void {
  try {
    storage.setItem(IMPORTED_SETTINGS_STORAGE, JSON.stringify(settings));
  } catch {
    // Storage is optional.
  }
}
