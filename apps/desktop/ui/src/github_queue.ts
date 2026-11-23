/** Native Tauri GitHub integration over the paired App broker.
 *
 * Read-only views (branches, issues, PRs, checks, file trees, PR diffs) and an
 * approval-first write flow: a write is queued as a broker proposal that
 * expires after 10 minutes, restated with its exact scope, and executes at
 * most once. OAuth and bridge tokens never enter this UI. */
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
interface GitHubBranch {
  name: string;
  sha?: string;
  protected?: boolean;
}
interface GitHubCheck {
  id: number;
  name: string;
  status?: string;
  conclusion?: string | null;
  branch?: string;
}
interface GitHubTreeFile {
  path: string;
  size?: number;
}
interface GitHubPrFile {
  path: string;
  status: string;
  additions: number;
  deletions: number;
  patch: string;
}
interface GitHubFileContent {
  path: string;
  sha: string;
  content: string;
}
interface GitHubViewEnvelope {
  view: string;
  data: unknown;
}
export interface GitHubProposal {
  id: string;
  source: string;
  status: string;
  operation: string;
  installationId: number;
  repoId: number;
  params: Record<string, unknown>;
  createdAt?: string;
  expiresInSeconds: number;
}
interface GitHubDecision {
  outcome: string;
  detail: string;
}

export type ProposalOperation =
  | 'create_branch'
  | 'commit_file'
  | 'open_pull_request'
  | 'review_comment';

export const PROPOSAL_OPERATIONS: readonly ProposalOperation[] = [
  'create_branch',
  'commit_file',
  'open_pull_request',
  'review_comment',
];

export function safeGithubUrl(value: string): string | null {
  try {
    const url = new URL(value);
    if (url.protocol !== 'https:' || url.hostname !== 'github.com'
      || !/^\/[A-Za-z0-9_.-]+\/[A-Za-z0-9_.-]+\/(pull|issues)\/[1-9][0-9]*\/?$/.test(url.pathname)) {
      return null;
    }
    return url.origin + url.pathname;
  } catch {
    return null;
  }
}

/** Bounds untrusted GitHub/broker text for plain-text display. */
export function clipScope(value: unknown, max = 160): string {
  const textValue = typeof value === 'string' ? value : value === undefined || value === null ? '' : String(value);
  const collapsed = textValue.split(/\s+/).filter(Boolean).join(' ');
  return collapsed.length > max ? collapsed.slice(0, max) : collapsed;
}

/** Restates exactly what an approved proposal will do, for the scope preview. */
export function describeProposalScope(
  operation: string,
  repoName: string,
  params: Record<string, unknown>,
): string[] {
  const lines: string[] = ['Repository: ' + clipScope(repoName)];
  switch (operation) {
    case 'create_branch':
      lines.push('Create Git branch: ' + clipScope(params.branch));
      lines.push('Based on the repository default branch; no code is modified.');
      break;
    case 'commit_file':
      lines.push('Commit file: ' + clipScope(params.path));
      lines.push('On branch: ' + clipScope(params.branch));
      lines.push('Commit message: ' + clipScope(params.message));
      lines.push(
        'Content: '
        + (typeof params.content === 'string' ? params.content.length : 0)
        + ' bytes of UTF-8 text',
      );
      break;
    case 'open_pull_request':
      lines.push('Pull request head branch: ' + clipScope(params.head));
      lines.push('Target: the repository default branch; nothing merges automatically');
      lines.push('Title: ' + clipScope(params.title));
      lines.push(
        'Body: ' + (typeof params.body === 'string' ? params.body.length : 0) + ' characters',
      );
      break;
    case 'review_comment':
      lines.push('Comment on pull request #' + String(params.number ?? ''));
      lines.push('Comment text: ' + clipScope(params.comment));
      break;
    default:
      lines.push('Unsupported operation; nothing will be executed');
  }
  lines.push('One-time execution: no auto-merge, default-branch push, workflow or admin change.');
  return lines;
}

/** Renders a CI check state without ever fabricating a pass. */
export function checkLabel(check: { status?: string; conclusion?: string | null }): string {
  if (check.status === 'completed' && check.conclusion) {
    return 'completed · ' + check.conclusion;
  }
  return (check.status || 'unknown') + ' · no conclusion yet';
}

/** Validates the sidebar form fields for one operation; broker remains authoritative. */
export function buildProposalParams(
  operation: string,
  fields: Map<string, string>,
): { params: Record<string, unknown> | null; error: string | null } {
  const value = (key: string): string => (fields.get(key) ?? '').trim();
  switch (operation) {
    case 'create_branch': {
      if (!value('branch')) return { params: null, error: 'enter a ryoko/ branch name' };
      if (value('branch').length > 120) return { params: null, error: 'branch name is too long' };
      return { params: { branch: value('branch') }, error: null };
    }
    case 'commit_file': {
      const content = fields.get('content') ?? '';
      if (!nonEmpty([value('branch'), value('path'), value('message')])) {
        return { params: null, error: 'branch, path and commit message are required' };
      }
      if (new TextEncoder().encode(content).length > 100_000) {
        return { params: null, error: 'file content exceeds 100,000 bytes' };
      }
      return {
        params: { branch: value('branch'), path: value('path'), message: value('message'), content },
        error: null,
      };
    }
    case 'open_pull_request': {
      if (!nonEmpty([value('head'), value('title')])) {
        return { params: null, error: 'head branch and title are required' };
      }
      if ((fields.get('body') ?? '').length > 12_000) {
        return { params: null, error: 'pull request body exceeds 12,000 characters' };
      }
      return {
        params: { head: value('head'), title: value('title'), body: fields.get('body') ?? '' },
        error: null,
      };
    }
    case 'review_comment': {
      const number = Number(value('number'));
      if (!Number.isInteger(number) || number <= 0 || !/^[1-9][0-9]*$/.test(value('number'))) {
        return { params: null, error: 'enter a valid pull request number' };
      }
      if (!value('comment')) return { params: null, error: 'enter a comment' };
      return { params: { number, comment: value('comment') }, error: null };
    }
    default:
      return { params: null, error: 'unsupported operation' };
  }
}

function nonEmpty(values: string[]): boolean {
  return values.every((value) => value.length > 0);
}

function text(value: unknown): string {
  return typeof value === 'string' ? value : '';
}

function byId<T extends HTMLElement>(id: string): T {
  const item = document.getElementById(id);
  if (!item) throw new Error('Missing GitHub queue element ' + id);
  return item as T;
}

function el(tag: string, className?: string, content?: string): HTMLElement {
  const node = document.createElement(tag);
  if (className) node.className = className;
  if (content !== undefined) node.textContent = content;
  return node;
}

export function mountGithubQueue(setStatus: (value: string) => void): void {
  const code = byId<HTMLInputElement>('githubPairCode');
  const pair = byId<HTMLButtonElement>('githubPair');
  const repo = byId<HTMLSelectElement>('githubRepo');
  const refresh = byId<HTMLButtonElement>('githubRefresh');
  const unpair = byId<HTMLButtonElement>('githubUnpair');
  const count = byId<HTMLElement>('githubQueueCount');
  const list = byId<HTMLElement>('githubQueueList');
  const explorerToolbar = byId<HTMLElement>('githubExploreToolbar');
  const exploreExtra = byId<HTMLElement>('githubExploreExtra');
  const branchesButton = byId<HTMLButtonElement>('githubBranches');
  const checksButton = byId<HTMLButtonElement>('githubChecks');
  const treeButton = byId<HTMLButtonElement>('githubTree');
  const diffNumber = byId<HTMLInputElement>('githubDiffNumber');
  const diffButton = byId<HTMLButtonElement>('githubDiffButton');
  const filePath = byId<HTMLInputElement>('githubFilePath');
  const fileButton = byId<HTMLButtonElement>('githubFileButton');
  const explorer = byId<HTMLElement>('githubExplorer');
  const operation = byId<HTMLSelectElement>('githubOperation');
  const fields = byId<HTMLElement>('githubOperationFields');
  const scopePreview = byId<HTMLElement>('githubScopePreview');
  const reviewScope = byId<HTMLButtonElement>('githubReviewScope');
  const queueProposal = byId<HTMLButtonElement>('githubQueueProposal');
  const refreshProposals = byId<HTMLButtonElement>('githubRefreshProposals');
  const proposalList = byId<HTMLElement>('githubProposalList');
  const writePanel = byId<HTMLElement>('githubWritePanel');

  let repositories: GitHubRepository[] = [];
  let paired = false;
  let reviewedParams: Record<string, unknown> | null = null;

  function selected(): GitHubRepository | null {
    return repositories.find(
      (item) => String(item.installationId) + ':' + String(item.repoId) === repo.value,
    ) ?? null;
  }

  function showNotice(target: HTMLElement, notice: string) {
    const p = document.createElement('p');
    p.className = 'hint';
    p.textContent = notice;
    target.replaceChildren(p);
  }

  function errorMessage(error: unknown): string {
    return error instanceof Error ? error.message : String(error);
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
        showNotice(list, 'No open ' + title.toLowerCase() + '.');
        continue;
      }
      for (const item of items) {
        const row = document.createElement('div');
        row.className = 'github-queue-item';
        const label = '#' + item.number + ' ' + item.title + (item.draft ? ' · draft' : '');
        const url = safeGithubUrl(item.html_url);
        if (url) {
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

  function renderBranches(data: unknown) {
    const branches = (Array.isArray(data) ? data : []) as GitHubBranch[];
    explorer.replaceChildren();
    explorer.append(el('div', 'github-inspect-title', 'Branches (' + branches.length + ')'));
    if (!branches.length) showNotice(explorer, 'No branches reported.');
    for (const branch of branches.slice(0, 30)) {
      explorer.append(el(
        'div',
        'github-queue-item',
        clipScope(branch.name) + (branch.protected ? ' · protected' : ''),
      ));
    }
    explorer.classList.remove('hidden');
  }

  function renderChecks(data: unknown) {
    const checks = (Array.isArray(data) ? data : []) as GitHubCheck[];
    explorer.replaceChildren();
    explorer.append(el('div', 'github-inspect-title', 'Actions / CI (' + checks.length + ')'));
    if (!checks.length) {
      showNotice(explorer, 'No workflow runs reported for this repository.');
    }
    for (const check of checks.slice(0, 15)) {
      explorer.append(el(
        'div',
        'github-queue-item',
        (clipScope(check.name) || 'workflow run')
        + (check.branch ? ' · ' + clipScope(check.branch, 60) : '')
        + ' · ' + checkLabel(check),
      ));
    }
    explorer.classList.remove('hidden');
  }

  function renderTree(data: unknown) {
    const payload = (data ?? {}) as { files?: GitHubTreeFile[]; truncated?: boolean };
    const files = Array.isArray(payload.files) ? payload.files : [];
    explorer.replaceChildren();
    explorer.append(el(
      'div',
      'github-inspect-title',
      'File tree (' + files.length + (payload.truncated ? ', truncated' : '') + ')',
    ));
    if (!files.length) showNotice(explorer, 'No files reported.');
    for (const file of files.slice(0, 300)) {
      explorer.append(el(
        'div',
        'github-queue-item',
        clipScope(file.path, 200)
        + (typeof file.size === 'number' ? ' (' + file.size + ' bytes)' : ''),
      ));
    }
    explorer.classList.remove('hidden');
  }

  function renderFileView(data: unknown) {
    const file = (data ?? {}) as GitHubFileContent;
    explorer.replaceChildren();
    explorer.append(el(
      'div',
      'github-inspect-title',
      clipScope(file.path, 200) + (file.sha ? ' @ ' + clipScope(file.sha, 12) : ''),
    ));
    const content = document.createElement('pre');
    content.className = 'github-patch';
    content.textContent = text(file.content) || '(empty file)';
    explorer.append(content);
    explorer.classList.remove('hidden');
  }

  function renderPrDiff(data: unknown) {
    const payload = (data ?? {}) as { files?: GitHubPrFile[] };
    const files = Array.isArray(payload.files) ? payload.files : [];
    explorer.replaceChildren();
    explorer.append(el(
      'div',
      'github-inspect-title',
      'PR #' + diffNumber.value + ' file changes (' + files.length + ')',
    ));
    if (!files.length) {
      showNotice(explorer, 'No file changes reported for this pull request.');
    }
    for (const file of files) {
      explorer.append(el(
        'div',
        'github-inspect-title',
        clipScope(file.path, 200) + ' · ' + clipScope(file.status, 20)
        + ' +' + file.additions + ' −' + file.deletions,
      ));
      const patch = document.createElement('pre');
      patch.className = 'github-patch';
      patch.textContent = typeof file.patch === 'string' && file.patch.length
        ? file.patch : '(no textual patch: binary or oversized change)';
      explorer.append(patch);
    }
    explorer.classList.remove('hidden');
  }

  async function loadView(view: string, extra: Record<string, string> = {}): Promise<void> {
    const current = selected();
    if (!current || !paired) return;
    explorer.replaceChildren(el('p', 'hint', 'Loading ' + view + '…'));
    explorer.classList.remove('hidden');
    try {
      const envelope = await invoke<GitHubViewEnvelope>('get_github_view', {
        installationId: current.installationId,
        repoId: current.repoId,
        view,
        path: extra.path ?? null,
        refName: extra.ref ?? null,
        number: extra.number ? Number(extra.number) : null,
      });
      if (envelope.view === 'branches') renderBranches(envelope.data);
      else if (envelope.view === 'checks') renderChecks(envelope.data);
      else if (envelope.view === 'tree') renderTree(envelope.data);
      else if (envelope.view === 'pr_diff') renderPrDiff(envelope.data);
      else if (envelope.view === 'file') renderFileView(envelope.data);
    } catch (error) {
      showNotice(explorer, view + ' unavailable: ' + errorMessage(error));
    }
  }

  function renderOperationFields(): void {
    reviewedParams = null;
    scopePreview.replaceChildren();
    queueProposal.disabled = true;
    const chosen = operation.value;
    const input = (key: string, placeholder: string): HTMLInputElement => {
      const field = document.createElement('input');
      field.dataset.githubField = key;
      field.placeholder = placeholder;
      field.spellcheck = false;
      field.autocomplete = 'off';
      return field;
    };
    const area = (key: string, placeholder: string, rows: number): HTMLTextAreaElement => {
      const field = document.createElement('textarea');
      field.dataset.githubField = key;
      field.placeholder = placeholder;
      field.rows = rows;
      field.spellcheck = false;
      return field;
    };
    fields.replaceChildren();
    if (chosen === 'create_branch') {
      fields.append(input('branch', 'ryoko/branch-name'));
    } else if (chosen === 'commit_file') {
      fields.append(input('branch', 'ryoko/branch-name'));
      fields.append(input('path', 'src/file.ext'));
      fields.append(input('message', 'commit message'));
      fields.append(area('content', 'new UTF-8 file content', 4));
    } else if (chosen === 'open_pull_request') {
      fields.append(input('head', 'ryoko/branch-name'));
      fields.append(input('title', 'pull request title'));
      fields.append(area('body', 'pull request body', 3));
    } else if (chosen === 'review_comment') {
      fields.append(input('number', 'pull request number'));
      fields.append(input('comment', 'comment'));
    }
  }

  function collectFields(): Map<string, string> {
    const values = new Map<string, string>();
    for (const node of Array.from(fields.querySelectorAll('[data-github-field]'))) {
      const key = (node as HTMLElement).dataset.githubField;
      if (key) values.set(key, (node as HTMLInputElement).value);
    }
    return values;
  }

  function repoLabel(proposal: GitHubProposal): string {
    return repositories.find(
      (item) => item.installationId === proposal.installationId && item.repoId === proposal.repoId,
    )?.name ?? 'repository #' + proposal.repoId;
  }

  function renderProposals(proposals: GitHubProposal[]): void {
    proposalList.replaceChildren();
    if (!proposals.length) {
      showNotice(proposalList, 'No pending proposals for your account.');
      return;
    }
    for (const proposal of proposals) {
      const row = el('div', 'github-proposal');
      const scope = el('div', 'github-proposal-scope');
      scope.textContent = describeProposalScope(proposal.operation, repoLabel(proposal), proposal.params).join('\n');
      const meta = el('div', 'github-proposal-meta');
      meta.textContent = proposal.operation + ' · ' + (proposal.source || 'desktop')
        + ' · expires in ' + proposal.expiresInSeconds + 's';
      const actions = el('div', 'github-proposal-actions');
      const approve = el('button', 'button primary compact', 'Approve once…') as HTMLButtonElement;
      approve.disabled = proposal.status !== 'pending';
      approve.addEventListener('click', () => { void decideProposal(proposal, true); });
      const reject = el('button', 'button ghost compact', 'Reject') as HTMLButtonElement;
      reject.disabled = proposal.status !== 'pending';
      reject.addEventListener('click', () => { void decideProposal(proposal, false); });
      actions.append(approve, reject);
      row.append(scope, meta, actions);
      proposalList.append(row);
    }
  }

  async function refreshProposalsList(): Promise<void> {
    if (!paired) return;
    try {
      const proposals = await invoke<GitHubProposal[]>('list_github_proposals');
      renderProposals(proposals.slice(0, 20));
    } catch (error) {
      showNotice(proposalList, 'Pending proposals unavailable: ' + errorMessage(error));
    }
  }

  async function decideProposal(proposal: GitHubProposal, approve: boolean): Promise<void> {
    const shared = {
      id: proposal.id,
      installationId: proposal.installationId,
      repoId: proposal.repoId,
    };
    if (!approve) {
      try {
        const decision = await invoke<GitHubDecision>('decide_github_proposal', {
          ...shared,
          approve: false,
        });
        setStatus('Proposal ' + decision.outcome + ': ' + decision.detail);
      } catch (error) {
        setStatus('Rejection failed: ' + errorMessage(error));
      }
      await refreshProposalsList();
      return;
    }
    try {
      const decision = await invoke<GitHubDecision>('decide_github_proposal', {
        ...shared,
        approve: true,
      });
      setStatus('GitHub write ' + decision.outcome + ': ' + decision.detail);
    } catch (error) {
      const message = errorMessage(error);
      setStatus(message.includes('cancelled') ? message : 'GitHub write refused: ' + message);
    }
    await refreshProposalsList();
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
      showNotice(list, 'No GitHub App installations with permitted repositories.');
      count.textContent = '0';
      explorerToolbar.classList.add('hidden');
      exploreExtra.classList.add('hidden');
      writePanel.classList.add('hidden');
      return;
    }
    explorerToolbar.classList.remove('hidden');
    exploreExtra.classList.remove('hidden');
    writePanel.classList.remove('hidden');
    await loadQueue();
    await refreshProposalsList();
  }

  async function loadQueue(): Promise<void> {
    if (!paired) return;
    const current = selected();
    if (!current) return;
    count.textContent = 'loading';
    try {
      const queue = await invoke<GitHubQueue>('get_github_queue', {
        installationId: current.installationId,
        repoId: current.repoId,
      });
      renderQueue(queue);
    } catch (error) {
      count.textContent = 'unavailable';
      showNotice(list, 'GitHub queue unavailable: ' + errorMessage(error));
    }
  }

  pair.addEventListener('click', () => {
    const submitted = code.value.trim();
    if (!submitted) return;
    pair.disabled = true;
    void (async () => {
      try {
        await invoke('pair_github_queue', { code: submitted });
        code.value = '';
        paired = true;
        count.textContent = 'connected';
        explorerToolbar.classList.remove('hidden');
        writePanel.classList.remove('hidden');
        setStatus('GitHub connected: read-only views plus approval-first proposals.');
        await loadRepositories();
      } catch (error) {
        paired = false;
        code.value = '';
        count.textContent = 'not connected';
        explorerToolbar.classList.add('hidden');
        exploreExtra.classList.add('hidden');
        writePanel.classList.add('hidden');
        showNotice(list, 'Pairing failed: ' + errorMessage(error));
      } finally {
        pair.disabled = false;
      }
    })();
  });

  refresh.addEventListener('click', () => {
    void (async () => {
      if (!paired) {
        showNotice(list, 'Pair GitHub first using a code generated by the browser workbench.');
        return;
      }
      try {
        await loadRepositories();
      } catch (error) {
        showNotice(list, 'Cannot refresh repositories: ' + errorMessage(error));
      }
    })();
  });

  repo.addEventListener('change', () => {
    void loadQueue();
    void refreshProposalsList();
  });

  branchesButton.addEventListener('click', () => { void loadView('branches'); });
  checksButton.addEventListener('click', () => { void loadView('checks'); });
  treeButton.addEventListener('click', () => { void loadView('tree'); });
  diffButton.addEventListener('click', () => {
    const number = diffNumber.value.trim();
    if (!/^[1-9][0-9]*$/.test(number)) {
      showNotice(explorer, 'Enter a pull request number first.');
      explorer.classList.remove('hidden');
      return;
    }
    void loadView('pr_diff', { number });
  });
  fileButton.addEventListener('click', () => {
    const path = filePath.value.trim();
    if (!path || path.length > 250) {
      showNotice(explorer, 'Enter a repository-relative file path first.');
      explorer.classList.remove('hidden');
      return;
    }
    void loadView('file', { path });
  });

  operation.addEventListener('change', renderOperationFields);

  reviewScope.addEventListener('click', () => {
    const current = selected();
    if (!current) return;
    const op = operation.value;
    const built = buildProposalParams(op, collectFields());
    if (built.error || !built.params) {
      scopePreview.replaceChildren(el(
        'span',
        'hint',
        built.error ?? 'operation is incomplete',
      ));
      queueProposal.disabled = true;
      reviewedParams = null;
      return;
    }
    reviewedParams = built.params;
    scopePreview.replaceChildren();
    const pre = document.createElement('pre');
    pre.className = 'github-patch';
    pre.textContent = describeProposalScope(op, current.name, built.params).join('\n');
    scopePreview.append(pre);
    queueProposal.disabled = false;
  });

  queueProposal.addEventListener('click', () => {
    const current = selected();
    if (!current || !reviewedParams) return;
    queueProposal.disabled = true;
    void (async () => {
      try {
        const proposal = await invoke<GitHubProposal>('queue_github_proposal', {
          installationId: current.installationId,
          repoId: current.repoId,
          operation: operation.value,
          params: reviewedParams,
        });
        setStatus('Queued pending proposal ' + proposal.id.slice(0, 8)
          + '; approve it from the list to run it once.');
        reviewedParams = null;
        scopePreview.replaceChildren();
        renderOperationFields();
      } catch (error) {
        setStatus('Proposal refused: ' + errorMessage(error));
        queueProposal.disabled = true;
      }
      await refreshProposalsList();
    })();
  });

  refreshProposals.addEventListener('click', () => { void refreshProposalsList(); });

  unpair.addEventListener('click', () => {
    void invoke('disconnect_github_queue').then(() => {
      paired = false;
      repositories = [];
      repo.replaceChildren();
      count.textContent = 'not connected';
      explorerToolbar.classList.add('hidden');
      exploreExtra.classList.add('hidden');
      writePanel.classList.add('hidden');
      explorer.replaceChildren();
      showNotice(list, 'GitHub access disconnected.');
    }).catch((error) => { setStatus('Disconnect error: ' + errorMessage(error)); });
  });

  renderOperationFields();
}
