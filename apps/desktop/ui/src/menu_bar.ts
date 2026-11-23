//! Application menus. `MENUS` is the single description of File / Edit /
//! View / Run / Help: the in-window menu bar renders it on Windows and Linux,
//! and the native macOS menu (built in Rust) uses the same command ids, which
//! come back to the renderer as `ryoko://menu` events.

import type { CommandRegistry } from './commands';

export type MenuEntry = { command: string } | { separator: true };

export interface MenuDefinition {
  id: string;
  label: string;
  entries: MenuEntry[];
}

export const MENUS: readonly MenuDefinition[] = [
  {
    id: 'file',
    label: 'File',
    entries: [
      { command: 'workspace.openFolder' },
      { command: 'workbench.quickOpen' },
      { separator: true },
      { command: 'editor.save' },
      { command: 'editor.close' },
      { separator: true },
      { command: 'workbench.settings' },
    ],
  },
  {
    id: 'edit',
    label: 'Edit',
    entries: [
      { command: 'editor.undo' },
      { command: 'editor.redo' },
      { separator: true },
      { command: 'editor.find' },
      { command: 'editor.replace' },
      { command: 'workbench.search' },
      { command: 'workbench.replace' },
      { separator: true },
      { command: 'editor.inlineEdit' },
    ],
  },
  {
    id: 'view',
    label: 'View',
    entries: [
      { command: 'workbench.commandPalette' },
      { separator: true },
      { command: 'view.explorer' },
      { command: 'workbench.changes' },
      { command: 'view.mission' },
      { command: 'view.problems' },
      { command: 'view.extensions' },
      { separator: true },
      { command: 'view.toggleSidebar' },
      { command: 'view.toggleInspector' },
      { command: 'view.togglePanel' },
      { command: 'view.toggleMinimap' },
      { command: 'view.resetLayout' },
      { separator: true },
      { command: 'theme.pick' },
    ],
  },
  {
    id: 'run',
    label: 'Run',
    entries: [
      { command: 'run.activeFile' },
      { separator: true },
      { command: 'workbench.debug' },
      { command: 'terminal.show' },
    ],
  },
  {
    id: 'help',
    label: 'Help',
    entries: [
      { command: 'help.welcome' },
      { command: 'help.keyboardShortcuts' },
    ],
  },
];

/** Every command id the menus reference; used to check the registry covers them. */
export function menuCommandIds(menus: readonly MenuDefinition[] = MENUS): string[] {
  return menus.flatMap((menu) => menu.entries.flatMap((entry) => ('command' in entry ? [entry.command] : [])));
}

export interface MenuBarDeps {
  registry: CommandRegistry;
  shortcutFor: (id: string) => string | null;
  setStatus: (message: string) => void;
}

/** Renders a keyboard-accessible menu bar (WAI-ARIA menubar pattern). */
export function mountMenuBar(host: HTMLElement, deps: MenuBarDeps): void {
  host.setAttribute('role', 'menubar');
  host.setAttribute('aria-label', 'Application menu');
  host.replaceChildren();
  let openMenu: HTMLElement | null = null;

  const closeAll = (focusTrigger = false): void => {
    if (!openMenu) return;
    const trigger = openMenu.previousElementSibling as HTMLButtonElement | null;
    openMenu.hidden = true;
    trigger?.setAttribute('aria-expanded', 'false');
    openMenu = null;
    if (focusTrigger) trigger?.focus();
  };

  const triggers: HTMLButtonElement[] = [];

  const openAt = (index: number, focusFirst: boolean): void => {
    closeAll();
    const trigger = triggers[(index + triggers.length) % triggers.length];
    const popup = trigger.nextElementSibling as HTMLElement;
    popup.hidden = false;
    trigger.setAttribute('aria-expanded', 'true');
    openMenu = popup;
    for (const item of popup.querySelectorAll<HTMLButtonElement>('[role="menuitem"]')) {
      item.disabled = !deps.registry.isEnabled(item.dataset.command ?? '');
    }
    if (focusFirst) popup.querySelector<HTMLButtonElement>('[role="menuitem"]:not(:disabled)')?.focus();
    else trigger.focus();
  };

  MENUS.forEach((menu, menuIndex) => {
    const wrapper = document.createElement('div');
    wrapper.className = 'menu';
    const trigger = document.createElement('button');
    trigger.type = 'button';
    trigger.className = 'menu-trigger';
    trigger.id = `menu-${menu.id}`;
    trigger.textContent = menu.label;
    trigger.setAttribute('role', 'menuitem');
    trigger.setAttribute('aria-haspopup', 'menu');
    trigger.setAttribute('aria-expanded', 'false');
    trigger.tabIndex = menuIndex === 0 ? 0 : -1;
    triggers.push(trigger);

    const popup = document.createElement('div');
    popup.className = 'menu-popup';
    popup.setAttribute('role', 'menu');
    popup.setAttribute('aria-labelledby', trigger.id);
    popup.hidden = true;
    for (const entry of menu.entries) {
      if ('separator' in entry) {
        const rule = document.createElement('div');
        rule.className = 'menu-separator';
        rule.setAttribute('role', 'separator');
        popup.append(rule);
        continue;
      }
      const command = deps.registry.get(entry.command);
      if (!command) continue;
      const item = document.createElement('button');
      item.type = 'button';
      item.className = 'menu-item';
      item.setAttribute('role', 'menuitem');
      item.dataset.command = command.id;
      const label = document.createElement('span');
      label.textContent = command.title;
      item.append(label);
      const shortcut = deps.shortcutFor(command.id);
      if (shortcut) {
        const kbd = document.createElement('kbd');
        kbd.textContent = shortcut;
        item.append(kbd);
      }
      item.addEventListener('click', () => {
        closeAll();
        void deps.registry.execute(command.id).catch((error: unknown) => deps.setStatus(String(error)));
      });
      popup.append(item);
    }

    trigger.addEventListener('click', () => {
      if (openMenu === popup) closeAll();
      else openAt(menuIndex, false);
    });
    trigger.addEventListener('mouseenter', () => {
      if (openMenu && openMenu !== popup) openAt(menuIndex, false);
    });
    trigger.addEventListener('keydown', (event) => {
      if (event.key === 'ArrowRight' || event.key === 'ArrowLeft') {
        event.preventDefault();
        const next = menuIndex + (event.key === 'ArrowRight' ? 1 : -1);
        if (openMenu) openAt(next, false);
        else triggers[(next + triggers.length) % triggers.length].focus();
      } else if (event.key === 'ArrowDown' || event.key === 'Enter' || event.key === ' ') {
        event.preventDefault();
        openAt(menuIndex, true);
      } else if (event.key === 'Escape') {
        closeAll(true);
      }
    });
    popup.addEventListener('keydown', (event) => {
      const items = Array.from(popup.querySelectorAll<HTMLButtonElement>('[role="menuitem"]:not(:disabled)'));
      const position = items.indexOf(document.activeElement as HTMLButtonElement);
      if (event.key === 'ArrowDown' || event.key === 'ArrowUp') {
        event.preventDefault();
        const step = event.key === 'ArrowDown' ? 1 : -1;
        items[(position + step + items.length) % items.length]?.focus();
      } else if (event.key === 'Escape') {
        event.preventDefault();
        closeAll(true);
      } else if (event.key === 'ArrowRight' || event.key === 'ArrowLeft') {
        event.preventDefault();
        openAt(menuIndex + (event.key === 'ArrowRight' ? 1 : -1), true);
      } else if (event.key === 'Tab') {
        closeAll();
      }
    });

    wrapper.append(trigger, popup);
    host.append(wrapper);
  });

  document.addEventListener('mousedown', (event) => {
    if (openMenu && !host.contains(event.target as Node)) closeAll();
  });
}
