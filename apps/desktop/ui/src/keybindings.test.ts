import { describe, expect, it } from 'vitest';
import {
  chordForEvent,
  loadOverrides,
  normalizeChord,
  resolveKeybindings,
  saveOverrides,
} from './keybindings';

describe('normalizeChord', () => {
  it('canonicalizes modifiers and keys', () => {
    expect(normalizeChord('ctrl+shift+f')).toBe('MOD+SHIFT+F');
    expect(normalizeChord('Mod+K')).toBe('MOD+K');
    expect(normalizeChord('mod + ,')).toBe('MOD+,');
  });
});

describe('resolveKeybindings', () => {
  it('defaults every action and indexes by chord', () => {
    const resolved = resolveKeybindings();
    expect(resolved.byAction.get('editor.inlineEdit')).toBe('MOD+K');
    expect(resolved.byChord.get('MOD+K')).toBe('editor.inlineEdit');
  });

  it('applies known overrides only', () => {
    const resolved = resolveKeybindings({ 'editor.inlineEdit': 'ctrl+i', unknown: 'x' });
    expect(resolved.byAction.get('editor.inlineEdit')).toBe('MOD+I');
    expect(resolved.byAction.has('unknown')).toBe(false);
  });
});

describe('chordForEvent', () => {
  const base = { ctrlKey: false, shiftKey: false, altKey: false, metaKey: false, key: '', code: '' };

  it('maps control and meta to MOD', () => {
    expect(chordForEvent({ ...base, ctrlKey: true, key: 'k', code: 'KeyK' })).toBe('MOD+K');
    expect(chordForEvent({ ...base, metaKey: true, key: 'f', code: 'KeyF' })).toBe('MOD+F');
  });

  it('normalizes punctuation via KeyboardEvent.code', () => {
    expect(chordForEvent({ ...base, metaKey: true, key: ',', code: 'Comma' })).toBe('MOD+,');
  });

  it('distinguishes shifted letters', () => {
    expect(chordForEvent({ ...base, ctrlKey: true, shiftKey: true, key: 'F', code: 'KeyF' }))
      .toBe('MOD+SHIFT+F');
  });
});

describe('override storage', () => {
  it('round-trips through a Storage stub and survives corrupt data', () => {
    const backing = new Map<string, string>();
    const storage = {
      getItem: (key: string) => backing.get(key) ?? null,
      setItem: (key: string, value: string) => void backing.set(key, value),
    };
    expect(loadOverrides(storage)).toEqual({});
    saveOverrides(storage, { 'editor.inlineEdit': 'ctrl+i', junk: '  ' });
    expect(loadOverrides(storage)).toEqual({ 'editor.inlineEdit': 'MOD+I' });
    backing.set('ryoko.keybindings.v1', '{broken');
    expect(loadOverrides(storage)).toEqual({});
  });
});
