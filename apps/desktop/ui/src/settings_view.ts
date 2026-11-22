//! The visual settings dialog: provider catalog editing, language-server
//! registry editing, and keybinding overrides. Every save is validated by the
//! Rust shell; nothing is written unless validation passes.

import { invoke } from '@tauri-apps/api/core';
import {
  DEFAULT_KEYBINDINGS,
  normalizeChord,
  type Keybinding,
} from './keybindings';

export interface SettingsSnapshot {
  protocol: number;
  path: string;
  providers: string | null;
  lspServers: string | null;
}

export interface LspRow {
  id: string;
  command: string;
  args: string[];
  languages: string[];
}

/** Parses the saved registry JSON into editable rows. */
export function registryToRows(json: string | null): LspRow[] {
  if (!json) return [];
  try {
    const parsed: unknown = JSON.parse(json);
    if (typeof parsed !== 'object' || parsed === null) return [];
    const servers = (parsed as { servers?: unknown }).servers;
    if (!Array.isArray(servers)) return [];
    const rows: LspRow[] = [];
    for (const raw of servers) {
      if (typeof raw !== 'object' || raw === null) continue;
      const server = raw as {
        id?: unknown;
        command?: unknown;
        args?: unknown;
        languages?: unknown;
      };
      if (typeof server.id !== 'string' || typeof server.command !== 'string') continue;
      rows.push({
        id: server.id,
        command: server.command,
        args: Array.isArray(server.args) ? server.args.filter((a): a is string => typeof a === 'string') : [],
        languages: Array.isArray(server.languages)
          ? server.languages.filter((l): l is string => typeof l === 'string')
          : [],
      });
    }
    return rows;
  } catch {
    return [];
  }
}

/** Serializes rows back to registry JSON for the Rust validator. */
export function rowsToRegistry(rows: readonly LspRow[]): string {
  return JSON.stringify({
    version: 1,
    servers: rows.map((row) => ({
      id: row.id.trim(),
      command: row.command.trim(),
      args: row.args,
      languages: row.languages.map((language) => language.trim().toLowerCase()),
    })),
  });
}

export function splitList(value: string): string[] {
  return value
    .split(/[,\s]+/)
    .map((part) => part.trim())
    .filter((part) => part.length > 0);
}

/** CSV-ish display form for args/languages editing. */
export function listToText(values: readonly string[]): string {
  return values.join(' ');
}

export interface SettingsPanelDeps {
  setStatus: (message: string) => void;
}

const MAX_SETTINGS_TEXT = 256 * 1024;
const KEYBINDINGS_STORAGE = 'ryoko.keybindings.v1';

function readOverrides(): Record<string, string> {
  try {
    const raw = localStorage.getItem(KEYBINDINGS_STORAGE);
    if (!raw) return {};
    const parsed: unknown = JSON.parse(raw);
    if (typeof parsed !== 'object' || parsed === null) return {};
    const out: Record<string, string> = {};
    for (const [action, chord] of Object.entries(parsed as Record<string, unknown>)) {
      if (typeof chord === 'string') out[action] = chord;
    }
    return out;
  } catch {
    return {};
  }
}

function writeOverrides(overrides: Record<string, string>): void {
  localStorage.setItem(KEYBINDINGS_STORAGE, JSON.stringify(overrides));
}

export function mountSettingsDialog(deps: SettingsPanelDeps): void {
  const dialog = document.getElementById('settingsDialog');
  if (!dialog) return;
  const openButton = document.getElementById('openSettings');
  const closeButton = dialog.querySelector<HTMLButtonElement>('#settingsClose');
  const pathLabel = dialog.querySelector<HTMLElement>('#settingsPath');
  const providersText = dialog.querySelector<HTMLTextAreaElement>('#settingsProviders');
  const saveProvidersButton = dialog.querySelector<HTMLButtonElement>('#settingsSaveProviders');
  const serverRows = dialog.querySelector<HTMLElement>('#settingsServerRows');
  const addServerButton = dialog.querySelector<HTMLButtonElement>('#settingsAddServer');
  const saveServersButton = dialog.querySelector<HTMLButtonElement>('#settingsSaveServers');
  const bindRows = dialog.querySelector<HTMLElement>('#settingsKeybindings');
  if (
    !closeButton || !pathLabel || !providersText || !saveProvidersButton
    || !serverRows || !addServerButton || !saveServersButton || !bindRows
  ) return;
  const pathText = pathLabel;
  const providerBox = providersText;
  const saveProviders = saveProvidersButton;
  const serverRowHost = serverRows;
  const addServer = addServerButton;
  const saveServers = saveServersButton;
  const keybindingHost = bindRows;
  void closeButton; void pathLabel; void providersText; void saveProvidersButton;
  void serverRows; void addServerButton; void saveServersButton; void bindRows;

  let rows: LspRow[] = [];
  const overrides: Record<string, string> = readOverrides();

  async function load(): Promise<void> {
    const snapshot = await invoke<SettingsSnapshot>('settings_read');
    pathText.textContent = snapshot.path;
    providerBox.value = snapshot.providers ?? '';
    rows = registryToRows(snapshot.lspServers);
    renderServerRows();
    renderKeybindingRows();
  }

  function renderServerRows(): void {
    serverRowHost.replaceChildren();
    rows.forEach((row, index) => renderServerRow(index, row));
    if (rows.length === 0) {
      const note = document.createElement('p');
      note.className = 'hint';
      note.textContent = 'No extra language servers configured; ryoko-lsp always runs.';
      serverRowHost.append(note);
    }
  }

  function renderServerRow(index: number, row: LspRow): void {
    const sync = () => {
      rows[index] = {
        id: id.value,
        command: command.value,
        languages: splitList(languages.value),
        args: splitList(args.value),
      };
    };
    const id = rowInput(row.id, 'id');
    const command = rowInput(row.command, 'command');
    const languages = rowInput(listToText(row.languages), 'languages');
    const args = rowInput(listToText(row.args), 'args');
    const remove = document.createElement('button');
    remove.type = 'button';
    remove.className = 'button ghost compact';
    remove.textContent = '\u00d7';
    remove.title = `Remove ${row.id}`;
    remove.addEventListener('click', () => {
      rows.splice(index, 1);
      renderServerRows();
    });
    for (const input of [id, command, languages, args]) {
      input.addEventListener('change', sync);
    }
    const line = document.createElement('div');
    line.className = 'settings-server-row';
    line.append(id, command, languages, args, remove);
    serverRowHost.append(line);
  }

  function rowInput(value: string, placeholder: string): HTMLInputElement {
    const input = document.createElement('input');
    input.type = 'text';
    input.value = value;
    input.placeholder = placeholder;
    input.spellcheck = false;
    return input;
  }

  function renderKeybindingRows(): void {
    keybindingHost.replaceChildren();
    for (const binding of DEFAULT_KEYBINDINGS) {
      keybindingHost.append(keybindingRow(binding));
    }
  }

  function keybindingRow(binding: Keybinding): HTMLElement {
    const line = document.createElement('div');
    line.className = 'settings-keybinding-row';
    const label = document.createElement('span');
    label.className = 'settings-keybinding-label';
    label.textContent = binding.title;
    const input = document.createElement('input');
    input.type = 'text';
    input.value = overrides[binding.action] ?? binding.chord;
    input.spellcheck = false;
    input.addEventListener('change', () => {
      const chord = normalizeChord(input.value);
      input.value = chord;
      if (chord === binding.chord) delete overrides[binding.action];
      else overrides[binding.action] = chord;
      writeOverrides(overrides);
      deps.setStatus(`Keybinding for ${binding.action}: ${chord}`);
    });
    line.append(label, input);
    return line;
  }

  openButton?.addEventListener('click', () => {
    dialog.classList.remove('hidden');
    void load();
  });
  closeButton.addEventListener('click', () => dialog.classList.add('hidden'));
  saveProviders.addEventListener('click', async () => {
    if (providerBox.value.length > MAX_SETTINGS_TEXT) {
      deps.setStatus('Provider catalog exceeds 256 KiB.');
      return;
    }
    try {
      const canonical = await invoke<string>('settings_save_providers', { json: providerBox.value });
      providerBox.value = canonical;
      deps.setStatus('Provider catalog saved.');
    } catch (error) {
      deps.setStatus(String(error));
    }
  });
  addServer.addEventListener('click', () => {
    rows.push({ id: '', command: '', args: [], languages: [] });
    renderServerRows();
  });
  saveServers.addEventListener('click', async () => {
    try {
      const canonical = await invoke<string>('settings_save_lsp_servers', {
        json: rowsToRegistry(rows),
      });
      rows = registryToRows(canonical);
      renderServerRows();
      deps.setStatus('Language servers saved. Reload languages to apply.');
    } catch (error) {
      deps.setStatus(String(error));
    }
  });
}
