/** Browse-only catalog: no remote HTML or executable installation controls. */
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
    'Browse Ryoko extensions and skills. Installing third-party code is disabled during alpha testing.';

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
  toolbar.append(extensionTab, skillTab, refresh);

  const message = document.createElement('p');
  message.className = 'marketplace-browser-message';
  message.setAttribute('role', 'status');
  message.setAttribute('aria-live', 'polite');

  const items = document.createElement('div');
  items.className = 'marketplace-browser-list';

  dialog.append(header, explanation, toolbar, message, items);
  document.body.append(dialog);

  let kind: CatalogKind = 'extension';
  let requestId = 0;

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
  refresh.addEventListener('click', () => { void load(kind); });
  trigger.addEventListener('click', () => {
    if (!dialog.open) dialog.showModal();
    void load(kind);
  });
  dialog.addEventListener('close', () => { requestId += 1; });
}
