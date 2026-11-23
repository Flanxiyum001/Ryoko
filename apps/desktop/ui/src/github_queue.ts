/** Native GitHub Mission Control. OAuth, App keys and the pairing bearer stay in Rust/Node. */
import { invoke } from '@tauri-apps/api/core';

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
}
interface GitHubQueue {
  repository: string;
  pulls: GitHubQueueItem[];
  issues: GitHubQueueItem[];
}
interface GitHubJob {
  issue: number;
  status: string;
  number?: number;
}
interface GitHubChecks {
  ready: boolean;
  sha?: string;
  reason: string;
  checks?: number;
  statuses?: number;
}
interface GitHubProposal {
  id: string;
  source: string;
  operation: string;
  status: string;
  params: Record<string, unknown>;
  createdAt: string;
}

export function safeGithubUrl(value: string): string | null {
  try {
    const url = new URL(value);
    if (url.protocol !== 'https:' || url.hostname !== 'github.com'
      || url.username || url.password
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

export function proposalParams(
  operation: string,
  input: { projectName: string; branch: string; path: string; message: string; number: string; title: string; content: string },
): Record<string, unknown> {
  const branch = input.branch.trim();
  const number = Number(input.number.trim());
  switch (operation) {
    case 'clone_repository': return { projectName: input.projectName.trim() };
    case 'create_branch': return { branch };
    case 'commit_file':
      return { branch, path: input.path.trim(), content: input.content, message: input.message.trim() };
    case 'open_pull_request':
      return { head: branch, title: input.title.trim(), body: input.content };
    case 'review_comment': return { number, comment: input.content };
    case 'mark_ready_for_review': return { number };
    default: throw new Error('Unsupported GitHub action');
  }
}

export function mountGithubQueue(setStatus: (value: string) => void): void {
  const code = byId<HTMLInputElement>('githubPairCode');
  const pair = byId<HTMLButtonElement>('githubPair');
  const repo = byId<HTMLSelectElement>('githubRepo');
  const refresh = byId<HTMLButtonElement>('githubRefresh');
  const unpair = byId<HTMLButtonElement>('githubUnpair');
  const count = byId<HTMLElement>('githubQueueCount');
  const list = byId<HTMLElement>('githubQueueList');
  const proposalsHost = byId<HTMLElement>('githubProposalList');
  const proposalCount = byId<HTMLElement>('githubProposalCount');
  const operation = byId<HTMLSelectElement>('githubOperation');
  const branch = byId<HTMLInputElement>('githubBranch');
  const projectName = byId<HTMLInputElement>('githubProjectName');
  const path = byId<HTMLInputElement>('githubPath');
  const message = byId<HTMLInputElement>('githubMessage');
  const number = byId<HTMLInputElement>('githubNumber');
  const title = byId<HTMLInputElement>('githubTitle');
  const content = byId<HTMLTextAreaElement>('githubContent');
  const queueProposal = byId<HTMLButtonElement>('githubQueueProposal');

  let repositories: GitHubRepository[] = [];
  let paired = false;
  let refreshSerial = 0;
  let jobs: GitHubJob[] = [];

  function selected(): GitHubRepository | undefined {
    return repositories.find((item) => item.installationId + ':' + item.repoId === repo.value);
  }

  function notice(host: HTMLElement, messageText: string): void {
    const p = document.createElement('p');
    p.className = 'hint';
    p.textContent = messageText;
    host.replaceChildren(p);
  }

  function showFields() {
    const op = operation.value;
    const visible = {
      githubProjectName: op === 'clone_repository',
      githubBranch: ['create_branch', 'commit_file', 'open_pull_request'].includes(op),
      githubPath: op === 'commit_file',
      githubMessage: op === 'commit_file',
      githubNumber: ['review_comment', 'mark_ready_for_review'].includes(op),
      githubTitle: op === 'open_pull_request',
      githubContent: ['commit_file', 'open_pull_request', 'review_comment'].includes(op),
    };
    for (const [id, show] of Object.entries(visible)) {
      byId<HTMLElement>(id).classList.toggle('hidden', !show);
      byId<HTMLElement>(id + 'Label').classList.toggle('hidden', !show);
    }
  }

  function renderProposals(items: GitHubProposal[]) {
    proposalsHost.replaceChildren();
    proposalCount.textContent = String(items.length);
    if (!items.length) {
      notice(proposalsHost, 'No pending proposals for this repository.');
      return;
    }
    for (const item of items) {
      const details = document.createElement('details');
      details.className = 'github-proposal';
      const summary = document.createElement('summary');
      summary.textContent = item.operation.replaceAll('_', ' ') + ' · ' + item.source;
      const preview = document.createElement('pre');
      preview.textContent = JSON.stringify(item.params, null, 2);
      preview.className = 'github-proposal-preview';
      const actions = document.createElement('div');
      actions.className = 'github-proposal-actions';
      for (const decision of ['approve', 'reject'] as const) {
        const button = document.createElement('button');
        button.type = 'button';
        button.className = 'button ' + (decision === 'approve' ? 'primary' : 'ghost') + ' compact';
        button.textContent = decision === 'approve' ? 'Approve write…' : 'Reject';
        button.addEventListener('click', () => {
          const active = selected();
          if (!active || !paired || !window.confirm(
            (decision === 'approve' ? 'Execute this exact GitHub write?' : 'Reject this proposal?')
            + '\n\n' + item.operation + '\n' + JSON.stringify(item.params, null, 2).slice(0, 1000),
          )) return;
          button.disabled = true;
          void invoke<{ status: string }>('decide_github_proposal', { id: item.id, decision })
            .then((result) => {
              const output = result as { status: string; result?: { path?: string } };
              setStatus(output.result?.path
                ? 'Repository cloned at ' + output.result.path + '. Open it using Open folder…'
                : 'GitHub proposal ' + result.status + '.');
              return refreshCurrent();
            })
            .catch((error) => {
              setStatus('GitHub decision failed: ' + String(error));
              button.disabled = false;
            });
        });
        actions.append(button);
      }
      details.append(summary, preview, actions);
      proposalsHost.append(details);
    }
  }

  function renderQueue(queue: GitHubQueue, selectedRepo: GitHubRepository) {
    list.replaceChildren();
    const pulls = Array.isArray(queue.pulls) ? queue.pulls.slice(0, 30) : [];
    const issues = Array.isArray(queue.issues) ? queue.issues.slice(0, 30) : [];
    count.textContent = String(pulls.length + issues.length);
    for (const [kind, items] of [['Pull requests', pulls], ['Issues', issues]] as const) {
      const heading = document.createElement('h4');
      heading.textContent = kind + ' (' + items.length + ')';
      list.append(heading);
      if (!items.length) {
        const p = document.createElement('p');
        p.className = 'hint';
        p.textContent = 'No open ' + kind.toLowerCase() + '.';
        list.append(p);
      }
      for (const item of items) {
        const row = document.createElement('div');
        row.className = 'github-queue-item';
        const link = document.createElement('a');
        const url = safeGithubUrl(item.html_url);
        link.textContent = '#' + item.number + ' ' + item.title + (item.draft ? ' · draft' : '');
        if (url) {
          link.href = url;
          link.target = '_blank';
          link.rel = 'noopener noreferrer';
          row.append(link);
        } else {
          const span = document.createElement('span');
          span.textContent = link.textContent;
          row.append(span);
        }
        const action = document.createElement('button');
        action.type = 'button';
        action.className = 'button ghost compact';
        if (kind === 'Issues') {
          const job = jobs.find((record) => record.issue === item.number);
          const marker = document.createElement('span');
          marker.className = 'hint';
          marker.textContent = job ? 'Ryoko: ' + job.status
            + (job.number ? ' · draft PR #' + job.number : '') : 'Not queued';
          row.append(marker);
          action.textContent = 'Run agent…';
          action.disabled = Boolean(job) || !paired;
          action.addEventListener('click', () => {
            if (!paired || !window.confirm(
              'Start a Ryoko engineering job for issue #' + item.number
              + '? This can consume model tokens and open a draft PR. No merge will occur.',
            )) return;
            action.disabled = true;
            void invoke('start_github_job', {
              installationId: selectedRepo.installationId,
              repoId: selectedRepo.repoId,
              number: item.number,
            }).then(() => {
              setStatus('Ryoko job queued for issue #' + item.number);
              return refreshCurrent();
            }).catch((error) => {
              setStatus('Could not start issue: ' + String(error));
              action.disabled = false;
            });
          });
        } else {
          action.textContent = 'Inspect CI';
          action.addEventListener('click', () => {
            action.disabled = true;
            void invoke<GitHubChecks>('get_github_checks', {
              installationId: selectedRepo.installationId,
              repoId: selectedRepo.repoId,
              number: item.number,
            }).then((result) => {
              const messageText = result.ready ? 'Ready for review: CI verified' : 'CI gate blocked';
              setStatus(messageText + ' · ' + result.reason);
              const info = document.createElement('p');
              info.className = 'hint';
              info.textContent = messageText + ' · ' + result.reason;
              row.append(info);
              if (result.ready) {
                operation.value = 'mark_ready_for_review';
                number.value = String(item.number);
                showFields();
              }
            }).catch((error) => setStatus('CI check unavailable: ' + String(error)))
              .finally(() => { action.disabled = false; });
          });
        }
        row.append(action);
        list.append(row);
      }
    }
  }

  async function refreshCurrent(): Promise<void> {
    const current = selected();
    if (!paired || !current) return;
    const serial = ++refreshSerial;
    count.textContent = 'loading';
    try {
      const [queue, currentJobs, pending] = await Promise.all([
        invoke<GitHubQueue>('get_github_queue', {
          installationId: current.installationId, repoId: current.repoId,
        }),
        invoke<GitHubJob[]>('get_github_jobs', {
          installationId: current.installationId, repoId: current.repoId,
        }),
        invoke<GitHubProposal[]>('list_github_proposals', {
          installationId: current.installationId, repoId: current.repoId,
        }),
      ]);
      if (serial !== refreshSerial || selected() !== current || !paired) return;
      jobs = currentJobs;
      renderQueue(queue, current);
      renderProposals(pending);
    } catch (error) {
      if (serial !== refreshSerial) return;
      count.textContent = 'unavailable';
      notice(list, 'GitHub data unavailable: ' + String(error));
    }
  }

  async function loadRepositories(): Promise<void> {
    repositories = await invoke<GitHubRepository[]>('list_github_repositories');
    repo.replaceChildren();
    for (const item of repositories) {
      const option = document.createElement('option');
      option.value = item.installationId + ':' + item.repoId;
      option.textContent = item.name;
      repo.append(option);
    }
    if (!repositories.length) {
      count.textContent = '0';
      notice(list, 'No repositories installed with the Ryoko GitHub App.');
      return;
    }
    await refreshCurrent();
  }

  queueProposal.addEventListener('click', () => {
    const current = selected();
    if (!paired || !current) {
      setStatus('Pair the local GitHub broker before proposing writes.');
      return;
    }
    let params: Record<string, unknown>;
    try {
      params = proposalParams(operation.value, {
        projectName: projectName.value, branch: branch.value, path: path.value, message: message.value,
        number: number.value, title: title.value, content: content.value,
      });
    } catch (error) {
      setStatus('Invalid GitHub action: ' + String(error));
      return;
    }
    queueProposal.disabled = true;
    void invoke('create_github_proposal', {
      installationId: current.installationId, repoId: current.repoId,
      operation: operation.value, params,
    }).then(() => {
      setStatus('GitHub action queued. Expand its full parameters before approving.');
      return refreshCurrent();
    }).catch((error) => setStatus('Proposal rejected: ' + String(error)))
      .finally(() => { queueProposal.disabled = false; });
  });
  operation.addEventListener('change', showFields);
  showFields();

  pair.addEventListener('click', () => {
    const submitted = code.value.trim();
    if (!submitted) return;
    pair.disabled = true;
    void invoke('pair_github_queue', { code: submitted })
      .then(async () => {
        code.value = '';
        paired = true;
        count.textContent = 'connected';
        setStatus('GitHub paired. Writes require a separate review and approval.');
        await loadRepositories();
      }).catch((error) => {
        paired = false;
        code.value = '';
        count.textContent = 'not connected';
        notice(list, 'Pairing failed: ' + String(error));
      }).finally(() => { pair.disabled = false; });
  });
  refresh.addEventListener('click', () => {
    if (!paired) {
      notice(list, 'Pair GitHub in the browser workbench first.');
      return;
    }
    void loadRepositories().catch((error) => setStatus('GitHub refresh failed: ' + String(error)));
  });
  repo.addEventListener('change', () => { void refreshCurrent(); });
  unpair.addEventListener('click', () => {
    void invoke('disconnect_github_queue').then(() => {
      paired = false;
      refreshSerial++;
      jobs = [];
      repositories = [];
      repo.replaceChildren();
      count.textContent = 'not connected';
      proposalCount.textContent = '0';
      notice(proposalsHost, 'Disconnected.');
      notice(list, 'GitHub disconnected.');
    }).catch((error) => setStatus('Disconnect failed: ' + String(error)));
  });
  document.addEventListener('visibilitychange', () => {
    if (paired && !document.hidden) void refreshCurrent();
  });
}
