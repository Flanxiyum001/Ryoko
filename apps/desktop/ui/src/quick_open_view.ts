import {
  createQuickOpenIndexCache,
  rankIndex,
  nextQuickOpenSelection,
  type QuickOpenMatch,
} from './quick_open_model';

const RECENT_PREFIX = 'ryoko.quickOpen.recent.v1:';
const MAX_RECENTS = 20;

export interface QuickOpenDeps {
  workspaceRoot: () => string;
  files: () => readonly string[];
  openFile: (path: string) => Promise<void>;
  setStatus: (message: string) => void;
}

export interface QuickOpenController {
  open: () => void;
  close: () => void;
  remember: (path: string) => void;
}

function readRecent(root: string): string[] {
  if (!root) return [];
  try {
    const parsed: unknown = JSON.parse(localStorage.getItem(RECENT_PREFIX + root) ?? '[]');
    return Array.isArray(parsed)
      ? parsed.filter((value): value is string => typeof value === 'string' && value.length <= 1024).slice(0, MAX_RECENTS)
      : [];
  } catch {
    return [];
  }
}

function writeRecent(root: string, paths: string[]): void {
  if (!root) return;
  try {
    localStorage.setItem(RECENT_PREFIX + root, JSON.stringify(paths.slice(0, MAX_RECENTS)));
  } catch {
    // Storage is optional (private browsing or quota limits): navigation still works.
  }
}

/** Creates one keyboard-accessible, non-destructive quick-open palette. */
export function mountQuickOpen(deps: QuickOpenDeps): QuickOpenController {
  const backdrop = document.createElement('div');
  backdrop.className = 'quick-open-backdrop';
  backdrop.hidden = true;

  const dialog = document.createElement('section');
  dialog.className = 'quick-open-dialog';
  dialog.setAttribute('role', 'dialog');
  dialog.setAttribute('aria-modal', 'true');
  dialog.setAttribute('aria-label', 'Quick open files');

  const input = document.createElement('input');
  input.type = 'search';
  input.className = 'quick-open-input';
  input.placeholder = 'Type a filename or path…';
  input.autocomplete = 'off';
  input.spellcheck = false;
  input.maxLength = 120;
  input.setAttribute('aria-label', 'Find a file in this workspace');
  input.setAttribute('role', 'combobox');
  input.setAttribute('aria-autocomplete', 'list');
  input.setAttribute('aria-expanded', 'true');

  const list = document.createElement('div');
  list.className = 'quick-open-results';
  list.id = 'ryoko-quick-open-results';
  list.setAttribute('role', 'listbox');
  list.setAttribute('aria-label', 'Matching files');
  input.setAttribute('aria-controls', list.id);

  const footer = document.createElement('p');
  footer.className = 'quick-open-footer';
  footer.textContent = '↑ ↓ Navigate · Enter Open · Esc Close';
  dialog.append(input, list, footer);
  backdrop.append(dialog);
  document.body.append(backdrop);

  let matches: QuickOpenMatch[] = [];
  let selected = 0;
  let previousFocus: HTMLElement | null = null;
  let recent: string[] = [];
  // One index per loaded file list. A keystroke never rebuilds it and never
  // touches the filesystem; only a new workspace file list does.
  const currentIndex = createQuickOpenIndexCache();

  const close = (): void => {
    if (backdrop.hidden) return;
    backdrop.hidden = true;
    input.value = '';
    input.removeAttribute('aria-activedescendant');
    previousFocus?.focus();
    previousFocus = null;
  };

  const remember = (path: string): void => {
    const root = deps.workspaceRoot();
    if (!root || !currentIndex(deps.files()).pathSet.has(path)) return;
    recent = [path, ...readRecent(root).filter((other) => other !== path)].slice(0, MAX_RECENTS);
    writeRecent(root, recent);
  };

  const openSelected = (): void => {
    const path = matches[selected]?.path;
    if (!path) return;
    close();
    void deps.openFile(path).catch((error: unknown) => {
      deps.setStatus('Could not open file: ' + String(error));
    });
  };

  const render = (): void => {
    matches = rankIndex(currentIndex(deps.files()), input.value, recent);
    selected = matches.length ? Math.min(selected, matches.length - 1) : -1;
    list.replaceChildren();
    if (matches.length === 0) {
      const empty = document.createElement('p');
      empty.className = 'quick-open-empty';
      empty.textContent = 'No matching files.';
      list.append(empty);
      input.removeAttribute('aria-activedescendant');
      return;
    }
    const fragment = document.createDocumentFragment();
    matches.forEach((match, index) => {
      const option = document.createElement('button');
      option.type = 'button';
      // Combobox navigation uses aria-activedescendant; options are not tab stops.
      option.tabIndex = -1;
      option.addEventListener('mousedown', (event) => event.preventDefault());
      option.id = 'ryoko-quick-open-option-' + index;
      option.className = 'quick-open-option';
      option.setAttribute('role', 'option');
      option.setAttribute('aria-selected', String(index === selected));
      const label = document.createElement('span');
      label.className = 'quick-open-name';
      label.textContent = match.path.split('/').pop() ?? match.path;
      const dirname = document.createElement('span');
      dirname.className = 'quick-open-path';
      dirname.textContent = match.path.includes('/') ? match.path.slice(0, match.path.lastIndexOf('/')) : 'Workspace root';
      option.title = match.path;
      option.append(label, dirname);
      option.addEventListener('mouseenter', () => {
        selected = index;
        updateSelection();
      });
      option.addEventListener('click', () => {
        selected = index;
        openSelected();
      });
      fragment.append(option);
    });
    list.append(fragment);
    updateSelection();
  };

  const updateSelection = (): void => {
    for (const [index, option] of Array.from(list.querySelectorAll<HTMLButtonElement>('.quick-open-option')).entries()) {
      option.setAttribute('aria-selected', String(index === selected));
      if (index === selected) {
        input.setAttribute('aria-activedescendant', option.id);
        option.scrollIntoView({ block: 'nearest' });
      }
    }
  };

  const open = (): void => {
    if (!backdrop.hidden) {
      input.focus();
      return;
    }
    const root = deps.workspaceRoot();
    if (!root) {
      deps.setStatus('Open a workspace before searching for files.');
      return;
    }
    previousFocus = document.activeElement instanceof HTMLElement ? document.activeElement : null;
    const available = currentIndex(deps.files()).pathSet;
    recent = readRecent(root).filter((path) => available.has(path));
    selected = 0;
    input.value = '';
    backdrop.hidden = false;
    render();
    input.focus();
  };

  input.addEventListener('input', () => {
    selected = 0;
    render();
  });
  input.addEventListener('keydown', (event) => {
    event.stopPropagation();
    if (event.key === 'Tab') {
      // The combobox is the only keyboard focus stop in this modal picker.
      event.preventDefault();
    } else if (event.key === 'Escape') {
      event.preventDefault();
      close();
    } else if (event.key === 'ArrowDown' || event.key === 'ArrowUp') {
      event.preventDefault();
      selected = nextQuickOpenSelection(selected, matches.length, event.key === 'ArrowDown' ? 1 : -1);
      updateSelection();
    } else if (event.key === 'Enter') {
      event.preventDefault();
      openSelected();
    }
  });
  backdrop.addEventListener('mousedown', (event) => {
    if (event.target === backdrop) close();
  });

  return { open, close, remember };
}
