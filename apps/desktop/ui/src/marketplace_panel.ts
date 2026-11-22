/** Marketplace catalog panel for extensions and skills. */
import { invoke } from '@tauri-apps/api/core';

export interface MarketplaceItemView {
  id: string;
  name: string;
  description: string;
  version: string;
  author: string;
  homepage?: string;
  repository?: string;
  license?: string;
  kind: 'extension' | 'skill';
  tags: string[];
  downloads: number;
  compatible_with: string[];
  asset_url: string;
  checksum: string;
  updated_at: string;
}

export interface MarketplaceSnapshot {
  protocol: number;
  path: string;
  extensions: MarketplaceItemView[];
  skills: MarketplaceItemView[];
  error?: string;
}

export interface MarketplaceInstalledItem {
  id: string;
  kind: string;
  version: string;
  installed_at: string;
  source_url: string;
  install_path: string;
}

export interface MarketplaceInstalledView {
  extensions: MarketplaceInstalledItem[];
  skills: MarketplaceInstalledItem[];
  last_checked: string;
}

export interface MarketplaceInstallResult {
  id: string;
  kind: string;
  version: string;
  installed_at: string;
  source_url: string;
  install_path: string;
  checksum: string;
}

const MAX_DESCRIPTION_LENGTH = 500;

export function skillKindLabel(kind: string): string {
  const labels: Record<string, string> = {
    analyzer: 'Analyzer',
    generator: 'Generator',
    linter: 'Linter',
    context: 'Context Extractor',
    'security-analyzer': 'Security Analyzer',
    custom: 'Custom',
  };
  return labels[kind] || kind;
}

export function itemUrlToDisplay(url: string): string {
  try {
    const parsed = new URL(url);
    return parsed.hostname + parsed.pathname.replace(/\/$/, '');
  } catch {
    return url.slice(0, 60);
  }
}

export function formattedDate(dateStr: string): string {
  const date = new Date(dateStr);
  if (isNaN(date.getTime())) return 'unknown';
  const now = new Date();
  const diffHours = Math.abs(now.getTime() - date.getTime()) / 3600000;
  if (diffHours < 24) {
    return `${Math.floor(diffHours)}h ago`;
  }
  const diffDays = Math.floor(diffHours / 24);
  if (diffDays < 7) {
    return `${diffDays}d ago`;
  }
  return date.toLocaleDateString();
}

export interface MarketplacePanelDeps {
  setStatus: (message: string) => void;
  refreshInstalled: () => void;
}

export function mountMarketplacePanel(deps: MarketplacePanelDeps): void {
  const count = byId<HTMLElement>('marketplaceCount');
  const error = byId<HTMLElement>('marketplaceError');
  const refreshBtn = byId<HTMLButtonElement>('marketplaceRefresh');
  const tabs = byId<HTMLDivElement>('marketplaceTabs');
  const lists: { extensions: HTMLElement; skills: HTMLElement } = {
    extensions: byId<HTMLElement>('marketplaceExtensions'),
    skills: byId<HTMLElement>('marketplaceSkills'),
  };

  let activeTab: 'extensions' | 'skills' = 'extensions';
  let currentItems: MarketplaceItemView[] = [];
  let currentType: 'extension' | 'skill' = 'extension';

  function byId<T extends HTMLElement>(id: string): T {
    const el = document.getElementById(id);
    if (!el) throw new Error('Missing marketplace element ' + id);
    return el as T;
  }

  function showError(message: string): void {
    error.textContent = message;
    error.classList.remove('hidden');
    count.textContent = 'unavailable';
  }

  function clearError(): void {
    error.classList.add('hidden');
    error.textContent = '';
  }

  async function loadMarketplace(type: 'extension' | 'skill'): Promise<void> {
    try {
      clearError();
      refreshBtn.disabled = true;
      const items = await invoke<MarketplaceItemView[]>(
        'marketplace_list',
        { itemType: type }
      );
      currentItems = items;
      currentType = type;
      renderList();
      count.textContent = `${items.length} ${type}s`;
    } catch (e) {
      showError(e instanceof Error ? e.message : String(e));
    } finally {
      refreshBtn.disabled = false;
    }
  }

  async function searchMarketplace(query: string, type: 'extension' | 'skill'): Promise<void> {
    try {
      clearError();
      refreshBtn.disabled = true;
      const items = await invoke<MarketplaceItemView[]>(
        'marketplace_search',
        { query, itemType: type }
      );
      currentItems = items;
      currentType = type;
      renderList();
      count.textContent = items.length === 0
        ? `No ${type}s found matching "${query}"`
        : `${items.length} ${type}s for "${query}"`;
    } catch (e) {
      showError(e instanceof Error ? e.message : String(e));
    } finally {
      refreshBtn.disabled = false;
    }
  }

  async function installItem(id: string): Promise<void> {
    try {
      const result = await invoke<MarketplaceInstallResult>('marketplace_install', { id });
      deps.setStatus(`${result.kind === 'extension' ? 'Extension' : 'Skill'} installed: ${id}`);
      deps.refreshInstalled();
    } catch (e) {
      deps.setStatus(`Install failed: ${e instanceof Error ? e.message : String(e)}`);
    }
  }

  function renderList(): void {
    const fragment = document.createDocumentFragment();
    const isExtension = currentType === 'extension';
    const items = currentItems.filter(item => item.kind === currentType);

    if (items.length === 0) {
      const hint = document.createElement('p');
      hint.className = 'hint';
      hint.textContent = isExtension
        ? 'No extensions available yet. Check your network connection.'
        : 'No skills available yet. Check your network connection.';
      fragment.append(hint);
      lists[isExtension ? 'extensions' : 'skills'].replaceChildren(fragment);
      return;
    }

    for (const item of items) {
      const card = createItemCard(item, isExtension);
      fragment.append(card);
    }

    lists[isExtension ? 'extensions' : 'skills'].replaceChildren(fragment);
  }

  function createItemCard(item: MarketplaceItemView, isExtension: boolean): HTMLElement {
    const article = document.createElement('article');
    article.className = 'marketplace-item';
    article.dataset.kind = item.kind;

    const head = document.createElement('div');
    head.className = 'marketplace-item-head';
    const title = document.createElement('strong');
    title.className = 'marketplace-item-title';
    title.textContent = item.name;
    const idSpan = document.createElement('span');
    idSpan.className = 'marketplace-item-id';
    idSpan.textContent = item.id;
    head.append(title, idSpan);

    const kindBadge = document.createElement('span');
    kindBadge.className = 'marketplace-badge ' + (isExtension ? 'extension' : 'skill');
    kindBadge.textContent = isExtension ? 'Extension' : skillKindLabel(item.tags[0] || 'custom');

    const description = document.createElement('p');
    description.className = 'marketplace-item-desc';
    description.textContent = item.description.slice(0, MAX_DESCRIPTION_LENGTH) +
      (item.description.length > MAX_DESCRIPTION_LENGTH ? '…' : '');

    const meta = document.createElement('div');
    meta.className = 'marketplace-item-meta';
    const versionSpan = document.createElement('span');
    versionSpan.className = 'marketplace-version';
    versionSpan.textContent = `v${item.version}`;
    const authorSpan = document.createElement('span');
    authorSpan.className = 'marketplace-author';
    authorSpan.textContent = `by ${item.author}`;
    const updatedSpan = document.createElement('span');
    updatedSpan.className = 'marketplace-updated';
    updatedSpan.textContent = formattedDate(item.updated_at);
    meta.append(versionSpan, authorSpan, updatedSpan);

    const tagsContainer = document.createElement('div');
    tagsContainer.className = 'marketplace-tags';
    for (const tag of item.tags.slice(0, 5)) {
      if (tag === 'skill-kind') continue;
      const tagEl = document.createElement('span');
      tagEl.className = 'marketplace-tag';
      tagEl.textContent = tag;
      tagsContainer.append(tagEl);
    }

    const actions = document.createElement('div');
    actions.className = 'marketplace-actions';
    const installBtn = document.createElement('button');
    installBtn.type = 'button';
    installBtn.className = 'button primary compact';
    installBtn.textContent = 'Install';
    installBtn.addEventListener('click', (e) => {
      e.stopPropagation();
      void installItem(item.id);
    });
    actions.append(installBtn);

    article.append(head, kindBadge, description, meta, tagsContainer, actions);
    return article;
  }

  function switchTab(to: 'extensions' | 'skills'): void {
    if (activeTab === to) return;
    activeTab = to;
    tabs.querySelectorAll('button').forEach(btn => {
      btn.classList.toggle('active', btn.dataset.tab === to);
    });
    void loadMarketplace(to === 'extensions' ? 'extension' : 'skill');
  }

  // Tab event listeners
  tabs.querySelector('[data-tab="extensions"]')?.addEventListener('click', () => {
    switchTab('extensions');
  });
  tabs.querySelector('[data-tab="skills"]')?.addEventListener('click', () => {
    switchTab('skills');
  });

  // Refresh button
  refreshBtn.addEventListener('click', () => {
    void loadMarketplace(currentType);
  });

  // Search handling
  const searchInput = document.getElementById('marketplaceSearch') as HTMLInputElement;
  if (searchInput) {
    let searchTimer: number;
    searchInput.addEventListener('input', (e) => {
      window.clearTimeout(searchTimer);
      const query = (e.target as HTMLInputElement).value.trim();
      if (query.length === 0) {
        void loadMarketplace(currentType);
        return;
      }
      if (query.length > 2) {
        searchTimer = window.setTimeout(() => {
          void searchMarketplace(query, currentType);
        }, 300);
      }
    });
  }

  // Initial load
  void loadMarketplace('extension');
}