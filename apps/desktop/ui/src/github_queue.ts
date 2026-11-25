/** Native Tauri read-only GitHub queue. OAuth and bearer tokens never enter this UI. */
import { invoke } from '@tauri-apps/api/core';
import { renderPullCard, type PullDetail } from './github_pull_card';

interface GitHubRepository {
  installationId: number;
  repoId: number;
  name: string;
}
interface GitHubQueueItem {
  number: number;
  title: string;
  html_url: string;
  draft?: boolean;
  state?: string;
  detail?: PullDetail | null;
}
interface GitHubQueue {
  repository: string;
  pulls: GitHubQueueItem[];
  issues: GitHubQueueItem[];
}

export function safeGithubUrl(value: string): string | null {
  try {
    const url = new URL(value);
    if (url.protocol !== 'https:' || url.host !== 'github.com' || url.username || url.password
      || !/^\/[A-Za-z0-9_.-]+\/[A-Za-z0-9_.-]+\/(pull|issues)\/[1-9][0-9]*\/?$/.test(url.pathname)) {
      return null;
    }
    return url.origin + url.pathname;
  } catch {
    return null;
  }
}

function byId<T extends HTMLElement>(id: string): T {
  const item = document.getElementById(id);
  if (!item) throw new Error('Missing GitHub queue element ' + id);
  return item as T;
}

export function mountGithubQueue(setStatus: (value: string) => void): void {
  const code = byId<HTMLInputElement>('githubPairCode');
  const pair = byId<HTMLButtonElement>('githubPair');
  const repo = byId<HTMLSelectElement>('githubRepo');
  const refresh = byId<HTMLButtonElement>('githubRefresh');
  const unpair = byId<HTMLButtonElement>('githubUnpair');
  const count = byId<HTMLElement>('githubQueueCount');
  const list = byId<HTMLElement>('githubQueueList');
  let repositories: GitHubRepository[] = [];
  let paired = false;
  let requestVersion = 0;
  let activeDialog: HTMLDialogElement | null = null;

  function closePull() {
    activeDialog?.close();
    activeDialog?.remove();
    activeDialog = null;
  }

  async function openPull(item: GitHubQueueItem, repository: string) {
    const selected = repositories.find(entry => String(entry.installationId) + ':' + String(entry.repoId) === repo.value);
    if (!paired || !selected) return;
    closePull();
    const dialog = document.createElement('dialog');
    dialog.className = 'github-pr-dialog';
    dialog.setAttribute('aria-label', 'Pull request #' + item.number);
    const dismiss = document.createElement('button');
    dismiss.type = 'button';
    dismiss.className = 'github-pr-dismiss';
    dismiss.textContent = 'Close';
    dismiss.addEventListener('click', () => dialog.close());
    const content = document.createElement('div');
    content.setAttribute('aria-live', 'polite');
    content.append(renderPullCard(item, repository, safeGithubUrl(item.html_url), 'Loading pull request details…'));
    dialog.append(dismiss, content);
    activeDialog = dialog;
    dialog.addEventListener('close', () => { dialog.remove(); if (activeDialog === dialog) activeDialog = null; });
    document.body.append(dialog);
    dialog.showModal();
    try {
      const result = await invoke<GitHubQueue>('get_github_queue', { installationId: selected.installationId, repoId: selected.repoId, pullNumber: item.number });
      if (activeDialog !== dialog || !dialog.open) return;
      const pull = result.pulls.find(entry => entry.number === item.number);
      if (!pull?.detail) throw new Error('Pull request details unavailable');
      content.replaceChildren(renderPullCard(pull, result.repository, safeGithubUrl(pull.html_url)));
    } catch {
      if (activeDialog === dialog && dialog.open) content.replaceChildren(renderPullCard(item, repository, safeGithubUrl(item.html_url), 'Details unavailable. Open GitHub to verify.'));
    }
  }

  function showNotice(text: string) {
    const p = document.createElement('p');
    p.className = 'hint';
    p.textContent = text;
    list.replaceChildren(p);
  }

  function renderQueue(queue: GitHubQueue) {
    list.replaceChildren();
    const pulls = Array.isArray(queue.pulls) ? queue.pulls.slice(0, 30) : [];
    const issues = Array.isArray(queue.issues) ? queue.issues.slice(0, 30) : [];
    count.textContent = String(pulls.length + issues.length);
    for (const [title, items] of [['Pull requests', pulls], ['Issues', issues]] as const) {
      const heading = document.createElement('h4');
      heading.textContent = title + ' (' + items.length + ')';
      list.append(heading);
      if (!items.length) {
        const note = document.createElement('p');
        note.className = 'hint';
        note.textContent = 'No open ' + title.toLowerCase() + '.';
        list.append(note);
        continue;
      }
      for (const item of items) {
        const row = document.createElement('div');
        row.className = 'github-queue-item';
        const label = '#' + item.number + ' ' + item.title + (item.draft ? ' · draft' : '');
        const url = safeGithubUrl(item.html_url);
        if (title === 'Pull requests') {
          const button = document.createElement('button');
          button.type = 'button';
          button.className = 'github-pr-launch';
          button.textContent = label;
          button.addEventListener('click', () => { void openPull(item, queue.repository); });
          row.append(button);
        } else if (url) {
          const link = document.createElement('a');
          link.textContent = label;
          link.href = url;
          link.target = '_blank';
          link.rel = 'noopener noreferrer';
          row.append(link);
        } else {
          row.textContent = label;
        }
        list.append(row);
      }
    }
  }

  async function loadRepositories(): Promise<void> {
    repositories = await invoke<GitHubRepository[]>('list_github_repositories');
    repo.replaceChildren();
    for (const item of repositories) {
      const option = document.createElement('option');
      option.value = String(item.installationId) + ':' + String(item.repoId);
      option.textContent = item.name;
      repo.append(option);
    }
    if (!repositories.length) {
      showNotice('No GitHub App installations with permitted repositories.');
      count.textContent = '0';
      return;
    }
    await loadQueue();
  }

  async function loadQueue(): Promise<void> {
    if (!paired) return;
    const selected = repositories.find(
      (item) => String(item.installationId) + ':' + String(item.repoId) === repo.value,
    );
    if (!selected) return;
    closePull();
    const version = ++requestVersion;
    count.textContent = 'loading';
    try {
      const queue = await invoke<GitHubQueue>('get_github_queue', {
        installationId: selected.installationId,
        repoId: selected.repoId,
        pullNumber: null,
      });
      if (version === requestVersion && paired) renderQueue(queue);
    } catch (error) {
      if (version !== requestVersion || !paired) return;
      count.textContent = 'unavailable';
      showNotice('GitHub queue unavailable: ' + String(error));
    }
  }

  pair.addEventListener('click', () => {
    const submitted = code.value.trim();
    if (!submitted) {
      showNotice('Paste the single-use pairing code from the GitHub workbench first.');
      code.focus();
      return;
    }
    pair.disabled = true;
    void (async () => {
      try {
        await invoke('pair_github_queue', { code: submitted });
        code.value = '';
        paired = true;
        count.textContent = 'connected';
        setStatus('GitHub paired for read-only PR and issue review.');
        await loadRepositories();
      } catch (error) {
        paired = false;
        code.value = '';
        count.textContent = 'not connected';
        showNotice('Pairing failed: ' + String(error));
      } finally {
        pair.disabled = false;
      }
    })();
  });
  refresh.addEventListener('click', () => {
    void (async () => {
      if (!paired) {
        showNotice('Pair GitHub first using a code generated by the browser workbench.');
        return;
      }
      try {
        await loadRepositories();
      } catch (error) {
        showNotice('Cannot refresh repositories: ' + String(error));
      }
    })();
  });
  repo.addEventListener('change', () => { void loadQueue(); });
  unpair.addEventListener('click', () => {
    void invoke('disconnect_github_queue').then(() => {
      paired = false;
      ++requestVersion;
      closePull();
      repositories = [];
      repo.replaceChildren();
      count.textContent = 'not connected';
      showNotice('Read-only GitHub access disconnected.');
    }).catch((error) => { setStatus('Disconnect error: ' + String(error)); });
  });
}
