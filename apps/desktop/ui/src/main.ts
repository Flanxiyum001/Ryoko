import { invoke } from '@tauri-apps/api/core';
import { open } from '@tauri-apps/plugin-dialog';
import * as monaco from 'monaco-editor';
import EditorWorker from 'monaco-editor/esm/vs/editor/editor.worker?worker';
import './styles.css';
import { LspClient, type LspState } from './lsp';
import {
  focusMissionTasks,
  layoutMission,
  missionNodeSize,
  parseMissionLog,
  reduceMissionEvents,
  type MissionSnapshot,
  type MissionTask,
} from './mission';
import {
  fileUri,
  joinWorkspacePath,
  languageForPath,
  markerSeverityFor,
  pathFromFileUri,
  type LspDiagnostic,
  type PublishDiagnosticsParams,
} from './protocol';

interface WorkspaceInfo {
  root: string;
  name: string;
}

interface OpenFile {
  path: string;
  uri: string;
  languageId: string;
  model: monaco.editor.ITextModel;
  version: number;
  savedText: string;
  dirty: boolean;
}

interface FlatProblem {
  uri: string;
  diagnostic: LspDiagnostic;
}

interface GitHubQueueItem {
  number: number;
  title: string;
  url: string;
  kind: 'pull' | 'issues';
  draft: boolean;
}

interface GitHubQueue {
  repository: string;
  pulls: GitHubQueueItem[];
  issues: GitHubQueueItem[];
}

interface MissionLog {
  name: string;
  bytes: number;
  modified_ms: number;
}

interface GitHunk {
  id: number;
  path: string;
  header: string;
  preview: string;
}

interface GitHunkSnapshot {
  patch: string;
  hunks: GitHunk[];
}

(globalThis as unknown as Record<string, unknown>).MonacoEnvironment = {
  getWorker: () => new EditorWorker(),
};

function element<T extends HTMLElement>(id: string): T {
  const found = document.getElementById(id);
  if (!found) throw new Error(`missing element #${id}`);
  return found as T;
}

const openFolderButton = element<HTMLButtonElement>('openFolder');
const saveButton = element<HTMLButtonElement>('saveFile');
const workspaceName = element('workspaceName');
const fileFilter = element<HTMLInputElement>('fileFilter');
const fileList = element('fileList');
const fileCount = element('fileCount');
const activePath = element('activePath');
const dirtyFlag = element('dirtyFlag');
const editorHost = element('editor');
const emptyEditor = element('emptyEditor');
const problemList = element('problemList');
const problemCount = element('problemCount');
const missionLogSelect = element<HTMLSelectElement>('missionLog');
const missionRefresh = element<HTMLButtonElement>('missionRefresh');
const missionFocus = element<HTMLButtonElement>('missionFocus');
const missionGraph = element('missionGraph');
const missionDetail = element('missionDetail');
const missionSummary = element('missionSummary');
const hunkRefresh = element<HTMLButtonElement>('hunkRefresh');
const hunkStage = element<HTMLButtonElement>('hunkStage');
const hunkList = element('hunkList');
const hunkCount = element('hunkCount');
const githubQueueRefresh = element<HTMLButtonElement>('githubQueueRefresh');
const githubQueueList = element('githubQueueList');
const githubQueueSummary = element('githubQueueSummary');
const lspStatus = element('lspStatus');
const editorStatus = element('editorStatus');
const cursorPos = element('cursorPos');
const languageMode = element('languageMode');

const state: {
  root: string;
  files: string[];
  current: OpenFile | null;
} = { root: '', files: [], current: null };

let missionLogs: MissionLog[] = [];
let missionSnapshot: MissionSnapshot | null = null;
let missionSelection: string | null = null;
let missionFocusEnabled = false;
let missionPolling = false;
let gitHunks: GitHunkSnapshot | null = null;
let hunkPolling = false;

const openFiles = new Map<string, OpenFile>();
const diagnostics = new Map<string, PublishDiagnosticsParams>();
const changeTimers = new Map<string, number>();

const editor = monaco.editor.create(editorHost, {
  theme: 'vs-dark',
  automaticLayout: true,
  fontSize: 13.5,
  lineHeight: 21,
  minimap: { enabled: false },
  scrollBeyondLastLine: false,
  renderWhitespace: 'selection',
  padding: { top: 10 },
  smoothScrolling: true,
});

const lsp = new LspClient({
  onDiagnostics: (params) => applyDiagnostics(params),
  onState: (connection, detail) => setLspState(connection, detail),
});

function setStatus(message: string): void {
  editorStatus.textContent = message;
}

function setLspState(connection: LspState, detail?: string): void {
  lspStatus.className = 'status-pill ' + connection;
  lspStatus.textContent = connection === 'ready' ? 'ryoko-lsp ready' : 'ryoko-lsp ' + connection;
  if (detail) setStatus(`ryoko-lsp: ${detail}`);
}

function markDirty(file: OpenFile): void {
  file.dirty = file.model.getValue() !== file.savedText;
  if (state.current === file) {
    dirtyFlag.classList.toggle('hidden', !file.dirty);
    activePath.textContent = (file.dirty ? '● ' : '') + file.path;
  }
}

function renderFiles(): void {
  const query = fileFilter.value.trim().toLowerCase();
  const visible = state.files.filter((path) => path.toLowerCase().includes(query));
  fileCount.textContent = `${state.files.length} files`;
  fileList.replaceChildren();
  if (visible.length === 0) {
    const hint = document.createElement('p');
    hint.className = 'hint';
    hint.textContent = state.files.length === 0 ? 'No files in this workspace.' : 'No matching files.';
    fileList.append(hint);
    return;
  }
  const fragment = document.createDocumentFragment();
  for (const path of visible.slice(0, 1500)) {
    const button = document.createElement('button');
    button.type = 'button';
    button.className = 'file-link' + (state.current?.path === path ? ' active' : '');
    button.textContent = path;
    button.title = path;
    button.addEventListener('click', () => {
      void openFile(path);
    });
    fragment.append(button);
  }
  fileList.append(fragment);
}

function revealEditor(): void {
  editorHost.classList.remove('hidden');
  emptyEditor.classList.add('hidden');
  editor.layout();
}

async function loadFiles(): Promise<void> {
  state.files = await invoke<string[]>('list_files');
  renderFiles();
}

async function openFolder(): Promise<void> {
  const selected = await open({ directory: true, multiple: false });
  if (typeof selected !== 'string') return;
  if ([...openFiles.values()].some((file) => file.dirty)
    && !window.confirm('Switch workspaces and discard unsaved edits?')) {
    return;
  }
  try {
    const info = await invoke<WorkspaceInfo>('open_workspace', { path: selected });
    // Open the new workspace successfully before discarding any old editor
    // buffers. Changing a folder must not erase unsaved content on failure.
    await lsp.stop();
    for (const timer of changeTimers.values()) window.clearTimeout(timer);
    changeTimers.clear();
    for (const file of openFiles.values()) file.model.dispose();
    openFiles.clear();
    diagnostics.clear();
    state.current = null;
    missionLogs = [];
    missionSnapshot = null;
    missionSelection = null;
    missionFocusEnabled = false;
    missionFocus.disabled = true;
    missionFocus.textContent = 'Focus';
    missionLogSelect.replaceChildren();
    missionGraph.replaceChildren();
    missionDetail.replaceChildren();
    gitHunks = null;
    hunkList.replaceChildren();
    hunkCount.textContent = '0';
    hunkStage.disabled = true;
    githubQueueSummary.textContent = 'not loaded';
    githubQueueList.replaceChildren();
    githubQueueRefresh.disabled = false;
    editor.setModel(null);
    clearProblems();
    state.root = info.root;
    workspaceName.textContent = info.name;
    workspaceName.title = info.root;
    saveButton.disabled = true;
    activePath.textContent = 'No file open';
    dirtyFlag.classList.add('hidden');
    setStatus('Loading files…');
    await loadFiles();
    setStatus(`${state.files.length} files from ${info.name}`);
    void refreshMissionLogs();
    void refreshHunks();
    await lsp.start(info.root);
  } catch (error) {
    setStatus(error instanceof Error ? error.message : String(error));
  }
}

function chosenHunks(): number[] {
  return [...hunkList.querySelectorAll<HTMLInputElement>('input[data-hunk-id]')]
    .filter((input) => input.checked)
    .map((input) => Number(input.dataset.hunkId));
}

function updateHunkStageButton(): void {
  hunkStage.disabled = !gitHunks || chosenHunks().length === 0 || hunkPolling;
}

async function refreshHunks(): Promise<void> {
  if (!state.root || hunkPolling) return;
  const openedRoot = state.root;
  hunkPolling = true;
  hunkStage.disabled = true;
  try {
    const snapshot = await invoke<GitHunkSnapshot>('list_review_hunks');
    if (openedRoot !== state.root) return;
    gitHunks = snapshot;
    hunkList.replaceChildren();
    hunkCount.textContent = String(snapshot.hunks.length);
    if (snapshot.hunks.length === 0) {
      const hint = document.createElement('p');
      hint.className = 'hint';
      hint.textContent = 'No unstaged selectable text hunks. Binary, rename and mode changes are excluded.';
      hunkList.append(hint);
      return;
    }
    for (const hunk of snapshot.hunks) {
      const item = document.createElement('div');
      item.className = 'hunk-item';
      const checkbox = document.createElement('input');
      checkbox.type = 'checkbox';
      checkbox.dataset.hunkId = String(hunk.id);
      checkbox.setAttribute('aria-label', 'Select hunk ' + hunk.id + ' in ' + hunk.path);
      checkbox.addEventListener('change', updateHunkStageButton);
      const title = document.createElement('span');
      title.className = 'hunk-title';
      title.textContent = '#' + hunk.id + ' ' + hunk.path + ' ' + hunk.header;
      const jump = document.createElement('button');
      jump.type = 'button';
      jump.className = 'button ghost compact';
      jump.textContent = 'Open';
      jump.title = 'Navigate to this hunk in Monaco';
      jump.addEventListener('click', () => {
        void (async () => {
          await openFile(hunk.path);
          const match = hunk.header.match(/\+(\d+)/);
          if (match) {
            const line = Number(match[1]);
            editor.revealLineInCenter(line);
            editor.setPosition({ lineNumber: line, column: 1 });
          }
        })();
      });
      const preview = document.createElement('pre');
      preview.className = 'hunk-preview';
      preview.textContent = hunk.preview;
      item.append(checkbox, title, jump, preview);
      hunkList.append(item);
    }
  } catch (error) {
    gitHunks = null;
    hunkCount.textContent = 'unavailable';
    const hint = document.createElement('p');
    hint.className = 'hint';
    hint.textContent = error instanceof Error ? error.message : String(error);
    hunkList.replaceChildren(hint);
  } finally {
    hunkPolling = false;
    updateHunkStageButton();
  }
}

async function stageSelectedHunks(): Promise<void> {
  if (!gitHunks || chosenHunks().length === 0) return;
  if ([...openFiles.values()].some((file) => file.dirty)) {
    setStatus('Save or discard unsaved Monaco edits before staging hunks.');
    return;
  }
  hunkStage.disabled = true;
  const ids = chosenHunks();
  try {
    await invoke('stage_review_hunks', { ids, expectedPatch: gitHunks.patch });
    setStatus('Staged ' + ids.length + ' selected text hunk(s) in Git index. No commit or merge was made.');
    await refreshHunks();
  } catch (error) {
    setStatus('Staging denied or failed: ' + (error instanceof Error ? error.message : String(error)));
    updateHunkStageButton();
  }
}

/** Read-only GitHub queue. Credentials remain in the local gh CLI, never in Monaco. */
async function refreshGitHubQueue(): Promise<void> {
  if (!state.root || githubQueueRefresh.disabled) return;
  githubQueueRefresh.disabled = true;
  githubQueueSummary.textContent = 'loading…';
  githubQueueList.replaceChildren();
  try {
    const queue = await invoke<GitHubQueue>('github_review_queue');
    githubQueueSummary.textContent = `${queue.pulls.length} PRs · ${queue.issues.length} issues`;
    const heading = document.createElement('p');
    heading.className = 'github-repository';
    heading.textContent = queue.repository;
    githubQueueList.append(heading);
    const renderGroup = (label: string, items: GitHubQueueItem[]) => {
      const title = document.createElement('p');
      title.className = 'github-queue-heading';
      title.textContent = label;
      githubQueueList.append(title);
      if (items.length === 0) {
        const empty = document.createElement('p');
        empty.className = 'hint';
        empty.textContent = 'No open items';
        githubQueueList.append(empty);
      }
      for (const item of items) {
        // Never trust a URL from an external tool without checking the host.
        const url = new URL(item.url);
        if (url.protocol !== 'https:' || url.hostname !== 'github.com') continue;
        const link = document.createElement('a');
        link.className = 'github-queue-item';
        link.href = url.href;
        link.target = '_blank';
        link.rel = 'noopener noreferrer';
        link.textContent = `#${item.number} ${item.title}${item.draft ? ' (draft)' : ''}`;
        link.title = 'Open on GitHub';
        githubQueueList.append(link);
      }
    };
    renderGroup('Pull requests', queue.pulls);
    renderGroup('Issues', queue.issues);
  } catch (error) {
    githubQueueSummary.textContent = 'unavailable';
    const notice = document.createElement('p');
    notice.className = 'hint';
    notice.textContent = String(error);
    githubQueueList.append(notice);
  } finally {
    githubQueueRefresh.disabled = false;
  }
}

function missionStatusLabel(task: MissionTask): string {
  if (task.status === 'succeeded') return 'succeeded';
  if (task.status === 'failed') return 'failed';
  if (task.status === 'skipped') return 'skipped';
  if (task.status === 'retrying') return `retry ${task.attempt ?? ''}`.trim();
  if (task.status === 'validating') return 'validating';
  if (task.status === 'running') return 'running';
  return 'scheduled';
}

function renderMissionOptions(): void {
  const previous = missionLogSelect.value;
  missionLogSelect.replaceChildren();
  if (missionLogs.length === 0) {
    const option = document.createElement('option');
    option.value = '';
    option.textContent = 'No event logs';
    missionLogSelect.append(option);
    missionLogSelect.disabled = true;
    return;
  }
  missionLogSelect.disabled = false;
  for (const log of missionLogs) {
    const option = document.createElement('option');
    option.value = log.name;
    option.textContent = log.name;
    missionLogSelect.append(option);
  }
  missionLogSelect.value = missionLogs.some((log) => log.name === previous)
    ? previous
    : missionLogs[0].name;
}

async function refreshMissionLogs(): Promise<void> {
  if (!state.root || missionPolling) return;
  missionPolling = true;
  try {
    const logs = await invoke<MissionLog[]>('list_mission_logs');
    const namesChanged =
      missionLogSelect.options.length === 0 ||
      logs.length !== missionLogs.length ||
      logs.some((log, index) => log.name !== missionLogs[index]?.name);
    missionLogs = logs;
    if (namesChanged) renderMissionOptions();
    if (!missionLogSelect.value) {
      missionSummary.textContent = 'no events';
      missionGraph.replaceChildren();
      const hint = document.createElement('p');
      hint.className = 'hint';
      hint.textContent = 'No event logs yet. Start a run with --event-log .ryoko/events/run.jsonl.';
      missionGraph.append(hint);
      missionDetail.replaceChildren(hint.cloneNode(true));
      return;
    }
    const text = await invoke<string>('read_mission_log', { name: missionLogSelect.value });
    missionSnapshot = reduceMissionEvents(parseMissionLog(text));
    renderMission();
  } catch (error) {
    missionSummary.textContent = 'unavailable';
    const hint = document.createElement('p');
    hint.className = 'hint';
    hint.textContent = error instanceof Error ? error.message : String(error);
    missionGraph.replaceChildren(hint);
  } finally {
    missionPolling = false;
  }
}

function renderMission(): void {
  const snapshot = missionSnapshot;
  missionGraph.replaceChildren();
  if (!snapshot || snapshot.tasks.length === 0) {
    missionSummary.textContent = 'no tasks';
    const hint = document.createElement('p');
    hint.className = 'hint';
    hint.textContent = 'Waiting for task_scheduled events…';
    missionGraph.append(hint);
    renderMissionDetail();
    return;
  }
  missionSummary.textContent = snapshot.finished
    ? `${snapshot.finished.succeeded} ok · ${snapshot.finished.failed} failed · ${snapshot.finished.durationMs}ms`
    : `${snapshot.tasks.length}/${snapshot.taskCount} tasks`;

  missionFocus.disabled = !missionSelection;
  missionFocus.textContent = missionFocusEnabled ? 'Show all' : 'Focus';
  const visible = missionFocusEnabled
    ? focusMissionTasks(snapshot.tasks, missionSelection)
    : snapshot.tasks;
  const layout = layoutMission(visible);
  const canvas = document.createElement('div');
  canvas.className = 'mission-canvas';
  canvas.style.width = `${layout.width}px`;
  canvas.style.height = `${layout.height}px`;

  const svg = document.createElementNS('http://www.w3.org/2000/svg', 'svg');
  svg.classList.add('mission-edges');
  svg.setAttribute('width', String(layout.width));
  svg.setAttribute('height', String(layout.height));
  svg.setAttribute('viewBox', `0 0 ${layout.width} ${layout.height}`);
  for (const edge of layout.edges) {
    const line = document.createElementNS('http://www.w3.org/2000/svg', 'path');
    const x1 = edge.from.x + missionNodeSize.width;
    const y1 = edge.from.y + missionNodeSize.height / 2;
    const x2 = edge.to.x;
    const y2 = edge.to.y + missionNodeSize.height / 2;
    const bend = Math.max(24, (x2 - x1) / 2);
    line.setAttribute('d', `M ${x1} ${y1} C ${x1 + bend} ${y1}, ${x2 - bend} ${y2}, ${x2} ${y2}`);
    line.setAttribute('class', 'mission-edge');
    svg.append(line);
  }
  canvas.append(svg);

  for (const node of layout.nodes) {
    const button = document.createElement('button');
    button.type = 'button';
    button.className = `mission-node ${node.task.status}`;
    if (missionSelection === node.task.id) button.classList.add('selected');
    button.style.left = `${node.x}px`;
    button.style.top = `${node.y}px`;
    button.style.width = `${missionNodeSize.width}px`;
    button.style.height = `${missionNodeSize.height}px`;
    button.title = `${node.task.role}: ${node.task.description}`;

    const role = document.createElement('span');
    role.className = 'mission-node-role';
    role.textContent = node.task.role;
    const label = document.createElement('span');
    label.className = 'mission-node-status';
    label.textContent = missionStatusLabel(node.task);
    const description = document.createElement('span');
    description.className = 'mission-node-description';
    description.textContent = node.task.description;
    button.append(role, label, description);
    button.addEventListener('click', () => {
      missionSelection = node.task.id;
      renderMission();
      renderMissionDetail();
    });
    canvas.append(button);
  }

  missionGraph.append(canvas);
  if (missionSelection && !snapshot.tasks.some((task) => task.id === missionSelection)) {
    missionSelection = null;
    missionFocusEnabled = false;
  }
  renderMissionDetail();
}

function addDetailRow(container: HTMLElement, label: string, value: string, code = false): void {
  if (!value) return;
  const row = document.createElement('div');
  row.className = 'mission-detail-row';
  const heading = document.createElement('span');
  heading.className = 'mission-detail-label';
  heading.textContent = label;
  const body = document.createElement(code ? 'pre' : 'p');
  body.className = 'mission-detail-value';
  body.textContent = value.length > 12_000 ? `${value.slice(0, 12_000)}…` : value;
  row.append(heading, body);
  container.append(row);
}

function renderMissionDetail(): void {
  missionDetail.replaceChildren();
  const task = missionSnapshot?.tasks.find((candidate) => candidate.id === missionSelection);
  if (!task) {
    const hint = document.createElement('p');
    hint.className = 'hint';
    hint.textContent = 'Select a task to inspect its brief, validation and output.';
    missionDetail.append(hint);
    return;
  }
  const heading = document.createElement('div');
  heading.className = 'mission-detail-heading';
  const title = document.createElement('h3');
  title.textContent = task.role;
  const status = document.createElement('span');
  status.className = `mission-status ${task.status}`;
  status.textContent = missionStatusLabel(task);
  heading.append(title, status);
  missionDetail.append(heading);

  addDetailRow(missionDetail, 'Task brief', task.description);
  addDetailRow(missionDetail, 'Agent', task.agent ?? '');
  addDetailRow(missionDetail, 'Attempts', task.attempts ? String(task.attempts) : '');
  addDetailRow(missionDetail, 'Progress', task.progress ?? '');
  if (task.usage) {
    addDetailRow(
      missionDetail,
      'Model usage',
      `${task.usage.requests} request(s); ${task.usage.inputTokens} input + ${task.usage.outputTokens} output tokens; ledger ${task.usage.ledgerCostUsd.toFixed(4)} (not verified billed cost)`,
    );
  }
  if (task.validation) {
    addDetailRow(
      missionDetail,
      task.validation.passed ? 'Validation passed' : 'Validation failed',
      task.validation.summary,
    );
    if (task.validation.logs.length > 0) {
      addDetailRow(missionDetail, 'Container / validation log', task.validation.logs.join('\n'), true);
    }
  }
  if (task.pullRequest) {
    addDetailRow(missionDetail, 'Pull request', `${task.pullRequest.id} → ${task.pullRequest.branch}`);
    addDetailRow(missionDetail, 'Files', task.pullRequest.files.join('\n'), true);
  }
  if (task.history?.length) {
    const section = document.createElement('p');
    section.className = 'mission-detail-label';
    section.textContent = 'Attempt timeline';
    missionDetail.append(section);
    for (const attempt of [...task.history].reverse()) {
      const row = document.createElement('section');
      row.className = 'mission-attempt';
      const label = document.createElement('strong');
      label.textContent = `Attempt ${attempt.number} · ${attempt.status}`;
      row.append(label);
      if (attempt.agent) addDetailRow(row, 'Agent', attempt.agent);
      if (attempt.progress) addDetailRow(row, 'Progress', attempt.progress);
      if (attempt.validation) {
        addDetailRow(row, 'Validation', attempt.validation.summary);
        if (attempt.validation.logs.length) {
          addDetailRow(row, 'Container / validation logs', attempt.validation.logs.join('\n'), true);
        }
      }
      if (attempt.error) addDetailRow(row, 'Error', attempt.error);
      missionDetail.append(row);
    }
  }
  addDetailRow(missionDetail, 'Output', task.output ?? '', true);
  addDetailRow(missionDetail, task.status === 'failed' ? 'Error' : 'Note', task.error ?? '');
}

async function openFile(path: string): Promise<void> {
  if (!state.root) return;
  try {
    setStatus(`Opening ${path}…`);
    const content = await invoke<string>('read_file', { path });
    const uri = fileUri(joinWorkspacePath(state.root, path));
    const languageId = languageForPath(path);
    let file = openFiles.get(uri);
    // Selection must move before `setValue` so the content-change handler
    // attributes the reload to this document rather than the previous one.
    if (file) state.current = file;
    if (!file) {
      const existing = monaco.editor.getModel(monaco.Uri.parse(uri));
      const model = existing ?? monaco.editor.createModel(content, languageId, monaco.Uri.parse(uri));
      if (existing && existing.getValue() !== content) existing.setValue(content);
      file = { path, uri, languageId, model, version: 1, savedText: content, dirty: false };
      openFiles.set(uri, file);
      state.current = file;
      lsp.ensureOpen(uri, languageId, file.version, model.getValue());
    } else if (!file.dirty && file.model.getValue() !== content) {
      file.savedText = content;
      file.model.setValue(content);
    }
    state.current = file;
    editor.setModel(file.model);
    revealEditor();
    languageMode.textContent = languageId;
    activePath.textContent = (file.dirty ? '● ' : '') + path;
    dirtyFlag.classList.toggle('hidden', !file.dirty);
    saveButton.disabled = false;
    renderFiles();
    updateCursor();
    setStatus(path);
  } catch (error) {
    setStatus(error instanceof Error ? error.message : String(error));
  }
}

function updateCursor(): void {
  const position = editor.getPosition();
  if (position) cursorPos.textContent = `Ln ${position.lineNumber}, Col ${position.column}`;
}

async function saveCurrent(): Promise<void> {
  const file = state.current;
  if (!file || !file.dirty) return;
  try {
    const content = file.model.getValue();
    await invoke('write_file', { path: file.path, content, expectedContent: file.savedText });
    file.savedText = content;
    file.version += 1;
    markDirty(file);
    lsp.change(file.uri, file.version, content);
    renderFiles();
    setStatus(`Saved ${file.path}`);
    void refreshHunks();
  } catch (error) {
    setStatus(`Save failed: ${error instanceof Error ? error.message : String(error)}`);
  }
}

function toMarker(diagnostic: LspDiagnostic): monaco.editor.IMarkerData {
  return {
    severity: markerSeverityFor(diagnostic.severity),
    message: diagnostic.message,
    source: diagnostic.source ?? 'ryoko',
    code: diagnostic.code !== undefined ? String(diagnostic.code) : undefined,
    startLineNumber: diagnostic.range.start.line + 1,
    startColumn: diagnostic.range.start.character + 1,
    endLineNumber: diagnostic.range.end.line + 1,
    endColumn: diagnostic.range.end.character + 1,
  };
}

function applyDiagnostics(params: PublishDiagnosticsParams): void {
  diagnostics.set(params.uri, params);
  const model = monaco.editor.getModel(monaco.Uri.parse(params.uri));
  if (model) {
    monaco.editor.setModelMarkers(model, 'ryoko-lsp', params.diagnostics.map(toMarker));
  }
  renderProblems();
}

function clearProblems(): void {
  problemList.replaceChildren();
  problemCount.textContent = '0';
}

function problemSeverityClass(severity: number | undefined): string {
  if (severity === 1) return 'error';
  if (severity === 2) return 'warning';
  return 'info';
}

function renderProblems(): void {
  const problems: FlatProblem[] = [];
  for (const params of diagnostics.values()) {
    for (const diagnostic of params.diagnostics) {
      problems.push({ uri: params.uri, diagnostic });
    }
  }
  problemCount.textContent = String(problems.length);
  problemList.replaceChildren();
  if (problems.length === 0) {
    const hint = document.createElement('p');
    hint.className = 'hint';
    hint.textContent = 'No diagnostics yet.';
    problemList.append(hint);
    return;
  }
  const fragment = document.createDocumentFragment();
  for (const problem of problems.slice(0, 300)) {
    const button = document.createElement('button');
    button.type = 'button';
    button.className = 'problem';
    const dot = document.createElement('span');
    dot.className = 'problem-dot ' + problemSeverityClass(problem.diagnostic.severity);
    const body = document.createElement('span');
    body.className = 'problem-body';
    const message = document.createElement('span');
    message.className = 'problem-message';
    message.textContent = problem.diagnostic.message;
    const location = document.createElement('span');
    location.className = 'problem-location';
    const path = pathFromFileUri(problem.uri) ?? problem.uri;
    location.textContent = `${path.split(/[\\/]/).pop() ?? path}:${problem.diagnostic.range.start.line + 1}`;
    body.append(message, location);
    button.append(dot, body);
    button.addEventListener('click', () => {
      void revealProblem(problem);
    });
    fragment.append(button);
  }
  problemList.append(fragment);
}

async function revealProblem(problem: FlatProblem): Promise<void> {
  const absolute = pathFromFileUri(problem.uri);
  if (!absolute || !state.root) return;
  const root = state.root.replace(/\\/g, '/').replace(/\/+$/, '');
  const candidate = absolute.replace(/\\/g, '/');
  if (!candidate.startsWith(root + '/')) return;
  const relative = candidate.slice(root.length + 1);
  await openFile(relative);
  editor.revealLineInCenter(problem.diagnostic.range.start.line + 1);
  editor.setPosition({
    lineNumber: problem.diagnostic.range.start.line + 1,
    column: problem.diagnostic.range.start.character + 1,
  });
  editor.focus();
}

editor.onDidChangeModelContent(() => {
  const file = state.current;
  if (!file) return;
  file.version += 1;
  markDirty(file);
  const timer = changeTimers.get(file.uri);
  if (timer !== undefined) window.clearTimeout(timer);
  changeTimers.set(
    file.uri,
    window.setTimeout(() => {
      changeTimers.delete(file.uri);
      lsp.change(file.uri, file.version, file.model.getValue());
    }, 180),
  );
});

editor.onDidChangeCursorPosition(updateCursor);

editor.addCommand(monaco.KeyMod.CtrlCmd | monaco.KeyCode.KeyS, () => {
  void saveCurrent();
});

editor.addCommand(monaco.KeyMod.CtrlCmd | monaco.KeyCode.KeyO, () => {
  void openFolder();
});

openFolderButton.addEventListener('click', () => {
  void openFolder();
});
saveButton.addEventListener('click', () => {
  void saveCurrent();
});
fileFilter.addEventListener('input', renderFiles);
hunkRefresh.addEventListener('click', () => {
  void refreshHunks();
});
hunkStage.addEventListener('click', () => {
  void stageSelectedHunks();
});
missionFocus.addEventListener('click', () => {
  if (!missionSelection) return;
  missionFocusEnabled = !missionFocusEnabled;
  renderMission();
});
githubQueueRefresh.addEventListener('click', () => {
  void refreshGitHubQueue();
});
missionRefresh.addEventListener('click', () => {
  void refreshMissionLogs();
});
missionLogSelect.addEventListener('change', () => {
  missionSelection = null;
  void refreshMissionLogs();
});

window.setInterval(() => {
  if (state.root && document.visibilityState === 'visible') void refreshMissionLogs();
}, 1200);

window.addEventListener('beforeunload', () => {
  void lsp.stop();
});

setLspState('offline');
renderMissionOptions();
