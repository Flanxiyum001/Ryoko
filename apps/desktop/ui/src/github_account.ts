/** Native GitHub account panel: sign-in, repositories, checkout sync, Copilot.
 *
 * The token never reaches this file. Every command returns a redacted status;
 * cloning and syncing are performed by the Rust shell with the session token.
 */
import { invoke } from '@tauri-apps/api/core';
import { open } from '@tauri-apps/plugin-dialog';

export interface GitHubStatus {
  connected: boolean;
  login: string;
  source: string;
  persisted: boolean;
  signInConfigured: boolean;
  storedCredentialAvailable: boolean;
}
export interface GitHubRepository {
  id: number;
  fullName: string;
  name: string;
  owner: string;
  defaultBranch: string;
  private: boolean;
  htmlUrl: string;
}
export interface SyncStatus {
  isRepository: boolean;
  branch: string | null;
  upstream: string | null;
  ahead: number;
  behind: number;
  dirty: boolean;
  remoteUrl: string | null;
}
export interface SyncOutcome {
  action: string;
  branch: string;
  upToDate: boolean;
  summary: string;
}
export interface LoginStart {
  userCode: string;
  verificationUri: string;
  expiresIn: number;
  interval: number;
}
export interface LoginPoll {
  state: 'pending' | 'connected' | 'denied' | 'expired' | string;
  status: GitHubStatus;
  message: string;
}

const DISCONNECTED: GitHubStatus = {
  connected: false,
  login: '',
  source: 'none',
  persisted: false,
  signInConfigured: false,
  storedCredentialAvailable: false,
};

/** One line that says where the checkout stands relative to its upstream. */
export function describeSync(status: SyncStatus): string {
  if (!status.isRepository) return 'Not a Git repository';
  const parts = [status.branch ?? 'detached HEAD'];
  if (status.upstream) {
    parts.push('↑' + String(status.ahead) + ' ↓' + String(status.behind));
  } else {
    parts.push('no upstream');
  }
  if (status.dirty) parts.push('uncommitted changes');
  return parts.join(' · ');
}

/** What the operator can do next, in one sentence. */
export function syncHint(status: SyncStatus, connected: boolean): string {
  if (!status.isRepository) return 'Open a Git repository to sync it with GitHub.';
  if (!connected) return 'Connect GitHub to fetch, pull and push this checkout.';
  if (!status.upstream) return 'This branch has no upstream yet; Push will publish it to origin.';
  if (status.behind > 0 && status.ahead > 0) {
    return 'The branch and its upstream have diverged. Ryoko only fast-forwards, so merge or rebase first.';
  }
  if (status.behind > 0) return 'The upstream has commits you do not have. Pull fast-forwards them.';
  if (status.ahead > 0) return 'You have commits the upstream does not have yet. Push publishes them.';
  return 'Up to date with the upstream branch.';
}

export function accountSummary(status: GitHubStatus): string {
  if (!status.connected) {
    return status.storedCredentialAvailable ? 'credential available' : 'not connected';
  }
  return status.persisted ? '@' + status.login : '@' + status.login + ' · this session';
}

function byId<T extends HTMLElement>(id: string): T {
  const element = document.getElementById(id);
  if (!element) throw new Error('Missing GitHub account element ' + id);
  return element as T;
}

export function mountGitHubAccount(
  setStatus: (value: string) => void,
  openFolderAt: (path: string) => Promise<void>,
): void {
  const summary = byId<HTMLElement>('githubAccountSummary');
  const notice = byId<HTMLElement>('githubAccountNotice');
  const connect = byId<HTMLButtonElement>('githubConnectExisting');
  const signIn = byId<HTMLButtonElement>('githubSignIn');
  const signOut = byId<HTMLButtonElement>('githubSignOut');
  const loginBox = byId<HTMLElement>('githubLoginBox');
  const userCode = byId<HTMLElement>('githubUserCode');
  const verification = byId<HTMLAnchorElement>('githubVerification');
  const loginState = byId<HTMLElement>('githubLoginState');
  const repo = byId<HTMLSelectElement>('githubRepoSelect');
  const parent = byId<HTMLInputElement>('githubCloneParent');
  const chooseParent = byId<HTMLButtonElement>('githubChooseParent');
  const clone = byId<HTMLButtonElement>('githubClone');
  const syncLabel = byId<HTMLElement>('githubSyncLabel');
  const syncHintText = byId<HTMLElement>('githubSyncHint');
  const fetchButton = byId<HTMLButtonElement>('githubFetch');
  const pullButton = byId<HTMLButtonElement>('githubPull');
  const pushButton = byId<HTMLButtonElement>('githubPush');
  const copilotModel = byId<HTMLSelectElement>('githubCopilotModel');
  const copilotUse = byId<HTMLButtonElement>('githubCopilotUse');
  const copilotStatus = byId<HTMLElement>('githubCopilotStatus');

  let status = DISCONNECTED;
  let repositories: GitHubRepository[] = [];
  let sync: SyncStatus = {
    isRepository: false, branch: null, upstream: null, ahead: 0, behind: 0, dirty: false, remoteUrl: null,
  };
  let loginTimer: number | null = null;

  function say(text: string) {
    notice.textContent = text;
    notice.classList.remove('hidden');
  }

  function clearNotice() {
    notice.textContent = '';
    notice.classList.add('hidden');
  }

  function render() {
    summary.textContent = accountSummary(status);
    connect.disabled = status.connected || !status.storedCredentialAvailable;
    signIn.disabled = status.connected || !status.signInConfigured;
    signIn.textContent = status.signInConfigured ? 'Sign in with GitHub' : 'Sign-in needs RYOKO_GITHUB_CLIENT_ID';
    signOut.disabled = !status.connected;
    clone.disabled = !status.connected || repositories.length === 0 || !parent.value;
    repo.disabled = !status.connected || repositories.length === 0;
    const canSync = status.connected && sync.isRepository;
    fetchButton.disabled = !canSync;
    pullButton.disabled = !canSync || sync.dirty;
    pushButton.disabled = !canSync;
    syncLabel.textContent = describeSync(sync);
    syncHintText.textContent = syncHint(sync, status.connected);
  }

  async function refreshStatus(): Promise<void> {
    status = await invoke<GitHubStatus>('github_status');
    if (!status.connected) {
      repositories = [];
      repo.replaceChildren();
    }
    render();
  }

  async function loadRepositories(): Promise<void> {
    if (!status.connected) return;
    repositories = await invoke<GitHubRepository[]>('github_repositories');
    repo.replaceChildren();
    for (const item of repositories) {
      const option = document.createElement('option');
      option.value = item.fullName;
      option.textContent = item.fullName + (item.private ? ' · private' : '');
      repo.append(option);
    }
    if (!repositories.length) {
      say('This GitHub account has no repositories Ryoko can list.');
    }
    render();
  }

  async function refreshSync(): Promise<void> {
    try {
      sync = await invoke<SyncStatus>('git_sync_status');
    } catch (error) {
      sync = { isRepository: false, branch: null, upstream: null, ahead: 0, behind: 0, dirty: false, remoteUrl: null };
      syncHintText.textContent = 'Sync status unavailable: ' + String(error);
    }
    render();
  }

  async function loadCopilot(): Promise<void> {
    copilotModel.replaceChildren();
    copilotUse.disabled = true;
    if (!status.connected) {
      copilotStatus.textContent = 'Connect GitHub to check Copilot.';
      return;
    }
    copilotStatus.textContent = 'Checking Copilot…';
    try {
      const models = await invoke<string[]>('github_copilot_models');
      if (!models.length) {
        copilotStatus.textContent = 'GitHub Copilot is not enabled for this account.';
        return;
      }
      for (const model of models) {
        const option = document.createElement('option');
        option.value = model;
        option.textContent = model;
        copilotModel.append(option);
      }
      copilotStatus.textContent = String(models.length) + ' Copilot models available.';
      copilotUse.disabled = false;
    } catch (error) {
      copilotStatus.textContent = 'Copilot unavailable: ' + String(error);
    }
  }

  function stopPolling() {
    if (loginTimer !== null) {
      window.clearInterval(loginTimer);
      loginTimer = null;
    }
  }

  function startPolling(intervalSeconds: number) {
    stopPolling();
    loginTimer = window.setInterval(() => {
      void pollLogin();
    }, Math.max(5, intervalSeconds) * 1000);
  }

  async function pollLogin(): Promise<void> {
    try {
      const poll = await invoke<LoginPoll>('github_sign_in_poll');
      loginState.textContent = poll.message;
      if (poll.state === 'connected') {
        stopPolling();
        loginBox.classList.add('hidden');
        status = poll.status;
        setStatus('Signed in to GitHub as ' + status.login + '.');
        await refreshStatus();
        await loadRepositories();
        await loadCopilot();
        await refreshSync();
      } else if (poll.state === 'pending') {
        status = poll.status;
        render();
      } else {
        stopPolling();
        say(poll.message);
        loginBox.classList.add('hidden');
      }
    } catch (error) {
      stopPolling();
      say('Sign-in failed: ' + String(error));
    }
  }

  connect.addEventListener('click', () => {
    clearNotice();
    connect.disabled = true;
    void (async () => {
      try {
        status = await invoke<GitHubStatus>('github_connect_existing');
        setStatus('Connected to GitHub as ' + status.login + '.');
        await loadRepositories();
        await loadCopilot();
        await refreshSync();
      } catch (error) {
        say(String(error));
      } finally {
        render();
      }
    })();
  });

  signIn.addEventListener('click', () => {
    clearNotice();
    signIn.disabled = true;
    void (async () => {
      try {
        const start = await invoke<LoginStart>('github_sign_in_start');
        userCode.textContent = start.userCode;
        verification.textContent = start.verificationUri;
        verification.href = start.verificationUri;
        loginBox.classList.remove('hidden');
        loginState.textContent =
          'Open the page, enter the code, and approve. This panel checks automatically.';
        startPolling(start.interval);
        setStatus('Waiting for GitHub approval.');
      } catch (error) {
        say('Cannot start GitHub sign-in: ' + String(error));
      } finally {
        render();
      }
    })();
  });

  signOut.addEventListener('click', () => {
    clearNotice();
    stopPolling();
    void (async () => {
      try {
        status = await invoke<GitHubStatus>('github_sign_out');
        repositories = [];
        repo.replaceChildren();
        copilotModel.replaceChildren();
        copilotUse.disabled = true;
        copilotStatus.textContent = 'Connect GitHub to check Copilot.';
        loginBox.classList.add('hidden');
        setStatus('Signed out of GitHub.');
        await refreshStatus();
      } catch (error) {
        say('Sign-out failed: ' + String(error));
      }
    })();
  });

  chooseParent.addEventListener('click', () => {
    void (async () => {
      const selected = await open({ directory: true, multiple: false });
      if (typeof selected !== 'string') return;
      parent.value = selected;
      render();
    })();
  });

  parent.addEventListener('change', render);

  clone.addEventListener('click', () => {
    const repository = repo.value;
    if (!repository || !parent.value) return;
    clearNotice();
    clone.disabled = true;
    say('Cloning ' + repository + '…');
    void (async () => {
      try {
        const workspace = await invoke<{ root: string; name: string }>('github_clone_repository', {
          repository,
          parent: parent.value,
        });
        // Opening goes through the editor's own guarded flow so unsaved buffers
        // and an active run are never discarded silently.
        await openFolderAt(workspace.root);
        say('Cloned ' + repository + ' and opened ' + workspace.name + '.');
        await refreshSync();
      } catch (error) {
        say('Clone failed: ' + String(error));
      } finally {
        render();
      }
    })();
  });

  function syncAction(action: 'fetch' | 'pull' | 'push', button: HTMLButtonElement) {
    clearNotice();
    button.disabled = true;
    void (async () => {
      try {
        const outcome = await invoke<SyncOutcome>('git_sync', { action });
        say(outcome.summary);
        setStatus(outcome.summary);
      } catch (error) {
        say(String(error));
      } finally {
        button.disabled = false;
        await refreshSync();
      }
    })();
  }

  fetchButton.addEventListener('click', () => syncAction('fetch', fetchButton));
  pullButton.addEventListener('click', () => syncAction('pull', pullButton));
  pushButton.addEventListener('click', () => syncAction('push', pushButton));

  copilotUse.addEventListener('click', () => {
    const model = copilotModel.value;
    if (!model) return;
    clearNotice();
    copilotUse.disabled = true;
    void (async () => {
      try {
        const message = await invoke<string>('github_use_copilot', { model });
        copilotStatus.textContent = 'Copilot ' + model + ' is now the workspace model.';
        setStatus('GitHub Copilot selected: ' + model + '.');
        say(message);
      } catch (error) {
        say('Cannot use Copilot: ' + String(error));
      } finally {
        copilotUse.disabled = false;
      }
    })();
  });

  void (async () => {
    try {
      await refreshStatus();
      if (status.connected) {
        await loadRepositories();
        await loadCopilot();
      }
      await refreshSync();
    } catch (error) {
      say('GitHub panel unavailable: ' + String(error));
    }
  })();

  // Sync state changes on disk, not on a timer; re-read it when the window
  // becomes active again instead of polling in the background.
  window.addEventListener('focus', () => {
    if (status.connected) void refreshSync();
  });
}
