//! Command palette (Mod+Shift+P): a keyboard-first fuzzy list over every
//! registered workbench command, with its shortcut shown beside it.
//! Reuses the quick-open dialog styling so both pickers look the same.

import { commandLabel, rankCommands, type CommandMatch, type CommandRegistry } from './commands';
import { nextQuickOpenSelection } from './quick_open_model';

const RECENT_KEY = 'ryoko.commandPalette.recent.v1';
const MAX_RECENT = 12;

export interface CommandPaletteDeps {
  registry: CommandRegistry;
  /** Display form of a command's shortcut, or null when it has none. */
  shortcutFor: (id: string) => string | null;
  setStatus: (message: string) => void;
}

export interface CommandPaletteController {
  /** Opens the palette, optionally pre-filled (e.g. `Theme: ` to pick a theme). */
  open: (prefill?: string) => void;
  close: () => void;
}

function readRecent(): string[] {
  try {
    const parsed: unknown = JSON.parse(localStorage.getItem(RECENT_KEY) ?? '[]');
    return Array.isArray(parsed)
      ? parsed.filter((value): value is string => typeof value === 'string' && value.length <= 120).slice(0, MAX_RECENT)
      : [];
  } catch {
    return [];
  }
}

function writeRecent(ids: string[]): void {
  try {
    localStorage.setItem(RECENT_KEY, JSON.stringify(ids.slice(0, MAX_RECENT)));
  } catch {
    // Storage is optional; the palette still works without history.
  }
}

function highlighted(label: string, positions: readonly number[]): DocumentFragment {
  const fragment = document.createDocumentFragment();
  const marks = new Set(positions);
  let run = '';
  let runMarked = false;
  const flush = (): void => {
    if (!run) return;
    if (runMarked) {
      const mark = document.createElement('mark');
      mark.textContent = run;
      fragment.append(mark);
    } else {
      fragment.append(document.createTextNode(run));
    }
    run = '';
  };
  for (let index = 0; index < label.length; index += 1) {
    const marked = marks.has(index);
    if (marked !== runMarked) { flush(); runMarked = marked; }
    run += label[index];
  }
  flush();
  return fragment;
}

export function mountCommandPalette(deps: CommandPaletteDeps): CommandPaletteController {
  const backdrop = document.createElement('div');
  backdrop.className = 'quick-open-backdrop command-palette';
  backdrop.hidden = true;

  const dialog = document.createElement('section');
  dialog.className = 'quick-open-dialog';
  dialog.setAttribute('role', 'dialog');
  dialog.setAttribute('aria-modal', 'true');
  dialog.setAttribute('aria-label', 'Command palette');

  const input = document.createElement('input');
  input.type = 'search';
  input.className = 'quick-open-input';
  input.placeholder = 'Type a command…';
  input.autocomplete = 'off';
  input.spellcheck = false;
  input.maxLength = 120;
  input.setAttribute('aria-label', 'Search commands');
  input.setAttribute('role', 'combobox');
  input.setAttribute('aria-autocomplete', 'list');
  input.setAttribute('aria-expanded', 'true');

  const list = document.createElement('div');
  list.className = 'quick-open-results';
  list.id = 'ryoko-command-palette-results';
  list.setAttribute('role', 'listbox');
  list.setAttribute('aria-label', 'Matching commands');
  input.setAttribute('aria-controls', list.id);

  const footer = document.createElement('p');
  footer.className = 'quick-open-footer';
  footer.textContent = '↑ ↓ Navigate · Enter Run · Esc Close';
  dialog.append(input, list, footer);
  backdrop.append(dialog);
  document.body.append(backdrop);

  let matches: CommandMatch[] = [];
  let selected = 0;
  let previousFocus: HTMLElement | null = null;

  const close = (): void => {
    if (backdrop.hidden) return;
    backdrop.hidden = true;
    input.value = '';
    input.removeAttribute('aria-activedescendant');
    previousFocus?.focus();
    previousFocus = null;
  };

  const runSelected = (): void => {
    const match = matches[selected];
    if (!match) return;
    const id = match.command.id;
    if (!deps.registry.isEnabled(id)) {
      deps.setStatus(`${commandLabel(match.command)} is not available right now.`);
      return;
    }
    close();
    writeRecent([id, ...readRecent().filter((other) => other !== id)]);
    void deps.registry.execute(id).catch((error: unknown) => {
      deps.setStatus(`${commandLabel(match.command)} failed: ${String(error)}`);
    });
  };

  const updateSelection = (): void => {
    Array.from(list.querySelectorAll<HTMLButtonElement>('.quick-open-option')).forEach((option, index) => {
      option.setAttribute('aria-selected', String(index === selected));
      if (index === selected) {
        input.setAttribute('aria-activedescendant', option.id);
        option.scrollIntoView({ block: 'nearest' });
      }
    });
  };

  const render = (): void => {
    matches = rankCommands(deps.registry.all(), input.value, readRecent());
    selected = matches.length ? Math.min(Math.max(selected, 0), matches.length - 1) : -1;
    list.replaceChildren();
    if (!matches.length) {
      const empty = document.createElement('p');
      empty.className = 'quick-open-empty';
      empty.textContent = 'No matching commands.';
      list.append(empty);
      input.removeAttribute('aria-activedescendant');
      return;
    }
    const fragment = document.createDocumentFragment();
    matches.forEach((match, index) => {
      const option = document.createElement('button');
      option.type = 'button';
      option.id = `ryoko-command-option-${index}`;
      option.className = 'quick-open-option command-option';
      option.setAttribute('role', 'option');
      option.dataset.command = match.command.id;
      const enabled = deps.registry.isEnabled(match.command.id);
      if (!enabled) option.setAttribute('aria-disabled', 'true');
      const label = document.createElement('span');
      label.className = 'quick-open-name';
      label.append(highlighted(commandLabel(match.command), match.positions));
      option.append(label);
      const shortcut = deps.shortcutFor(match.command.id);
      if (shortcut) {
        const kbd = document.createElement('kbd');
        kbd.className = 'command-shortcut';
        kbd.textContent = shortcut;
        option.append(kbd);
      }
      option.addEventListener('mouseenter', () => { selected = index; updateSelection(); });
      option.addEventListener('click', () => { selected = index; runSelected(); });
      fragment.append(option);
    });
    list.append(fragment);
    updateSelection();
  };

  const open = (prefill = ''): void => {
    if (!backdrop.hidden) { input.value = prefill; render(); input.focus(); return; }
    previousFocus = document.activeElement instanceof HTMLElement ? document.activeElement : null;
    selected = 0;
    input.value = prefill.slice(0, input.maxLength);
    backdrop.hidden = false;
    render();
    input.focus();
  };

  input.addEventListener('input', () => { selected = 0; render(); });
  input.addEventListener('keydown', (event) => {
    event.stopPropagation();
    if (event.key === 'Escape') {
      event.preventDefault();
      close();
    } else if (event.key === 'ArrowDown' || event.key === 'ArrowUp') {
      event.preventDefault();
      selected = nextQuickOpenSelection(selected, matches.length, event.key === 'ArrowDown' ? 1 : -1);
      updateSelection();
    } else if (event.key === 'Enter') {
      event.preventDefault();
      runSelected();
    }
  });
  backdrop.addEventListener('mousedown', (event) => {
    if (event.target === backdrop) close();
  });

  return { open, close };
}
