//! "New update available" notice, status bar item and settings toggle.

import { invoke } from '@tauri-apps/api/core';
import { listen } from '@tauri-apps/api/event';
import {
  loadPrefs, progressLabel, savePrefs, shouldAnnounce, shouldAutoCheck,
  type UpdateInfo, type UpdateStatus,
} from './updates';

export interface UpdateViewDeps {
  setStatus: (message: string) => void;
  /** Paths of open files with unsaved changes. */
  unsavedFiles: () => string[];
  statusHost: HTMLElement;
  settingsHost: HTMLElement | null;
}

export interface UpdateController {
  /** Manual check (Help › Check for Updates). */
  checkNow: () => Promise<void>;
}

const STARTUP_DELAY_MS = 8_000;
const RECHECK_MS = 60 * 60 * 1000;
const RELEASES_URL = 'https://github.com/Flanxiyum001/Ryoko/releases';

function button(label: string, className: string, onClick: () => void): HTMLButtonElement {
  const element = document.createElement('button');
  element.type = 'button';
  element.className = className;
  element.textContent = label;
  element.addEventListener('click', onClick);
  return element;
}

export function mountUpdates(deps: UpdateViewDeps): UpdateController {
  let prefs = loadPrefs(localStorage);
  let status: UpdateStatus | null = null;
  let available: UpdateInfo | null = null;
  let installing = false;

  // Status bar item, shown only while an update is available.
  const statusItem = button('', 'status-update hidden', () => showNotice());
  statusItem.id = 'updateStatus';
  statusItem.title = 'A new version of Ryoko is available';
  deps.statusHost.append(statusItem);

  // Notice (bottom right), built once and filled per update.
  const notice = document.createElement('section');
  notice.className = 'update-notice';
  notice.hidden = true;
  notice.setAttribute('role', 'dialog');
  notice.setAttribute('aria-modal', 'false');
  notice.setAttribute('aria-labelledby', 'updateNoticeTitle');
  const title = document.createElement('h2');
  title.id = 'updateNoticeTitle';
  const detail = document.createElement('p');
  detail.className = 'update-detail';
  const notes = document.createElement('pre');
  notes.className = 'update-notes';
  notes.hidden = true;
  const progress = document.createElement('p');
  progress.className = 'update-progress';
  progress.setAttribute('role', 'status');
  progress.setAttribute('aria-live', 'polite');
  const actions = document.createElement('div');
  actions.className = 'update-actions';
  const install = button('Download and install', 'button primary compact', () => void installUpdate());
  const toggleNotes = button('Release notes', 'button ghost compact', () => {
    notes.hidden = !notes.hidden;
    toggleNotes.textContent = notes.hidden ? 'Release notes' : 'Hide notes';
  });
  const skip = button('Skip this version', 'button ghost compact', () => {
    if (!available) return;
    prefs = { ...prefs, skippedVersion: available.version };
    savePrefs(localStorage, prefs);
    hideNotice();
    deps.setStatus(`Ryoko ${available.version} skipped. Check again from Help › Check for Updates.`);
  });
  const later = button('Later', 'button ghost compact', () => hideNotice());
  const close = button('×', 'update-close', () => hideNotice());
  close.setAttribute('aria-label', 'Close update notice');
  actions.append(install, toggleNotes, skip, later);
  notice.append(close, title, detail, notes, progress, actions);
  document.body.append(notice);

  function hideNotice(): void {
    if (!installing) notice.hidden = true;
  }

  function showNotice(): void {
    if (!available) return;
    title.textContent = 'A new update is available';
    detail.textContent = `Ryoko ${available.version} is ready to install. You have ${available.currentVersion}.`;
    notes.textContent = available.notes.trim() || 'No release notes were published for this version.';
    toggleNotes.hidden = false;
    progress.textContent = '';
    for (const control of [install, toggleNotes, skip, later]) control.disabled = installing;
    notice.hidden = false;
    install.focus();
  }

  function announce(info: UpdateInfo | null): void {
    available = info;
    statusItem.classList.toggle('hidden', !info);
    statusItem.textContent = info ? `⬆ Update ${info.version}` : '';
    if (info) showNotice();
  }

  async function check(manual: boolean): Promise<void> {
    status ??= await invoke<UpdateStatus>('update_status');
    if (!status.configured) {
      if (manual) {
        deps.setStatus(`This build cannot update itself. Download new versions from ${RELEASES_URL}`);
      }
      return;
    }
    if (manual) deps.setStatus('Checking for updates…');
    try {
      const info = await invoke<UpdateInfo | null>('update_check');
      prefs = { ...prefs, lastCheck: Date.now() };
      savePrefs(localStorage, prefs);
      if (shouldAnnounce(info, prefs, manual)) {
        announce(info);
        deps.setStatus(`Ryoko ${info!.version} is available.`);
      }
      else if (manual) deps.setStatus(`Ryoko is up to date (${status.currentVersion}).`);
    } catch (error) {
      if (manual) deps.setStatus(String(error));
    }
  }

  async function installUpdate(): Promise<void> {
    if (!available || installing) return;
    const unsaved = deps.unsavedFiles();
    if (unsaved.length && !window.confirm(
      `Ryoko restarts after updating. Discard unsaved changes in ${unsaved.length} file(s)?\n\n${unsaved.slice(0, 8).join('\n')}`,
    )) return;
    installing = true;
    for (const control of [install, toggleNotes, skip, later]) control.disabled = true;
    progress.textContent = 'Starting download…';
    try {
      await invoke('update_install');
      progress.textContent = 'Installed. Restarting…';
    } catch (error) {
      installing = false;
      for (const control of [install, toggleNotes, skip, later]) control.disabled = false;
      progress.textContent = String(error);
    }
  }

  void listen<{ downloaded: number; total: number | null }>('ryoko://update-progress', (event) => {
    const { downloaded, total } = event.payload ?? { downloaded: 0, total: null };
    if (typeof downloaded === 'number') progress.textContent = progressLabel(downloaded, typeof total === 'number' ? total : null);
  });

  // Settings › Updates.
  if (deps.settingsHost) {
    const label = document.createElement('label');
    label.className = 'run-check settings-updates';
    const box = document.createElement('input');
    box.type = 'checkbox';
    box.id = 'settingsAutoUpdate';
    box.checked = prefs.autoCheck;
    box.addEventListener('change', () => {
      prefs = { ...prefs, autoCheck: box.checked };
      savePrefs(localStorage, prefs);
    });
    label.append(box, ' Check for updates automatically');
    const now = button('Check now', 'button ghost compact', () => void check(true));
    deps.settingsHost.append(label, now);
  }

  const autoCheck = (): void => {
    if (shouldAutoCheck(prefs, Date.now())) void check(false).catch(() => {});
  };
  window.setTimeout(autoCheck, STARTUP_DELAY_MS);
  window.setInterval(autoCheck, RECHECK_MS);

  return { checkNow: () => check(true) };
}
