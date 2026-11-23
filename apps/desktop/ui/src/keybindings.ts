//! Keybinding customization: a small defaults table, localStorage overrides,
//! and chord parsing/matching. The settings UI writes overrides; every
//! consumer goes through `resolveKeybindings` and `matchChord`.

export const KEYBINDINGS_STORAGE = 'ryoko.keybindings.v1';

export interface Keybinding {
  action: string;
  chord: string;
  title: string;
}

export const DEFAULT_KEYBINDINGS: readonly Keybinding[] = [
  { action: 'workbench.quickOpen', chord: 'MOD+P', title: 'Quick open file' },
  { action: 'workbench.search', chord: 'MOD+SHIFT+F', title: 'Search workspace' },
  { action: 'workbench.replace', chord: 'MOD+SHIFT+R', title: 'Search and replace' },
  { action: 'workbench.changes', chord: 'MOD+SHIFT+G', title: 'Toggle change drawer' },
  { action: 'workbench.debug', chord: 'MOD+SHIFT+D', title: 'Toggle debug panel' },
  { action: 'workbench.settings', chord: 'MOD+,', title: 'Open settings' },
  { action: 'editor.inlineEdit', chord: 'MOD+K', title: 'Inline edit with the agent' },
  { action: 'editor.save', chord: 'MOD+S', title: 'Save the active buffer' },
];

const MAX_OVERRIDES = 32;

export function normalizeChord(chord: string): string {
  const parts = chord
    .trim()
    .split('+')
    .map((part) => part.trim())
    .filter((part) => part.length > 0);
  const modifiers = parts
    .slice(0, -1)
    .map((part) => part.toLowerCase())
    .map((part) => (part === 'ctrl' ? 'mod' : part))
    .filter((part) => part === 'mod' || part === 'shift' || part === 'alt')
    .sort()
    .map((part) => part.toUpperCase());
  const key = parts[parts.length - 1]?.toUpperCase() ?? '';
  return [...modifiers, key].join('+');
}

export interface ResolvedKeybindings {
  byAction: Map<string, string>;
  byChord: Map<string, string>;
}

export function resolveKeybindings(
  overrides: Record<string, string> = {},
): ResolvedKeybindings {
  const byAction = new Map<string, string>();
  for (const binding of DEFAULT_KEYBINDINGS) byAction.set(binding.action, binding.chord);
  for (const [action, chord] of Object.entries(overrides).slice(0, MAX_OVERRIDES)) {
    if (byAction.has(action) && typeof chord === 'string' && chord.trim().length > 0) {
      byAction.set(action, normalizeChord(chord));
    }
  }
  const byChord = new Map<string, string>();
  for (const [action, chord] of byAction) {
    if (!byChord.has(chord)) byChord.set(chord, action);
  }
  return { byAction, byChord };
}

export interface KeyEventLike {
  ctrlKey: boolean;
  shiftKey: boolean;
  altKey: boolean;
  metaKey: boolean;
  key: string;
  code: string;
}

export function chordForEvent(event: KeyEventLike): string {
  const parts: string[] = [];
  const mod = event.metaKey || event.ctrlKey;
  if (mod) parts.push('MOD');
  if (event.shiftKey) parts.push('SHIFT');
  if (event.altKey) parts.push('ALT');
  parts.push(keyPart(event));
  return parts.join('+');
}

function keyPart(event: KeyEventLike): string {
  if (/^[a-zA-Z]$/.test(event.key)) return event.key.toUpperCase();
  if (/^[0-9]$/.test(event.key)) return event.key;
  if (event.code === 'Comma') return ',';
  if (event.code === 'Period') return '.';
  if (event.code === 'Slash') return '/';
  return event.key.toUpperCase();
}

export interface SettingStorage {
  getItem(key: string): string | null;
  setItem(key: string, value: string): void;
}

export function loadOverrides(storage: SettingStorage): Record<string, string> {
  try {
    const raw = storage.getItem(KEYBINDINGS_STORAGE);
    if (!raw) return {};
    const parsed: unknown = JSON.parse(raw);
    if (typeof parsed !== 'object' || parsed === null) return {};
    const overrides: Record<string, string> = {};
    for (const [action, chord] of Object.entries(parsed as Record<string, unknown>)) {
      if (typeof chord === 'string' && chord.trim().length > 0) overrides[action] = chord;
    }
    return overrides;
  } catch {
    return {};
  }
}

export function saveOverrides(storage: SettingStorage, overrides: Record<string, string>): void {
  const cleaned: Record<string, string> = {};
  for (const [action, chord] of Object.entries(overrides).slice(0, MAX_OVERRIDES)) {
    if (typeof chord === 'string' && chord.trim().length > 0) {
      cleaned[action] = normalizeChord(chord);
    }
  }
  storage.setItem(KEYBINDINGS_STORAGE, JSON.stringify(cleaned));
}
