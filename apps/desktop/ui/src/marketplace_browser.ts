/** Browse-only catalog plus an explicit local WASI install path.
 * Remote HTML and remote executable installation remain disabled.
 */
import { invoke } from '@tauri-apps/api/core';

type CatalogKind = 'extension' | 'skill';
interface CatalogItem {
  id: string;
  name: string;
  description: string;
  author: string;
  version: string;
  kind: CatalogKind;
}

interface InstalledExtension {
  id: string;
  name: string;
  version: string;
  kind: string;
  description: string;
  path: string;
  wasmSha256?: string | null;
}

export function mountMarketplaceBrowser(setStatus: (message: string) => void): void {
  const trigger = document.getElementById('openMarketplace') as HTMLButtonElement | null;
  if (!trigger) return;

  const dialog = document.createElement('dialog');
  dialog.className = 'marketplace-dialog';
  dialog.setAttribute('aria-labelledby', 'marketplaceBrowserTitle');

  const header = document.createElement('div');
  header.className = 'marketplace-browser-head';
  const title = document.createElement('h2');
  title.id = 'marketplaceBrowserTitle';
  title.textContent = 'Marketplace preview';
  const close = document.createElement('button');
  close.type = 'button';
  close.className = 'button ghost';
  close.textContent = 'Close';
  close.addEventListener('click', () => dialog.close());
  header.append(title, close);

  const explanation = document.createElement('p');
  explanation.className = 'marketplace-description';
  explanation.textContent =
    'Browse Ryoko extensions and skills. You can install a local WASI extension directory you already trust; remote installation and automatic policy changes stay disabled.';

  const toolbar = document.createElement('div');
  toolbar.className = 'marketplace-browser-toolbar';
  const extensionTab = document.createElement('button');
  const skillTab = document.createElement('button');
  for (const [tab, label] of [
    [extensionTab, 'Extensions'],
    [skillTab, 'Skills'],
  ] as const) {
    tab.type = 'button';
    tab.className = 'button ghost';
    tab.textContent = label;
  }
  const refresh = document.createElement('button');
  refresh.type = 'button';
  refresh.className = 'button ghost';
  refresh.textContent = 'Refresh';
  const installLocal = document.createElement('button');
  installLocal.type = 'button';
  installLocal.className = 'button primary';
  installLocal.textContent = 'Install local extension…';
  installLocal.title = 'Choose a local directory containing plugin.json and plugin.wasm';
  const shaInput = document.createElement('input');
  shaInput.type = 'text';
  shaInput.className = 'marketplace-sha-input';
  shaInput.placeholder = 'Optional SHA-256 pin';
  shaInput.maxLength = 64;
  shaInput.setAttribute('aria-label', 'Expected SHA-256 of plugin.wasm');
  toolbar.append(extensionTab, skillTab, refresh, installLocal, shaInput);

  const message = document.createElement('p');
  message.className = 'marketplace-browser-message';
  message.setAttribute('role', 'status');
  message.setAttribute('aria-live', 'polite');

  const installedTitle = document.createElement('h3');
  installedTitle.className = 'marketplace-installed-title';
  installedTitle.textContent = 'Installed locally';
  const installed = document.createElement('div');
  installed.className = 'marketplace-installed-list';
  const items = document.createElement('div');
  items.className = 'marketplace-browser-list';

  dialog.append(header, explanation, toolbar, installedTitle, installed, message, items);
  document.body.append(dialog);

  let kind: CatalogKind = 'extension';
  let requestId = 0;

  async function refreshInstalled(): Promise<void> {
    installed.replaceChildren();
    try {
      const extensions = await invoke<InstalledExtension[]>('list_installed_extensions');
      if (extensions.length === 0) {
        const empty = document.createElement('p');
        empty.className = 'marketplace-browser-message';
        empty.textContent = 'No local extensions installed.';
        installed.append(empty);
        return;
      }
      for (const extension of extensions) {
        const card = document.createElement('article');
        card.className = 'marketplace-browser-card';
        const name = document.createElement('h3');
        name.textContent = `${extension.name || extension.id} · v${extension.version}`;
        const metadata = document.createElement('p');
        metadata.className = 'marketplace-browser-metadata';
        metadata.textContent = `${extension.id} • ${extension.kind} • ${extension.path}`;
        const hash = document.createElement('p');
        hash.className = 'marketplace-browser-metadata';
        hash.textContent = extension.wasmSha256
          ? `SHA-256 ${extension.wasmSha256}`
          : 'Manifest has no pinned SHA-256';
        const note = document.createElement('span');
        note.className = 'marketplace-browser-restricted';
        note.textContent = 'Installed copy only. Policy opt-in is still required to run it.';
        card.append(name, metadata, hash, note);
        installed.append(card);
      }
    } catch (error) {
      const reason = error instanceof Error ? error.message : String(error);
      const failed = document.createElement('p');
      failed.className = 'marketplace-browser-message';
      failed.textContent = `Installed extensions unavailable: ${reason.slice(0, 300)}`;
      installed.append(failed);
    }
  }

  async function load(next: CatalogKind): Promise<void> {
    kind = next;
    const ticket = ++requestId;
    extensionTab.setAttribute('aria-pressed', String(next === 'extension'));
    skillTab.setAttribute('aria-pressed', String(next === 'skill'));
    message.textContent = 'Loading catalog…';
    items.replaceChildren();
    refresh.disabled = true;
    try {
      const response = await invoke<CatalogItem[]>('marketplace_catalog', { kind: next });
      if (ticket !== requestId) return;
      message.textContent = response.length
        ? `${response.length} ${next}s available to browse`
        : `No ${next}s currently listed.`;
      for (const item of response) {
        const card = document.createElement('article');
        card.className = 'marketplace-browser-card';
        const name = document.createElement('h3');
        name.textContent = item.name;
        const metadata = document.createElement('p');
        metadata.className = 'marketplace-browser-metadata';
        metadata.textContent = `${item.id} • v${item.version} • ${item.author}`;
        const description = document.createElement('p');
        description.textContent = item.description;
        const unavailable = document.createElement('span');
        unavailable.className = 'marketplace-browser-restricted';
        unavailable.textContent = 'Install disabled in alpha';
        card.append(name, metadata, description, unavailable);
        items.append(card);
      }
    } catch (error) {
      if (ticket !== requestId) return;
      const reason = error instanceof Error ? error.message : String(error);
      message.textContent = `Catalog unavailable: ${reason.slice(0, 300)}`;
      setStatus('Marketplace catalog unavailable. The registry or bundled CLI may not be configured.');
    } finally {
      if (ticket === requestId) refresh.disabled = false;
    }
  }

  extensionTab.addEventListener('click', () => { void load('extension'); });
  skillTab.addEventListener('click', () => { void load('skill'); });
  refresh.addEventListener('click', () => {
    void load(kind);
    void refreshInstalled();
  });
  installLocal.addEventListener('click', () => {
    void (async () => {
      try {
        const selected = await invoke<string | null>('pick_workspace_folder', { title: 'Choose a folder' });
        if (typeof selected !== 'string') return;
        const trimmed = shaInput.value.trim();
        if (trimmed && !/^[0-9a-fA-F]{64}$/.test(trimmed)) {
          message.textContent = 'The SHA-256 pin must be exactly 64 hexadecimal characters.';
          return;
        }
        message.textContent = 'Installing a local extension copy…';
        const result = await invoke<string>('install_wasi_extension', {
          sourcePath: selected,
          sha256: trimmed || null,
        });
        message.textContent = result.trim().slice(0, 500) || 'Extension installed.';
        shaInput.value = '';
        setStatus('Local extension installed. Review its manifest and policy before running it.');
        await refreshInstalled();
      } catch (error) {
        const reason = error instanceof Error ? error.message : String(error);
        message.textContent = `Install failed: ${reason.slice(0, 400)}`;
        setStatus('Local extension install failed.');
      }
    })();
  });
  trigger.addEventListener('click', () => {
    if (!dialog.open) dialog.showModal();
    void load(kind);
    void refreshInstalled();
  });
  dialog.addEventListener('close', () => { requestId += 1; });
}
