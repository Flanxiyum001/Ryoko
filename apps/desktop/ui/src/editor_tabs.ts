import { fileIconElement } from './file_icons';
/** Tab ordering is independent of Monaco models and never discards buffers. */
export function selectionAfterClose(
  paths: readonly string[], active: string | null, closing: string,
): string | null {
  const index = paths.indexOf(closing);
  if (index < 0 || active !== closing) return active;
  return paths[index + 1] ?? paths[index - 1] ?? null;
}

export interface EditorTab {
  path: string;
  dirty: boolean;
}

export function mountEditorTabs(
  host: HTMLElement,
  select: (path: string) => void,
  close: (path: string) => void,
): (tabs: EditorTab[], active: string | null) => void {
  host.setAttribute('role', 'tablist');
  host.setAttribute('aria-label', 'Open files');
  return (tabs, active) => {
    // Preserve keyboard focus when a dirty indicator changes while typing.
    const focused = host.contains(document.activeElement)
      ? (document.activeElement as HTMLElement).dataset : null;
    const focusPath = focused?.path;
    const focusAction = focused?.action;
    host.replaceChildren();
    tabs.forEach((file, index) => {
      const wrapper = document.createElement('div');
      wrapper.className = 'editor-tab' + (file.path === active ? ' selected' : '');
      wrapper.setAttribute('role', 'presentation');
      const tab = document.createElement('button');
      tab.type = 'button';
      tab.setAttribute('role', 'tab');
      tab.setAttribute('aria-selected', String(file.path === active));
      tab.setAttribute('aria-controls', 'editor');
      tab.setAttribute('aria-label', file.path + (file.dirty ? ', unsaved changes' : ''));
      tab.tabIndex = file.path === active ? 0 : -1;
      tab.dataset.path = file.path;
      tab.dataset.action = 'select';
      tab.title = file.path;
      // Full paths disambiguate files with the same basename.
      const name = file.path.split('/').pop() ?? file.path;
      const duplicate = tabs.some((other) => other.path !== file.path && other.path.split('/').pop() === name);
      const title = document.createElement('span');
      title.className = 'tab-title';
      title.textContent = duplicate ? file.path : name;
      tab.replaceChildren(fileIconElement(file.path), title);
      tab.addEventListener('click', () => select(file.path));
      tab.addEventListener('keydown', (event) => {
        let next: number;
        if (event.key === 'ArrowRight') next = (index + 1) % tabs.length;
        else if (event.key === 'ArrowLeft') next = (index + tabs.length - 1) % tabs.length;
        else if (event.key === 'Home') next = 0;
        else if (event.key === 'End') next = tabs.length - 1;
        else if (event.key === 'Delete') { event.preventDefault(); close(file.path); return; }
        else return;
        event.preventDefault();
        select(tabs[next].path);
        host.querySelectorAll<HTMLButtonElement>('[role="tab"]')[next]?.focus();
      });
      const dismiss = document.createElement('button');
      dismiss.type = 'button';
      dismiss.className = 'tab-close';
      dismiss.dataset.path = file.path;
      dismiss.dataset.action = 'close';
      dismiss.textContent = file.dirty ? '●' : '×';
      dismiss.title = file.dirty ? 'Close file (unsaved changes)' : 'Close file';
      dismiss.setAttribute('aria-label', `Close ${file.path}`);
      dismiss.addEventListener('click', () => close(file.path));
      wrapper.append(tab, dismiss);
      host.append(wrapper);
    });
    if (focusPath) {
      const target = [...host.querySelectorAll<HTMLButtonElement>('button')].find(
        (button) => button.dataset.path === focusPath && button.dataset.action === focusAction,
      );
      (target ?? host.querySelector<HTMLButtonElement>('[aria-selected="true"]'))?.focus();
    }
  };
}
