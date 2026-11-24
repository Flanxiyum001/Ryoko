import { describe, expect, it } from 'vitest';
import {
  KEYMAP_STORAGE_KEY,
  keymapById,
  loadKeymap,
  saveKeymap,
  shouldDeferToEditor,
  type KeymapPreset,
} from './keymap_preset';

function storage(initial: Record<string, string> = {}) {
  const values = new Map(Object.entries(initial));
  return {
    getItem: (key: string) => values.get(key) ?? null,
    setItem: (key: string, value: string) => { values.set(key, value); },
  };
}

describe('keymap storage', () => {
  it('defaults to the standard keymap and persists a valid preset', () => {
    const area = storage();
    expect(loadKeymap(area)).toBe('default');
    saveKeymap(area, 'vim');
    expect(area.getItem(KEYMAP_STORAGE_KEY)).toBe('vim');
    expect(loadKeymap(area)).toBe('vim');
  });

  it('ignores an unknown stored value', () => {
    expect(loadKeymap(storage({ [KEYMAP_STORAGE_KEY]: 'sublime' }))).toBe('default');
    expect(keymapById('emacs').id).toBe('emacs');
  });
});

describe('modal editor ownership', () => {
  const cases: Array<[KeymapPreset, string, boolean, boolean]> = [
    ['default', 'MOD+B', true, false],
    ['vim', 'MOD+B', true, true],
    ['emacs', 'Mod+P', true, true],
    ['vim', 'Mod+S', true, false],
    ['emacs', 'Mod+Shift+P', true, false],
    ['vim', 'Mod+B', false, false],
  ];

  it.each(cases)('%s defers %s in editor=%s', (preset, chord, inEditor, expected) => {
    expect(shouldDeferToEditor(preset, chord, inEditor)).toBe(expected);
  });
});
