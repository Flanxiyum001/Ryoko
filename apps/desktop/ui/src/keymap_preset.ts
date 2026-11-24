/** Editor keymap presets. Monaco owns modal editing; the workbench keeps the
 * chords that are not single-modifier editor motions (save, palette, etc.).
 */

export type KeymapPreset = 'default' | 'vim' | 'emacs';

export const KEYMAP_STORAGE_KEY = 'ryoko.keymap.v1';

export interface KeymapOption {
  id: KeymapPreset;
  label: string;
  description: string;
}

export const KEYMAP_OPTIONS: readonly KeymapOption[] = [
  {
    id: 'default',
    label: 'Default',
    description: 'Ryoko and Monaco shortcuts.',
  },
  {
    id: 'vim',
    label: 'Vim',
    description: 'Normal, insert and visual modes in the editor.',
  },
  {
    id: 'emacs',
    label: 'Emacs',
    description: 'Emacs motion and kill-ring chords in the editor.',
  },
];

export function keymapById(id: string | null | undefined): KeymapOption {
  return KEYMAP_OPTIONS.find((option) => option.id === id) ?? KEYMAP_OPTIONS[0];
}

export function loadKeymap(storage: Pick<Storage, 'getItem'>): KeymapPreset {
  try {
    return keymapById(storage.getItem(KEYMAP_STORAGE_KEY)).id;
  } catch {
    return 'default';
  }
}

export function saveKeymap(storage: Pick<Storage, 'setItem'>, preset: KeymapPreset): void {
  try {
    storage.setItem(KEYMAP_STORAGE_KEY, preset);
  } catch {
    // A private or full storage area must not break the editor.
  }
}

/**
 * In Vim/Emacs mode, single-modifier letter chords belong to the editor
 * (`C-b` is a motion, not "toggle sidebar"). Save and multi-modifier chords
 * stay with the workbench so a file can always be saved.
 */
export function shouldDeferToEditor(
  preset: KeymapPreset,
  chord: string,
  inEditor: boolean,
): boolean {
  if (!inEditor || preset === 'default') return false;
  const normalized = chord.toUpperCase();
  if (normalized === 'MOD+S' || normalized === 'MOD+SHIFT+S') return false;
  return /^MOD\+[A-Z]$/.test(normalized);
}
