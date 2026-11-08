const $ = (id) => document.getElementById(id);

const ROLES = [
  { id: 'project-manager', title: 'Project Manager', icon: '▣', detail: 'Product scope & requirements' },
  { id: 'software-architect', title: 'Software Architect', icon: '⌘', detail: 'Systems & interfaces' },
  { id: 'ui-ux-designer', title: 'UI/UX Engineer', icon: '◧', detail: 'Interaction & visual design' },
  { id: 'backend-engineer', title: 'Backend Engineer', icon: '⌑', detail: 'APIs & data architecture' },
  { id: 'senior-developer', title: 'Senior Developer', icon: '⌨', detail: 'Implementation & integration' },
  { id: 'security-engineer', title: 'Security Engineer', icon: '◈', detail: 'Threat modeling' },
  { id: 'qa-engineer', title: 'QA Engineer', icon: '◇', detail: 'Tests & release quality' },
  { id: 'devops-engineer', title: 'DevOps / SRE', icon: '⬡', detail: 'Delivery & operations' },
];

const VIEWS = {
  workbench: ['DEVELOPMENT ENVIRONMENT', 'Workbench', 'Inspect code and coordinate safe engineering work.'],
  mission: ['ENGINEERING OPERATIONS', 'Agent Mission Control', 'View real worktree status and authenticated GitHub events.'],
  github: ['REPOSITORY INTELLIGENCE', 'GitHub', 'Branches, issues, pull requests and workflows in one place.'],
  approvals: ['CHANGE GOVERNANCE', 'Approvals', 'Review exact GitHub operations before executing them.'],
  settings: ['CONFIGURATION', 'Settings', 'Connection status, permissions and setup.'],
};

const state = {
  session: null, installationId: null, repoId: null, repositories: [], files: [], events: [],
  mission: { connected: false, sessions: [] }, selectedPath: '', generation: 0, stream: null,
};

function status(message) { $('statusMessage').textContent = String(message); }
function errorMessage(error) { status('Error: ' + error.message); }
function node(tag, className, text) {
  const element = document.createElement(tag);
  if (className) element.className = className;
  if (text !== undefined) element.textContent = String(text);
  return element;
}
function empty(container, message) {
  container.replaceChildren(node('div', 'empty', message));
}
function option(value, label) {
  const item = document.createElement('option');
  item.value = String(value);
  item.textContent = String(label);
  return item;
}
function linkIfTrusted(item, text) {
  try {
    const url = new URL(item);
    if (url.protocol !== 'https:' || url.hostname !== 'github.com') throw Error('unsafe link');
    const element = node('a', 'entry-title', text);
    element.href = url.href;
    element.target = '_blank';
    element.rel = 'noopener noreferrer';
    return element;
  } catch {
    return node('span', 'entry-title', text);
  }
}
async function api(path, options = {}) {
  const headers = { Accept: 'application/json' };
  if (options.body !== undefined) headers['Content-Type'] = 'application/json';
  if (options.method && options.method !== 'GET' && state.session?.csrf) {
    headers['X-Ryoko-CSRF'] = state.session.csrf;
  }
  const response = await fetch(path, {
    method: options.method || 'GET', headers, credentials: 'same-origin',
    ...(options.body !== undefined ? { body: JSON.stringify(options.body) } : {}),
  });
  const result = await response.json();
  if (!response.ok) throw new Error(result.error || 'Request failed (' + response.status + ')');
  return result;
}

function selectView(view) {
  if (!VIEWS[view]) return;
  document.querySelectorAll('.view').forEach((element) => {
    element.classList.toggle('active', element.id === 'view-' + view);
  });
  document.querySelectorAll('[data-view]').forEach((element) => {
    element.classList.toggle('active', element.dataset.view === view);
    if (element.dataset.view === view) element.setAttribute('aria-current', 'page');
    else element.removeAttribute('aria-current');
  });
  $('viewEyebrow').textContent = VIEWS[view][0];
  $('viewTitle').textContent = VIEWS[view][1];
  $('viewDescription').textContent = VIEWS[view][2];
  if (view === 'mission') refreshMission().catch(errorMessage);
  if (view === 'approvals') refreshProposals().catch(errorMessage);
}

function selectedRepo() {
  return state.repositories.find((item) => item.id === state.repoId) || null;
}
function selectedQuery(view, extra = {}) {
  const args = new URLSearchParams({
    installationId: String(state.installationId),
    repoId: String(state.repoId),
    view,
    ...extra,
  });
  return '/api/repository?' + args.toString();
}
function setAuthenticated(session) {
  const connected = Boolean(session.connected);
  $('connectionState').textContent = connected ? session.user.login : 'Not connected';
  $('connectButton').classList.toggle('hidden', connected || !session.configured);
  $('disconnectButton').classList.toggle('hidden', !connected);
  $('installLink').classList.toggle('hidden', !connected || !session.installUrl);
  if (session.installUrl) $('installLink').href = session.installUrl;
  $('inspectAuth').textContent = connected ? 'Connected as ' + session.user.login : 'Disconnected';
  $('onlineDot').classList.toggle('online', connected);
  $('accountDetails').textContent = connected
    ? 'Authenticated as ' + session.user.login + '. Tokens are held only by the trusted server.'
    : session.configured ? 'Sign in to connect your GitHub installations.' : 'GitHub App server is not configured yet.';
  $('workspaceBadge').textContent = connected ? 'GitHub connected' : 'Offline preview';
}

async function loadInstallations() {
  const installations = await api('/api/installations');
  const select = $('installationSelect');
  select.replaceChildren(option('', installations.length ? 'Choose installation' : 'No GitHub App installation'));
  installations.forEach((item) => select.append(option(item.id, item.account)));
  if (installations.length) {
    state.installationId = installations[0].id;
    select.value = String(state.installationId);
    await loadRepositories();
  } else {
    state.installationId = null;
    empty($('fileList'), 'Install the GitHub App on selected repositories to get started.');
  }
}

async function loadRepositories() {
  state.repoId = null;
  state.repositories = [];
  const select = $('repoSelect');
  select.replaceChildren(option('', 'Choose repository'));
  if (!state.installationId) return;
  const repos = await api('/api/repositories?installationId=' + encodeURIComponent(state.installationId));
  state.repositories = repos;
  repos.forEach((item) => select.append(option(item.id, item.full_name + (item.private ? '  · private' : ''))));
  $('repoCounter').textContent = repos.length + ' repos';
  if (repos.length) {
    state.repoId = repos[0].id;
    select.value = String(state.repoId);
    await loadRepositoryData();
  }
}
function renderFileTree() {
  const list = $('fileList');
  const query = $('fileSearch').value.trim().toLowerCase();
  const files = state.files.filter((item) => item.path.toLowerCase().includes(query)).slice(0, 220);
  list.replaceChildren();
  $('fileCounter').textContent = state.files.length + ' files';
  if (!files.length) return empty(list, 'No matching files.');
  for (const file of files) {
    const button = node('button', 'file-link' + (state.selectedPath === file.path ? ' active' : ''), '◦ ' + file.path);
    button.type = 'button';
    button.title = file.path;
    button.addEventListener('click', () => openFile(file.path).catch(errorMessage));
    list.append(button);
  }
}

async function openFile(path) {
  if (!state.repoId) return;
  const previousRepo = state.repoId;
  status('Loading ' + path + '…');
  const file = await api(selectedQuery('file', { path }));
  if (state.repoId !== previousRepo) return;
  $('codeViewer').textContent = file.content;
  $('filePath').textContent = file.path;
  $('editorLanguage').textContent = (path.split('.').pop() || 'text').toUpperCase();
  state.selectedPath = path;
  renderFileTree();
  selectView('workbench');
  status('Read-only file: ' + path);
}

function renderCollection(view, data, containerId, countId) {
  const target = $(containerId);
  $(countId).textContent = data.length;
  target.replaceChildren();
  if (!data.length) return empty(target, 'Nothing to show yet.');
  for (const item of data) {
    const card = node('div', 'item-entry');
    let name = '';
    let meta = '';
    if (view === 'issues' || view === 'pulls') {
      name = '#' + item.number + ' · ' + item.title;
      meta = view === 'pulls' ? (item.draft ? 'Draft · ' : '') + (item.head || '') : item.state;
    } else if (view === 'checks') {
      name = item.name || 'Workflow';
      meta = (item.branch || 'no branch') + ' · ' + (item.conclusion || item.status || 'pending');
    } else {
      name = item.name;
      meta = item.protected ? 'Protected branch' : 'Branch';
    }
    card.append(linkIfTrusted(item.html_url, name), node('span', 'entry-meta', meta));
    target.append(card);
  }
}

async function loadRepositoryData() {
  const repo = selectedRepo();
  if (!repo) return;
  const generation = ++state.generation;
  state.selectedPath = '';
  state.files = [];
  $('currentProject').textContent = repo.full_name;
  $('inspectRepo').textContent = repo.full_name;
  $('inspectBranch').textContent = 'Default: ' + repo.default_branch;
  $('codeViewer').textContent = 'Choose a file from the explorer to inspect the current GitHub snapshot.';
  $('filePath').textContent = 'No file open';
  status('Loading GitHub repository: ' + repo.full_name);
  const views = [
    ['pulls', 'prList', 'prCount'], ['issues', 'issueList', 'issueCount'],
    ['checks', 'ciList', 'ciCount'], ['branches', 'branchList', 'branchCount'],
  ];
  const tasks = views.map(async ([view, containerId, countId]) => {
    try {
      const data = await api(selectedQuery(view));
      if (generation === state.generation) renderCollection(view, data, containerId, countId);
    } catch (error) {
      if (generation === state.generation) empty($(containerId), error.message);
    }
  });
  tasks.push((async () => {
    try {
      const result = await api(selectedQuery('tree'));
      if (generation !== state.generation) return;
      state.files = result.files || [];
      renderFileTree();
      if (result.truncated) status('Repository tree was truncated; only part is shown.');
    } catch (error) {
      if (generation === state.generation) empty($('fileList'), error.message);
    }
  })());
  await Promise.all(tasks);
  if (generation === state.generation) status('Repository loaded: ' + repo.full_name);
}

function agentStatus(role, records) {
  // Existing CLI worktrees do not identify all eight specialists. Avoid
  // claiming a role is active without grounded agent metadata.
  const related = records.find((item) => {
    const id = String(item.session?.agent_id || '').toLowerCase();
    if (role.id === 'senior-developer') return id.includes('developer') || id.includes('dev');
    return id.includes(role.id);
  });
  if (!related) return { label: 'No active worktree linked', style: '' };
  if (related.state === 'published') return { label: 'Awaiting human review', style: 'review' };
  if (related.state === 'active') return { label: 'Worktree active', style: 'running' };
  return { label: 'Last: ' + related.state, style: '' };
}

function renderAgents() {
  const grid = $('agentsGrid');
  grid.replaceChildren();
  const records = state.mission.sessions || [];
  for (const role of ROLES) {
    const card = node('article', 'agent-card');
    const icon = node('div', 'agent-icon', role.icon);
    icon.setAttribute('aria-hidden', 'true');
    card.append(icon, node('div', 'agent-title', role.title), node('div', 'agent-caption', role.detail));
    const statusValue = state.mission.connected
      ? agentStatus(role, records) : { label: 'Not connected to engine', style: '' };
    const statusNode = node('div', 'agent-status ' + statusValue.style);
    statusNode.append(node('span', 'dot'), node('span', '', statusValue.label));
    card.append(statusNode);
    grid.append(card);
  }
}

async function refreshMission() {
  if (!state.session?.connected) return;
  state.mission = await api('/api/mission');
  $('missionConnection').textContent = state.mission.connected ? 'Linked' : 'Offline';
  $('missionNote').textContent = state.mission.note || '';
  $('reviewCount').textContent = (state.mission.sessions || [])
    .filter((item) => item.state === 'published').length;
  renderAgents();
}

function renderEvents() {
  const feed = $('eventFeed');
  feed.replaceChildren();
  const selected = selectedRepo();
  const display = state.events.filter((event) => !selected || event.repositoryId === selected.id).slice(0, 45);
  if (!display.length) return empty(feed, 'No verified events for this repository yet.');
  for (const event of display) {
    const row = node('div', 'timeline-item');
    row.append(node('div', 'timeline-icon', '⑂'));
    const body = node('div');
    body.append(node('strong', '', event.repository + ' · ' + event.type + ' / ' + event.action));
    body.append(node('small', '', event.summary + ' · ' + new Date(event.timestamp).toLocaleString()));
    row.append(body);
    feed.append(row);
  }
}

function openEventStream() {
  if (state.stream) state.stream.close();
  if (!state.session?.connected) return;
  state.stream = new EventSource('/api/events');
  state.stream.addEventListener('github', (event) => {
    try {
      const payload = JSON.parse(event.data);
      if (!state.events.some((entry) => entry.id === payload.id)) {
        state.events.unshift(payload);
        state.events = state.events.slice(0, 100);
        renderEvents();
        status('GitHub event: ' + payload.type + ' · ' + payload.repository);
      }
    } catch { status('An unreadable GitHub event was ignored.'); }
  });
  state.stream.onerror = () => status('GitHub webhook stream reconnecting…');
}

function inputField(id, label, { multiline = false, placeholder = '' } = {}) {
  const wrapper = node('div');
  const caption = node('label', '', label);
  caption.htmlFor = id;
  const field = node(multiline ? 'textarea' : 'input');
  field.id = id;
  field.placeholder = placeholder;
  wrapper.append(caption, field);
  return wrapper;
}

function renderProposalFields() {
  const operation = $('operation').value;
  const host = $('proposalFields');
  host.replaceChildren();
  if (operation === 'create_branch') {
    host.append(inputField('branch', 'New branch name', { placeholder: 'ryoko/feature-name' }));
  } else if (operation === 'commit_file') {
    host.append(
      inputField('branch', 'Existing Ryoko feature branch', { placeholder: 'ryoko/feature-name' }),
      inputField('path', 'Relative file path', { placeholder: 'src/module.rs' }),
      inputField('message', 'Commit message', { placeholder: 'feat: add component' }),
      inputField('expectedSha', 'Existing file SHA (required when updating)', { placeholder: '40-digit blob SHA; leave blank to create' }),
      inputField('content', 'UTF-8 file content', { multiline: true, placeholder: 'Complete new file contents' }),
    );
  } else if (operation === 'open_pull_request') {
    host.append(
      inputField('head', 'Feature branch', { placeholder: 'ryoko/feature-name' }),
      inputField('title', 'Pull request title'),
      inputField('body', 'Pull request description', { multiline: true }),
    );
  } else if (operation === 'review_comment') {
    host.append(
      inputField('number', 'Pull request number'),
      inputField('comment', 'Comment', { multiline: true, placeholder: 'Describe the review findings' }),
    );
  }
}

function readProposal() {
  if (!state.repoId || !state.installationId) throw new Error('Select an authorized GitHub repository first.');
  const op = $('operation').value;
  const get = (id) => $(id)?.value || '';
  let params;
  if (op === 'create_branch') params = { branch: get('branch') };
  else if (op === 'commit_file') {
    params = { branch: get('branch'), path: get('path'), content: get('content'), message: get('message') };
    if (get('expectedSha')) params.expectedSha = get('expectedSha');
  } else if (op === 'open_pull_request') {
    params = { head: get('head'), title: get('title'), body: get('body') };
  } else params = { number: Number(get('number')), comment: get('comment') };
  return { installationId: state.installationId, repoId: state.repoId, operation: op, params };
}

function prettyProposal(item) {
  const proposal = item.proposal;
  const format = { create_branch: 'Create branch', commit_file: 'Commit file', open_pull_request: 'Open pull request', review_comment: 'Comment on PR' };
  const root = node('article', 'proposal-card');
  const header = node('header');
  header.append(node('h4', '', format[proposal.operation] || proposal.operation));
  header.append(node('span', 'small-pill', item.status));
  root.append(header);
  root.append(node('p', '', 'Source: ' + item.source + ' · Repository ID: ' + proposal.repoId));
  const preview = node('pre', '', JSON.stringify(proposal.params, null, 2));
  root.append(preview);
  if (item.result) root.append(node('p', '', 'Result: ' + JSON.stringify(item.result)));
  if (item.error) root.append(node('p', 'danger', item.error));
  if (item.status === 'pending') {
    const actions = node('div', 'proposal-actions');
    const approve = node('button', 'button primary', 'Approve and execute');
    const reject = node('button', 'button ghost danger', 'Reject');
    approve.type = reject.type = 'button';
    approve.addEventListener('click', async () => {
      if (!window.confirm('Execute this exact GitHub action? This may write to a feature branch.')) return;
      approve.disabled = true;
      try {
        await api('/api/proposals/' + item.id + '/approve', { method: 'POST' });
        status('GitHub operation executed. Reloading repository data.');
        await Promise.all([refreshProposals(), loadRepositoryData()]);
      } catch (error) { errorMessage(error); await refreshProposals(); }
    });
    reject.addEventListener('click', async () => {
      reject.disabled = true;
      try {
        await api('/api/proposals/' + item.id + '/reject', { method: 'POST' });
        await refreshProposals();
        status('Proposal rejected without GitHub changes.');
      } catch (error) { errorMessage(error); reject.disabled = false; }
    });
    actions.append(approve, reject);
    root.append(actions);
  }
  return root;
}

async function refreshProposals() {
  if (!state.session?.connected) return;
  const data = await api('/api/proposals');
  const list = $('proposalList');
  list.replaceChildren();
  if (!data.length) return empty(list, 'No pending approvals. Create a proposal to request a scoped GitHub change.');
  for (const item of data.slice().reverse()) {
    if (state.repoId && item.proposal.repoId !== state.repoId) continue;
    list.append(prettyProposal(item));
  }
  if (!list.children.length) empty(list, 'No proposals for this repository.');
}

async function initialize() {
  renderAgents();
  renderProposalFields();
  const theme = localStorage.getItem('ryoko-theme');
  if (theme === 'light' || theme === 'dark') document.documentElement.dataset.theme = theme;
  document.querySelectorAll('[data-view]').forEach((button) => {
    button.addEventListener('click', () => selectView(button.dataset.view));
  });
  $('openApprovals').addEventListener('click', () => selectView('approvals'));
  $('themeToggle').addEventListener('click', () => {
    const current = document.documentElement.dataset.theme;
    const next = current === 'dark' ? 'light' : 'dark';
    document.documentElement.dataset.theme = next;
    localStorage.setItem('ryoko-theme', next);
  });
  $('installationSelect').addEventListener('change', async (event) => {
    state.installationId = Number(event.target.value) || null;
    try { await loadRepositories(); } catch (error) { errorMessage(error); }
  });
  $('repoSelect').addEventListener('change', async (event) => {
    state.repoId = Number(event.target.value) || null;
    try { await loadRepositoryData(); renderEvents(); await refreshProposals(); } catch (error) { errorMessage(error); }
  });
  $('fileSearch').addEventListener('input', renderFileTree);
  $('refreshButton').addEventListener('click', () => loadRepositoryData().catch(errorMessage));
  $('refreshGitHub').addEventListener('click', () => loadRepositoryData().catch(errorMessage));
  $('refreshMission').addEventListener('click', () => refreshMission().catch(errorMessage));
  $('refreshProposals').addEventListener('click', () => refreshProposals().catch(errorMessage));
  $('operation').addEventListener('change', renderProposalFields);
  $('proposalForm').addEventListener('submit', async (event) => {
    event.preventDefault();
    try {
      const proposal = readProposal();
      await api('/api/proposals', { method: 'POST', body: proposal });
      status('Proposal queued; review its exact details before approval.');
      await refreshProposals();
      $('proposalForm').reset();
      renderProposalFields();
    } catch (error) { errorMessage(error); }
  });
  $('disconnectButton').addEventListener('click', async () => {
    try {
      await api('/api/disconnect', { method: 'POST' });
      window.location.reload();
    } catch (error) { errorMessage(error); }
  });

  try {
    state.session = await api('/api/session');
    setAuthenticated(state.session);
    if (!state.session.connected) {
      status(state.session.configured ? 'Connect GitHub to start.' : 'GitHub App credentials required. See setup documentation.');
      return;
    }
    await loadInstallations();
    await Promise.all([refreshMission(), refreshProposals()]);
    openEventStream();
    status('GitHub connected. Select a repository to begin.');
  } catch (error) { errorMessage(error); }
}

initialize();
