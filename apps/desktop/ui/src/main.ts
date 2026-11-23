import { invoke } from '@tauri-apps/api/core';
import { listen } from '@tauri-apps/api/event';
import { open } from '@tauri-apps/plugin-dialog';
import { FitAddon } from '@xterm/addon-fit';
import { Terminal } from '@xterm/xterm';
import '@xterm/xterm/css/xterm.css';
import * as monaco from 'monaco-editor';
import EditorWorker from 'monaco-editor/esm/vs/editor/editor.worker?worker';
import './styles.css';
import { LspClient, type LspState, type LspServerStatus } from './lsp';
import { registerMonacoCapabilities } from './lsp_bridge';
import { buildTree, changeKind, filterTreePaths } from './tree';
import { mountQuickOpen } from './quick_open_view';
import { mountSearchPanel } from './search_view';
import {
  renderChanges,
  type ChangesSnapshot,
} from './changes_view';
import { mountSettingsDialog } from './settings_view';
import { InlineEditController } from './inline_edit_view';
import {
  chordForEvent,
  loadOverrides,
  resolveKeybindings,
} from './keybindings';
import { mountConnectors } from './connectors_panel';
import { mountMarketplaceBrowser } from './marketplace_browser';
import { mountGithubQueue } from './github_queue';
import {
  focusMissionTasks,
  formatLedgerCost,
  formatTokens,
  layoutMission,
  liveStatus,
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
import {
  sessionMeta,
  sessionTitle,
  type AgentSession,
} from './sessions';

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
  old_start: number;
  new_start: number;
  new_lines: number;
}

interface GitHunkSnapshot {
  patch: string;
  hunks: GitHunk[];
}

interface ReviewFileContent {
  path: string;
  original: string;
  modified: string;
}

interface RunStatus {
  running: boolean;
  paused: boolean;
  exitCode: number | null;
  eventLog: string | null;
  startedMs: number | null;
  logTail: string;
}

interface ReviewValidation {
  passed: boolean;
  summary: string;
  logs: string[];
  duration_ms: number;
}

interface FimChunk {
  requestId: string;
  text: string;
}

interface FimError {
  requestId: string;
  message: string;
}

interface TerminalOutput {
  sessionId: number;
  data: string;
}

interface TerminalStatus {
  running: boolean;
  sessionId: number | null;
}

interface DapMessage {
  sessionId: number;
  message: string;
}

interface DapLog {
  sessionId: number;
  message: string;
}

interface DapStatus {
  running: boolean;
  sessionId: number | null;
}

interface SandboxInitResult {
  image: string;
  dockerfile: string;
  built: boolean;
  ecosystem: string;
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
const agentSessionRefresh = element<HTMLButtonElement>('agentSessionRefresh');
const agentSessionList = element('agentSessionList');
const agentSessionSummary = element('agentSessionSummary');
const hunkRefresh = element<HTMLButtonElement>('hunkRefresh');
const hunkStage = element<HTMLButtonElement>('hunkStage');
const hunkList = element('hunkList');
const hunkCount = element('hunkCount');
const hunkValidation = element('hunkValidation');
const diffReview = element('diffReview');
const diffReviewPath = element('diffReviewPath');
const diffReviewClose = element<HTMLButtonElement>('diffReviewClose');
const diffEditorHost = element('diffEditor');
const runForm = element<HTMLFormElement>('runForm');
const runGoal = element<HTMLTextAreaElement>('runGoal');
const runBudget = element<HTMLInputElement>('runBudget');
const runBudgetValue = element<HTMLOutputElement>('runBudgetValue');
const runConcurrency = element<HTMLInputElement>('runConcurrency');
const runConcurrencyValue = element<HTMLOutputElement>('runConcurrencyValue');
const runSandbox = element<HTMLInputElement>('runSandbox');
const sandboxInit = element<HTMLButtonElement>('sandboxInit');
const runTeam = element<HTMLInputElement>('runTeam');
const runStart = element<HTMLButtonElement>('runStart');
const runPause = element<HTMLButtonElement>('runPause');
const runCancel = element<HTMLButtonElement>('runCancel');
const runState = element('runState');
const runLog = element('runLog');
const lspStatus = element('lspStatus');
const lspPills = element('lspPills');
const changesSummary = element('changesSummary');
const changesList = element('changesList');
const editorStatus = element('editorStatus');
const cursorPos = element('cursorPos');
const languageMode = element('languageMode');
const bottomPanel = element('bottomPanel');
const terminalTab = element<HTMLButtonElement>('terminalTab');
const searchTab = element<HTMLButtonElement>('searchTab');
const searchPanel = element('searchPanel');
const searchInput = element<HTMLInputElement>('searchInput');
const debugTab = element<HTMLButtonElement>('debugTab');
const bottomClose = element<HTMLButtonElement>('bottomClose');
const bottomStatus = element('bottomStatus');
const terminalPanel = element('terminalPanel');
const terminalHost = element('terminalHost');
const terminalStart = element<HTMLButtonElement>('terminalStart');
const terminalStop = element<HTMLButtonElement>('terminalStop');
const toggleTerminal = element<HTMLButtonElement>('toggleTerminal');
const toggleDebug = element<HTMLButtonElement>('toggleDebug');
const debugPanel = element('debugPanel');
const debugAdapter = element<HTMLInputElement>('debugAdapter');
const debugArgs = element<HTMLInputElement>('debugArgs');
const debugProgram = element<HTMLInputElement>('debugProgram');
const debugStart = element<HTMLButtonElement>('debugStart');
const debugStop = element<HTMLButtonElement>('debugStop');
const debugContinue = element<HTMLButtonElement>('debugContinue');
const debugNext = element<HTMLButtonElement>('debugNext');
const debugStepIn = element<HTMLButtonElement>('debugStepIn');
const debugStepOut = element<HTMLButtonElement>('debugStepOut');
const debugBreakpoint = element<HTMLButtonElement>('debugBreakpoint');
const debugStatus = element('debugStatus');
const debugLog = element('debugLog');
const inlineEditRun = element<HTMLButtonElement>('inlineEditRun');
const inlineEditCancel = element<HTMLButtonElement>('inlineEditCancel');
const inlineEditAccept = element<HTMLButtonElement>('inlineEditAccept');
const inlineEditInput = element<HTMLInputElement>('inlineEditInput');
const searchReplace = element<HTMLInputElement>('searchReplace');
const changesDetails = element<HTMLDetailsElement>('changesDetails');
const changesRefresh = element<HTMLButtonElement>('changesRefresh');
const openSettings = element<HTMLButtonElement>('openSettings');
const workbenchLayout = document.querySelector<HTMLElement>('.layout')!;
const inspectorPane = element('inspectorPane');
const railButtons = Array.from(document.querySelectorAll<HTMLButtonElement>('.rail-button'));

type WorkbenchView = 'explorer' | 'editor' | 'mission' | 'changes' | 'problems' | 'settings';

function activateView(view: WorkbenchView): void {
  if (view === 'settings') {
    openSettings.click();
    return;
  }
  workbenchLayout.dataset.activeView = view;
  workbenchLayout.classList.toggle('hide-explorer', view !== 'explorer');
  workbenchLayout.classList.toggle('hide-inspector', view === 'explorer' || view === 'editor');
  for (const button of railButtons) {
    const active = button.dataset.view === view;
    button.classList.toggle('active', active);
    if (button.dataset.view !== 'settings') button.setAttribute('aria-pressed', String(active));
  }
  if (view === 'changes') {
    changesDetails.open = true;
    changesDetails.scrollIntoView({ block: 'nearest' });
  } else if (view === 'mission') {
    inspectorPane.querySelector('.mission')?.scrollIntoView({ block: 'nearest' });
  } else if (view === 'problems') {
    problemList.scrollIntoView({ block: 'nearest' });
  }
  requestAnimationFrame(() => editor.layout());
}

railButtons.forEach((button) => button.addEventListener('click', () => {
  const view = button.dataset.view;
  if (view === 'explorer' || view === 'editor' || view === 'mission' ||
      view === 'changes' || view === 'problems' || view === 'settings') {
    activateView(view);
  }
}));
element<HTMLButtonElement>('emptyOpenFolder').addEventListener('click', () => openFolderButton.click());

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
let agentSessions: AgentSession[] = [];
let agentSessionPolling = false;
let gitHunks: GitHunkSnapshot | null = null;
let hunkPolling = false;
let missionPreferredLog: string | null = null;
let runSnapshot: RunStatus | null = null;
let runPolling = false;
let diffWidgets: monaco.editor.IContentWidget[] = [];
let diffOriginalModel: monaco.editor.ITextModel | null = null;
let diffModifiedModel: monaco.editor.ITextModel | null = null;
let xterm: Terminal | null = null;
let fitAddon: FitAddon | null = null;
let terminalSessionId: number | null = null;
let terminalStarting = false;
let pendingTerminalOutput: TerminalOutput[] = [];
let terminalStatusPolling = false;
let dapStatusPolling = false;
let dapSessionId: number | null = null;
let dapSeq = 0;
let dapThreadId = 1;
let dapActive = false;
const debugBreakpoints = new Map<string, Set<number>>();

const openFiles = new Map<string, OpenFile>();
const diagnostics = new Map<string, PublishDiagnosticsParams>();
const changeTimers = new Map<string, number>();
const expandedDirectories = new Set<string>();
const workbenchChanges = new Map<string, ReturnType<typeof changeKind>>();
const keybindings = resolveKeybindings(loadOverrides(localStorage));
const quickOpen = mountQuickOpen({
  workspaceRoot: () => state.root,
  files: () => state.files,
  openFile,
  setStatus,
});

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

// Ghost text is a content widget rather than Monaco's inline-completions API:
// the provider API resolves once, while local FIM tokens arrive incrementally.
let fimGeneration = 0;
let fimText = '';
let fimPosition: monaco.Position | null = null;
let fimTimer: number | undefined;
let acceptingFim = false;
const fimNode = document.createElement('div');
fimNode.className = 'fim-ghost hidden';
fimNode.setAttribute('aria-hidden', 'true');
const fimWidget: monaco.editor.IContentWidget = {
  getId: () => 'ryoko.fim.ghost',
  getDomNode: () => fimNode,
  getPosition: () =>
    fimPosition
      ? {
          position: fimPosition,
          preference: [monaco.editor.ContentWidgetPositionPreference.EXACT],
        }
      : null,
};
editor.addContentWidget(fimWidget);

function hideFim(cancelServer = true): void {
  const hadRequest = fimText !== '' || fimPosition !== null;
  fimText = '';
  fimPosition = null;
  fimNode.textContent = '';
  fimNode.classList.add('hidden');
  editor.layoutContentWidget(fimWidget);
  if (cancelServer && hadRequest) {
    fimGeneration += 1;
    void invoke('fim_cancel').catch(() => undefined);
  }
}

function scheduleFim(): void {
  if (fimTimer !== undefined) window.clearTimeout(fimTimer);
  if (acceptingFim) return;
  fimTimer = window.setTimeout(() => {
    fimTimer = undefined;
    void requestFim();
  }, 220);
}

async function requestFim(): Promise<void> {
  const file = state.current;
  const model = editor.getModel();
  const position = editor.getPosition();
  const selection = editor.getSelection();
  if (!file || !model || !position || !selection || !selection.isEmpty()) {
    hideFim();
    return;
  }
  hideFim();
  const offset = model.getOffsetAt(position);
  const full = model.getValue();
  const prefix = full.slice(Math.max(0, offset - 6_000), offset);
  const suffix = full.slice(offset, offset + 3_000);
  if (!prefix.trim()) return;
  const requestId = String(fimGeneration + 1);
  fimGeneration += 1;
  fimPosition = position;
  try {
    await invoke('fim_complete', { prefix, suffix, requestId });
  } catch (error) {
    if (requestId === String(fimGeneration)) hideFim(false);
    setStatus(`Completion unavailable: ${error instanceof Error ? error.message : String(error)}`);
  }
}

function applyFimChunk(payload: FimChunk): void {
  if (payload.requestId !== String(fimGeneration) || !fimPosition) return;
  const selection = editor.getSelection();
  if (!selection || !selection.isEmpty()) {
    hideFim();
    return;
  }
  fimText += payload.text;
  fimNode.textContent = fimText.split('\n').slice(0, 3).join('\n');
  fimNode.classList.remove('hidden');
  editor.layoutContentWidget(fimWidget);
}

function acceptFim(): void {
  const position = editor.getPosition();
  const model = editor.getModel();
  if (!position || !model || !fimText) return;
  const text = fimText;
  const offset = model.getOffsetAt(position);
  acceptingFim = true;
  editor.executeEdits('ryoko-fim', [
    {
      range: new monaco.Range(
        position.lineNumber,
        position.column,
        position.lineNumber,
        position.column,
      ),
      text,
      forceMoveMarkers: true,
    },
  ]);
  editor.setPosition(model.getPositionAt(offset + text.length));
  acceptingFim = false;
  hideFim();
}

const diffEditor = monaco.editor.createDiffEditor(diffEditorHost, {
  theme: 'vs-dark',
  automaticLayout: true,
  readOnly: true,
  originalEditable: false,
  renderSideBySide: false,
  fontSize: 13,
  lineHeight: 20,
  minimap: { enabled: false },
  scrollBeyondLastLine: false,
  padding: { top: 10 },
});

const lsp = new LspClient({
  onDiagnostics: (params, server) => applyDiagnostics(params, server),
  onState: (connection, detail) => setLspState(connection, detail),
  onServers: (servers) => renderServerPills(servers),
});

const inlineEdit = new InlineEditController({
  editor,
  currentFile: () => (state.current ? { path: state.current.path, model: state.current.model } : null),
  languageId: () => state.current?.languageId ?? 'plaintext',
  setStatus,
  onSaveAfterApply: saveCurrent,
});

function setStatus(message: string): void {
  editorStatus.textContent = message;
}

function showBottom(panel: 'terminal' | 'debug' | 'search'): void {
  bottomPanel.classList.remove('hidden');
  const terminal = panel === 'terminal';
  const search = panel === 'search';
  terminalTab.classList.toggle('active', terminal);
  searchTab.classList.toggle('active', search);
  debugTab.classList.toggle('active', !terminal && !search);
  terminalPanel.classList.toggle('hidden', !terminal);
  searchPanel.classList.toggle('hidden', !search);
  debugPanel.classList.toggle('hidden', terminal || search);
  if (terminal) {
    window.requestAnimationFrame(() => fitTerminal());
  }
  if (search) {
    searchInput.focus();
  }
}

function ensureTerminal(): Terminal {
  if (xterm) return xterm;
  const terminal = new Terminal({
    convertEol: true,
    cursorBlink: true,
    fontFamily: 'ui-monospace, SFMono-Regular, Menlo, monospace',
    fontSize: 12,
    scrollback: 5_000,
    theme: {
      background: '#0b0d12',
      foreground: '#e6e9f0',
      cursor: '#7c9cff',
      selectionBackground: '#21305a',
    },
  });
  const fit = new FitAddon();
  terminal.loadAddon(fit);
  terminal.open(terminalHost);
  terminal.onData((data) => {
    void invoke('terminal_write', { data }).catch((error) => {
      bottomStatus.textContent = String(error);
    });
  });
  xterm = terminal;
  fitAddon = fit;
  new ResizeObserver(() => fitTerminal()).observe(terminalHost);
  return terminal;
}

function fitTerminal(): void {
  if (!xterm || !fitAddon || terminalPanel.classList.contains('hidden')) return;
  if (terminalHost.clientWidth === 0 || terminalHost.clientHeight === 0) return;
  fitAddon.fit();
  void invoke('terminal_resize', { rows: xterm.rows, cols: xterm.cols }).catch(() => undefined);
}

async function startTerminal(): Promise<void> {
  if (terminalStarting) return;
  terminalStarting = true;
  terminalStart.disabled = true;
  pendingTerminalOutput = [];
  try {
    showBottom('terminal');
    const terminal = ensureTerminal();
    terminal.reset();
    terminalSessionId = null;
    const status = await invoke<TerminalStatus>('terminal_start', { shell: null });
    terminalSessionId = status.sessionId;
    for (const output of pendingTerminalOutput) {
      if (output.sessionId === terminalSessionId) terminal.write(output.data);
    }
    terminalStop.disabled = false;
    bottomStatus.textContent = 'Local shell running';
    fitTerminal();
    terminal.focus();
  } catch (error) {
    terminalSessionId = null;
    terminalStart.disabled = false;
    terminalStop.disabled = true;
    bottomStatus.textContent = `Terminal unavailable: ${error instanceof Error ? error.message : String(error)}`;
  } finally {
    terminalStarting = false;
    pendingTerminalOutput = [];
  }
}

async function stopTerminal(): Promise<void> {
  try {
    await invoke('terminal_stop');
    terminalSessionId = null;
    terminalStart.disabled = false;
    terminalStop.disabled = true;
    bottomStatus.textContent = 'Terminal stopped';
  } catch (error) {
    bottomStatus.textContent = 'Could not stop terminal: ' + String(error);
  }
}

function appendDebug(line: string): void {
  const next = `${debugLog.textContent ?? ''}${line}\n`;
  const lines = next.split('\n');
  debugLog.textContent = lines.slice(Math.max(0, lines.length - 300)).join('\n');
  debugLog.scrollTop = debugLog.scrollHeight;
}

function updateDapButtons(running: boolean): void {
  dapActive = running;
  debugStart.disabled = running;
  debugStop.disabled = !running;
  debugContinue.disabled = !running;
  debugNext.disabled = !running;
  debugStepIn.disabled = !running;
  debugStepOut.disabled = !running;
  debugBreakpoint.disabled = !running;
}

async function sendDap(command: string, args?: Record<string, unknown>): Promise<void> {
  dapSeq += 1;
  const message = JSON.stringify({
    seq: dapSeq,
    type: 'request',
    command,
    ...(args ? { arguments: args } : {}),
  });
  appendDebug(`→ ${message}`);
  await invoke('dap_send', { message });
}

async function startDebug(): Promise<void> {
  const adapter = debugAdapter.value.trim();
  if (!adapter) {
    debugStatus.textContent = 'Enter a DAP adapter executable first.';
    return;
  }
  try {
    showBottom('debug');
    const args = debugArgs.value.trim().split(/\s+/).filter(Boolean);
    const status = await invoke<DapStatus>('dap_start', { adapter, args });
    dapSessionId = status.sessionId;
    dapSeq = 0;
    updateDapButtons(true);
    debugStatus.textContent = 'Adapter started; initializing…';
    await sendDap('initialize', {
      adapterID: 'ryoko',
      clientID: 'ryoko-desktop',
      linesStartAt1: true,
      columnsStartAt1: true,
      pathFormat: 'path',
      supportsRunInTerminalRequest: false,
    });
  } catch (error) {
    await invoke('dap_stop').catch(() => undefined);
    dapSessionId = null;
    updateDapButtons(false);
    debugStatus.textContent = `Debug adapter unavailable: ${error instanceof Error ? error.message : String(error)}`;
  }
}

async function launchDap(): Promise<void> {
  const program = debugProgram.value.trim();
  if (!program) {
    debugStatus.textContent = 'Adapter initialized. Set a program path and restart to launch it.';
    return;
  }
  const absolute = program.startsWith('/') || /^[A-Za-z]:[\\/]/.test(program);
  try {
    await sendDap('launch', {
      program: absolute ? program : joinWorkspacePath(state.root, program),
      cwd: state.root,
      args: [],
      stopOnEntry: false,
    });
    await sendDap('configurationDone');
    debugStatus.textContent = 'Launch requested';
  } catch (error) {
    debugStatus.textContent = `Launch failed: ${error instanceof Error ? error.message : String(error)}`;
  }
}

function handleDapMessage(payload: DapMessage): void {
  if (dapSessionId === null || payload.sessionId !== dapSessionId) return;
  appendDebug(`← ${payload.message}`);
  let value: Record<string, unknown>;
  try {
    value = JSON.parse(payload.message) as Record<string, unknown>;
  } catch {
    return;
  }
  if (value.type === 'event') {
    const event = String(value.event ?? '');
    if (event === 'initialized') {
      debugStatus.textContent = 'Adapter initialized';
      void launchDap();
    } else if (event === 'stopped') {
      const body = value.body as Record<string, unknown> | undefined;
      if (typeof body?.threadId === 'number') dapThreadId = body.threadId;
      debugStatus.textContent = `Stopped: ${String(body?.reason ?? 'paused')}`;
    } else if (event === 'continued') {
      debugStatus.textContent = 'Running';
    } else if (event === 'terminated' || event === 'exited') {
      debugStatus.textContent = 'Session ended';
      updateDapButtons(false);
    }
  } else if (value.type === 'response' && value.success === false) {
    debugStatus.textContent = `DAP ${String(value.command)} failed: ${String(value.message ?? '')}`;
  }
}

async function stopDebug(): Promise<void> {
  await invoke('dap_stop').catch(() => undefined);
  dapSessionId = null;
  updateDapButtons(false);
  debugStatus.textContent = 'Debug adapter stopped';
}

async function dapStep(command: string): Promise<void> {
  if (!dapActive) return;
  try {
    await sendDap(command, { threadId: dapThreadId });
  } catch (error) {
    debugStatus.textContent = String(error);
  }
}

async function toggleBreakpoint(): Promise<void> {
  const file = state.current;
  const position = editor.getPosition();
  if (!file || !position || !dapActive) return;
  const absolute = joinWorkspacePath(state.root, file.path);
  const lines = debugBreakpoints.get(absolute) ?? new Set<number>();
  if (lines.has(position.lineNumber)) lines.delete(position.lineNumber);
  else lines.add(position.lineNumber);
  debugBreakpoints.set(absolute, lines);
  try {
    await sendDap('setBreakpoints', {
      source: { path: absolute, name: file.path },
      breakpoints: [...lines].sort((a, b) => a - b).map((line) => ({ line })),
    });
    debugStatus.textContent = `Breakpoints in ${file.path}: ${lines.size}`;
  } catch (error) {
    debugStatus.textContent = String(error);
  }
}

function setLspState(connection: LspState, detail?: string): void {
  lspStatus.className = 'status-pill ' + connection;
  lspStatus.textContent = connection === 'ready' ? 'ryoko-lsp ready' : 'ryoko-lsp ' + connection;
  if (detail) setStatus(`ryoko-lsp: ${detail}`);
}

function renderServerPills(servers: LspServerStatus[]): void {
  lspPills.replaceChildren();
  for (const server of servers) {
    if (server.id === 'ryoko-lsp') continue;
    const pill = document.createElement('span');
    pill.className = 'status-pill ' + server.state;
    pill.textContent = `${server.id} ${server.state}`;
    lspPills.append(pill);
  }
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
  const visible = filterTreePaths(state.files, query);
  fileCount.textContent = `${state.files.length} files`;
  fileList.replaceChildren();
  if (visible.length === 0) {
    const hint = document.createElement('p');
    hint.className = 'hint';
    hint.textContent = state.files.length === 0 ? 'No files in this workspace.' : 'No matching files.';
    fileList.append(hint);
    return;
  }
  const nodes = buildTree(visible, { expanded: expandedDirectories, changes: workbenchChanges });
  const fragment = document.createDocumentFragment();
  for (const node of nodes) {
    if (node.type === 'dir') {
      const button = document.createElement('button');
      button.type = 'button';
      button.className = 'tree-dir';
      button.style.paddingLeft = `${8 + node.depth * 14}px`;
      button.textContent = `${expandedDirectories.has(node.path) ? '▾' : '▸'} ${node.name}`;
      button.title = node.path;
      button.addEventListener('click', () => {
        if (expandedDirectories.has(node.path)) expandedDirectories.delete(node.path);
        else expandedDirectories.add(node.path);
        renderFiles();
      });
      fragment.append(button);
    } else {
      const button = document.createElement('button');
      button.type = 'button';
      button.className = 'file-link tree-file'
        + (state.current?.path === node.path ? ' active' : '')
        + (node.change ? ` change-${node.change}` : '');
      button.style.paddingLeft = `${8 + node.depth * 14}px`;
      button.textContent = node.change ? `${node.name} · ${node.change.charAt(0).toUpperCase()}` : node.name;
      button.title = node.change ? `${node.path} (${node.change})` : node.path;
      button.addEventListener('click', () => {
        void openFile(node.path);
        if (window.matchMedia('(max-width: 720px)').matches) activateView('editor');
      });
      fragment.append(button);
    }
  }
  fileList.append(fragment);
}

async function refreshWorkbenchChanges(): Promise<void> {
  if (!state.root) return;
  try {
    const snapshot = await invoke<ChangesSnapshot>('git_changes');
    workbenchChanges.clear();
    if (snapshot.isRepository) {
      for (const entry of snapshot.entries) {
        workbenchChanges.set(entry.path, changeKind(entry.kind));
      }
    }
    renderFiles();
    changesSummary.textContent = snapshot.isRepository
      ? `${snapshot.entries.length} change(s)`
      : 'not a repository';
    renderChanges(snapshot.entries, snapshot.isRepository, changesList, {
      openDiff: openPathDiff,
      setStatus,
    });
  } catch (error) {
    changesSummary.textContent = 'unavailable';
    setStatus(`Change scan failed: ${error instanceof Error ? error.message : String(error)}`);
  }
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
  if (runSnapshot?.running
    && !window.confirm('An agent run is active. Cancel it before switching workspaces?')) {
    return;
  }
  if (runSnapshot?.running) {
    try {
      await invoke('cancel_run');
    } catch (error) {
      setStatus('Could not cancel the active run: ' + (error instanceof Error ? error.message : String(error)));
      return;
    }
  }
  try {
    const info = await invoke<WorkspaceInfo>('open_workspace', { path: selected });
    // Open the new workspace successfully before discarding any old editor
    // buffers. Changing a folder must not erase unsaved content on failure.
    await lsp.stop();
    quickOpen.close();
    await stopTerminal();
    await stopDebug();
    for (const timer of changeTimers.values()) window.clearTimeout(timer);
    changeTimers.clear();
    for (const file of openFiles.values()) file.model.dispose();
    openFiles.clear();
    diagnostics.clear();
    workbenchChanges.clear();
    expandedDirectories.clear();
    changesSummary.textContent = 'no folder';
    changesList.replaceChildren();
    state.current = null;
    closeDiffReview();
    missionLogs = [];
    missionSnapshot = null;
    missionSelection = null;
    missionPreferredLog = null;
    missionFocusEnabled = false;
    runSnapshot = null;
    runState.textContent = 'No run started.';
    runLog.textContent = '';
    runLog.classList.add('hidden');
    runStart.disabled = false;
    runPause.disabled = true;
    runCancel.disabled = true;
    missionFocus.disabled = true;
    missionFocus.textContent = 'Focus';
    missionLogSelect.replaceChildren();
    missionGraph.replaceChildren();
    missionDetail.replaceChildren();
    agentSessions = [];
    agentSessionList.replaceChildren();
    agentSessionSummary.textContent = '0 sessions';
    agentSessionRefresh.disabled = false;
    gitHunks = null;
    hunkList.replaceChildren();
    hunkCount.textContent = '0';
    hunkValidation.textContent = '';
    hunkValidation.classList.add('hidden');
    hunkValidation.classList.remove('passed', 'failed');
    hunkStage.disabled = true;
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
    lsp.setWorkspaceRoot(info.root);
    void refreshWorkbenchChanges();
    void refreshAgentSessions();
    void refreshMissionLogs();
    void refreshHunks();
    void refreshRunStatus();
    await lsp.start(info.root);
  } catch (error) {
    setStatus(error instanceof Error ? error.message : String(error));
  }
}

function sessionStateLabel(session: AgentSession): string {
  return session.state === 'active' ? 'working' : session.state;
}

function renderAgentSessions(): void {
  agentSessionList.replaceChildren();
  agentSessionSummary.textContent = agentSessions.length === 1
    ? '1 session'
    : `${agentSessions.length} sessions`;
  if (agentSessions.length === 0) {
    const hint = document.createElement('p');
    hint.className = 'hint';
    hint.textContent = 'No active or published worktree sessions. Start one with ryoko run.';
    agentSessionList.append(hint);
    return;
  }
  for (const session of agentSessions) {
    const item = document.createElement('article');
    item.className = 'agent-session';
    item.dataset.state = session.state;
    const heading = document.createElement('div');
    heading.className = 'agent-session-head';
    const title = document.createElement('span');
    title.className = 'agent-session-title';
    title.textContent = sessionTitle(session);
    title.title = session.id;
    const state = document.createElement('span');
    state.className = 'agent-session-state';
    state.textContent = sessionStateLabel(session);
    heading.append(title, state);
    const meta = document.createElement('span');
    meta.className = 'agent-session-meta';
    meta.textContent = sessionMeta(session);
    const branch = document.createElement('span');
    branch.className = 'agent-session-branch';
    branch.textContent = `${session.branch} → ${session.baseBranch}`;
    item.append(heading, meta, branch);
    if (session.files.length > 0) {
      const files = document.createElement('span');
      files.className = 'agent-session-files';
      files.textContent = session.files.slice(0, 4).join(' · ')
        + (session.files.length > 4 ? ` · +${session.files.length - 4}` : '');
      item.append(files);
    }
    agentSessionList.append(item);
  }
}

async function refreshAgentSessions(): Promise<void> {
  if (!state.root || agentSessionPolling) return;
  const openedRoot = state.root;
  agentSessionPolling = true;
  agentSessionRefresh.disabled = true;
  try {
    const sessions = await invoke<AgentSession[]>('list_agent_sessions');
    if (openedRoot !== state.root) return;
    agentSessions = sessions;
    renderAgentSessions();
  } catch (error) {
    agentSessions = [];
    agentSessionSummary.textContent = 'unavailable';
    const hint = document.createElement('p');
    hint.className = 'hint';
    hint.textContent = error instanceof Error ? error.message : String(error);
    agentSessionList.replaceChildren(hint);
  } finally {
    agentSessionPolling = false;
    agentSessionRefresh.disabled = false;
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
      jump.textContent = 'Review diff';
      jump.title = 'Open this hunk in the Monaco diff editor with inline actions';
      jump.addEventListener('click', () => {
        void openHunkDiff(hunk);
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
    void refreshWorkbenchChanges();
    setStatus('Staged ' + ids.length + ' selected text hunk(s) in Git index. No commit or merge was made.');
    await refreshHunks();
  } catch (error) {
    setStatus('Staging denied or failed: ' + (error instanceof Error ? error.message : String(error)));
    updateHunkStageButton();
  }
}

function clearDiffEditor(): void {
  const modified = diffEditor.getModifiedEditor();
  for (const widget of diffWidgets) modified.removeContentWidget(widget);
  diffWidgets = [];
  diffEditor.setModel(null);
  diffOriginalModel?.dispose();
  diffModifiedModel?.dispose();
  diffOriginalModel = null;
  diffModifiedModel = null;
}

async function openPathDiff(path: string): Promise<void> {
  if (!state.root) return;
  if ([...openFiles.values()].some((file) => file.dirty)) {
    setStatus('Save or discard unsaved Monaco edits before reviewing changes.');
    return;
  }
  try {
    const review = await invoke<ReviewFileContent>('read_review_file', { path });
    clearDiffEditor();
    const languageId = languageForPath(path);
    const suffix = encodeURIComponent(path).replaceAll('%', '_');
    diffOriginalModel = monaco.editor.createModel(
      review.original,
      languageId,
      monaco.Uri.parse(`ryoko-review://original/${suffix}`),
    );
    diffModifiedModel = monaco.editor.createModel(
      review.modified,
      languageId,
      monaco.Uri.parse(`ryoko-review://modified/${suffix}`),
    );
    diffEditor.setModel({ original: diffOriginalModel, modified: diffModifiedModel });
    diffReviewPath.textContent = `${path} - working tree`;
    diffReview.classList.remove('hidden');
    editorHost.classList.add('hidden');
    emptyEditor.classList.add('hidden');
    diffEditor.layout();
    setStatus(`Reviewing ${path} against HEAD`);
  } catch (error) {
    setStatus('Diff review failed: ' + (error instanceof Error ? error.message : String(error)));
  }
}

function closeDiffReview(): void {
  clearDiffEditor();
  diffReview.classList.add('hidden');
  if (state.current) {
    revealEditor();
  } else {
    editorHost.classList.add('hidden');
    emptyEditor.classList.remove('hidden');
  }
}

function hunkWidget(hunk: GitHunk): monaco.editor.IContentWidget {
  const dom = document.createElement('div');
  dom.className = 'hunk-inline-actions';
  const accept = document.createElement('button');
  accept.type = 'button';
  accept.className = 'button primary compact';
  accept.textContent = 'Accept hunk';
  accept.title = 'Stage only this hunk after native confirmation';
  accept.addEventListener('click', () => void actOnHunk('accept', hunk));
  const reject = document.createElement('button');
  reject.type = 'button';
  reject.className = 'button ghost compact';
  reject.textContent = 'Reject hunk';
  reject.title = 'Revert only this hunk from the working files after native confirmation';
  reject.addEventListener('click', () => void actOnHunk('reject', hunk));
  const revise = document.createElement('button');
  revise.type = 'button';
  revise.className = 'button ghost compact';
  revise.textContent = 'Ask agent to revise';
  revise.title = 'Start a new validated worktree run with this hunk as context';
  revise.addEventListener('click', () => void actOnHunk('revise', hunk));
  dom.append(accept, reject, revise);
  return {
    getId: () => `ryoko-hunk-${hunk.id}-${hunk.path}`,
    getDomNode: () => dom,
    getPosition: () => ({
      position: { lineNumber: Math.max(1, hunk.new_start), column: 1 },
      preference: [
        monaco.editor.ContentWidgetPositionPreference.ABOVE,
        monaco.editor.ContentWidgetPositionPreference.BELOW,
      ],
    }),
  };
}

async function openHunkDiff(hunk: GitHunk): Promise<void> {
  if (!state.root) return;
  if ([...openFiles.values()].some((file) => file.dirty)) {
    setStatus('Save or discard unsaved Monaco edits before reviewing hunks.');
    return;
  }
  try {
    const review = await invoke<ReviewFileContent>('read_review_file', { path: hunk.path });
    clearDiffEditor();
    const languageId = languageForPath(hunk.path);
    const suffix = encodeURIComponent(hunk.path).replaceAll('%', '_');
    diffOriginalModel = monaco.editor.createModel(
      review.original,
      languageId,
      monaco.Uri.parse(`ryoko-review://original/${suffix}`),
    );
    diffModifiedModel = monaco.editor.createModel(
      review.modified,
      languageId,
      monaco.Uri.parse(`ryoko-review://modified/${suffix}`),
    );
    diffEditor.setModel({ original: diffOriginalModel, modified: diffModifiedModel });
    diffReviewPath.textContent = `${hunk.path} · hunk #${hunk.id}`;
    diffReview.classList.remove('hidden');
    editorHost.classList.add('hidden');
    emptyEditor.classList.add('hidden');
    diffEditor.layout();
    const modified = diffEditor.getModifiedEditor();
    for (const candidate of (gitHunks?.hunks ?? []).filter((item) => item.path === hunk.path)) {
      const widget = hunkWidget(candidate);
      diffWidgets.push(widget);
      modified.addContentWidget(widget);
    }
    modified.revealLineInCenter(Math.max(1, hunk.new_start));
    setStatus(`Reviewing ${hunk.path} in the diff editor`);
  } catch (error) {
    setStatus('Diff review failed: ' + (error instanceof Error ? error.message : String(error)));
  }
}

async function validateAfterHunk(): Promise<void> {
  const image = runSandbox.value.trim();
  hunkValidation.classList.remove('hidden', 'passed', 'failed');
  if (!image) {
    hunkValidation.textContent = 'Hunk applied. Set a sandbox image to run background pre-flight validation.';
    return;
  }
  hunkValidation.textContent = 'Validating the current checkout in the sandbox…';
  try {
    const report = await invoke<ReviewValidation>('validate_review', {
      sandboxImage: image,
      timeoutSecs: 300,
    });
    hunkValidation.classList.add(report.passed ? 'passed' : 'failed');
    hunkValidation.textContent = [
      `${report.passed ? 'Validation passed' : 'Validation failed'} · ${report.duration_ms}ms`,
      report.summary,
      ...report.logs,
    ].join('\n');
  } catch (error) {
    hunkValidation.classList.add('failed');
    hunkValidation.textContent = 'Validation could not run: '
      + (error instanceof Error ? error.message : String(error));
  }
}

async function actOnHunk(action: 'accept' | 'reject' | 'revise', hunk: GitHunk): Promise<void> {
  if (!gitHunks) return;
  if (action === 'revise') {
    const feedback = window.prompt(`Ask the agent to revise ${hunk.path}:`, '');
    if (!feedback?.trim()) return;
    if (!window.confirm(
      'A revision starts a new isolated worktree run from the current branch. ' +
      'The local unstaged hunk below is included as context, not applied automatically. Continue?',
    )) return;
    const goal = [
      `Revise the change in ${hunk.path}.`,
      '',
      'Selected local hunk:',
      hunk.preview,
      '',
      'Operator feedback:',
      feedback.trim(),
    ].join('\n');
    runGoal.value = goal;
    await startRun(goal);
    return;
  }
  try {
    if (action === 'accept') {
      await invoke('stage_review_hunks', { ids: [hunk.id], expectedPatch: gitHunks.patch });
      void refreshWorkbenchChanges();
      setStatus(`Accepted hunk #${hunk.id} into the Git index. No commit or merge was made.`);
    } else {
      await invoke('reject_review_hunks', { ids: [hunk.id], expectedPatch: gitHunks.patch });
      void refreshWorkbenchChanges();
      setStatus(`Rejected hunk #${hunk.id}; only those working-file edits were reverted.`);
    }
    await refreshHunks();
    const remaining = gitHunks?.hunks.find((item) => item.id === hunk.id);
    if (remaining) await openHunkDiff(remaining);
    else closeDiffReview();
    await validateAfterHunk();
  } catch (error) {
    setStatus(`Hunk ${action} failed: ` + (error instanceof Error ? error.message : String(error)));
  }
}

function updateRunLabels(): void {
  runBudgetValue.value = `$${Number(runBudget.value).toFixed(2)}`;
  runConcurrencyValue.value = runConcurrency.value;
}

function showRunStatus(status: RunStatus): void {
  runSnapshot = status;
  runStart.disabled = status.running;
  runPause.disabled = !status.running;
  runPause.textContent = status.paused ? 'Resume' : 'Pause';
  runCancel.disabled = !status.running;
  if (status.running) {
    runState.textContent = status.paused ? 'Run paused.' : 'Run active. Mission Control follows its event log.';
  } else if (status.exitCode !== null) {
    runState.textContent = `Last run exited with code ${status.exitCode}.`;
  } else if (status.eventLog) {
    runState.textContent = 'Last run finished.';
  } else {
    runState.textContent = 'No run started.';
  }
  runLog.textContent = status.logTail;
  runLog.classList.toggle('hidden', !status.logTail);
}

async function refreshRunStatus(): Promise<void> {
  if (!state.root || runPolling) return;
  runPolling = true;
  try {
    const status = await invoke<RunStatus>('run_status');
    showRunStatus(status);
  } catch (error) {
    runState.textContent = error instanceof Error ? error.message : String(error);
  } finally {
    runPolling = false;
  }
}

async function initializeSandbox(): Promise<void> {
  sandboxInit.disabled = true;
  runState.textContent = 'Inspecting project and building an offline sandbox image…';
  try {
    const result = await invoke<SandboxInitResult>('sandbox_init');
    runSandbox.value = result.image;
    runState.textContent = result.built
      ? `Built ${result.image} from ${result.dockerfile}`
      : `Wrote ${result.dockerfile}; image was not built`;
  } catch (error) {
    runState.textContent = `Sandbox initialization failed: ${error instanceof Error ? error.message : String(error)}`;
  } finally {
    sandboxInit.disabled = false;
  }
}

async function startRun(goalOverride?: string): Promise<void> {
  const goal = (goalOverride ?? runGoal.value).trim();
  if (!goal) {
    runState.textContent = 'Enter a goal before starting a run.';
    runGoal.focus();
    return;
  }
  updateRunLabels();
  runStart.disabled = true;
  runState.textContent = 'Starting ryoko run…';
  try {
    const status = await invoke<RunStatus>('start_run', {
      goal,
      sandboxImage: runSandbox.value.trim(),
      budgetUsd: Number(runBudget.value),
      maxConcurrency: Number(runConcurrency.value),
      team: runTeam.checked,
      agent: null,
      attempts: null,
      configPath: null,
      checkCommand: null,
    });
    showRunStatus(status);
    missionPreferredLog = status.eventLog;
    await refreshMissionLogs();
  } catch (error) {
    runStart.disabled = false;
    runState.textContent = error instanceof Error ? error.message : String(error);
  }
}

async function toggleRunPause(): Promise<void> {
  if (!runSnapshot?.running) return;
  try {
    const status = await invoke<RunStatus>(runSnapshot.paused ? 'resume_run' : 'pause_run');
    showRunStatus(status);
  } catch (error) {
    runState.textContent = error instanceof Error ? error.message : String(error);
  }
}

async function cancelRun(): Promise<void> {
  if (!runSnapshot?.running) return;
  if (!window.confirm('Cancel the active agent run? Validation child processes will be terminated.')) return;
  try {
    showRunStatus(await invoke<RunStatus>('cancel_run'));
  } catch (error) {
    runState.textContent = error instanceof Error ? error.message : String(error);
  }
}

function missionStatusLabel(task: MissionTask): string {
  const status = liveStatus(task).toLowerCase();
  return task.status === 'retrying' ? `retry ${task.attempt ?? ''} · ${status}`.trim() : status;
}

function failureExplanation(task: MissionTask): string {
  const stage = task.failureStage;
  const lead = stage === 'workspace'
    ? 'The workspace lease could not be created, so the agent never ran.'
    : stage === 'agent'
      ? 'The agent did not return a usable result.'
      : stage === 'staging'
        ? 'The proposed changes could not be written to the isolated worktree.'
        : stage === 'validation'
          ? 'The project check rejected the changes before publication.'
          : stage === 'publish'
            ? 'The changes passed validation but the worktree could not be published.'
            : 'The task did not complete.';
  return task.error ? `${lead} ${task.error}` : lead;
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
  const preferred = missionPreferredLog && missionLogs.some((log) => log.name === missionPreferredLog)
    ? missionPreferredLog
    : null;
  missionLogSelect.value = preferred
    ?? (missionLogs.some((log) => log.name === previous) ? previous : missionLogs[0].name);
  if (preferred) missionPreferredLog = null;
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
  const tokens = formatTokens(snapshot.totals.inputTokens + snapshot.totals.outputTokens);
  const usage = `${tokens} tok · est. ${formatLedgerCost(snapshot.totals.ledgerCostUsd)}`;
  missionSummary.textContent = snapshot.finished
    ? `${snapshot.finished.succeeded} ok · ${snapshot.finished.failed} failed · ${snapshot.finished.durationMs}ms · ${usage}`
    : `${snapshot.tasks.length}/${snapshot.taskCount} tasks · ${usage}`;

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

  addDetailRow(missionDetail, 'Live status', liveStatus(task));
  const dependencies = task.dependencies.length > 0
    ? `\nDepends on: ${task.dependencies.join(', ')}`
    : '\nDepends on: no upstream task';
  addDetailRow(
    missionDetail,
    'Prompt context (operator-visible)',
    `${task.description}${dependencies}\nRaw model prompts are not persisted.`,
    true,
  );
  addDetailRow(missionDetail, 'Agent', task.agent ?? '');
  addDetailRow(missionDetail, 'Attempts', task.attempts ? String(task.attempts) : '');
  addDetailRow(missionDetail, 'Progress', task.progress ?? '');
  if (task.usage) {
    addDetailRow(
      missionDetail,
      'Token usage and estimated cost',
      `${task.usage.requests} request(s); ${task.usage.inputTokens} input + ${task.usage.outputTokens} output tokens; estimated ledger ${formatLedgerCost(task.usage.ledgerCostUsd)} (not verified billed cost)`,
    );
  }
  if (task.error || task.failureStage) {
    addDetailRow(missionDetail, 'Failure explanation', failureExplanation(task));
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
  if (task.activity?.length) {
    const section = document.createElement('p');
    section.className = 'mission-detail-label';
    section.textContent = 'Agent activity timeline';
    missionDetail.append(section);
    const first = task.activity.find((entry) => entry.atMs > 0)?.atMs;
    const timeline = document.createElement('div');
    timeline.className = 'mission-activity-timeline';
    for (const entry of task.activity.slice(-40).reverse()) {
      const row = document.createElement('div');
      row.className = 'mission-activity';
      const time = document.createElement('span');
      time.className = 'mission-activity-time';
      time.textContent = first && entry.atMs >= first
        ? `+${((entry.atMs - first) / 1000).toFixed(1)}s`
        : '—';
      const label = document.createElement('span');
      label.className = 'mission-activity-label';
      label.textContent = entry.attempt ? `Attempt ${entry.attempt} · ${entry.label}` : entry.label;
      row.append(time, label);
      if (entry.detail) {
        const detail = document.createElement('pre');
        detail.className = 'mission-activity-detail';
        detail.textContent = entry.detail;
        row.append(detail);
      }
      timeline.append(row);
    }
    missionDetail.append(timeline);
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
  if (!diffReview.classList.contains('hidden')) closeDiffReview();
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
    hideFim();
    editor.setModel(file.model);
    revealEditor();
    languageMode.textContent = languageId;
    activePath.textContent = (file.dirty ? '● ' : '') + path;
    dirtyFlag.classList.toggle('hidden', !file.dirty);
    saveButton.disabled = false;
    renderFiles();
    updateCursor();
    setStatus(path);
    quickOpen.remember(path);
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
    void refreshWorkbenchChanges();
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

function applyDiagnostics(params: PublishDiagnosticsParams, server: string): void {
  diagnostics.set(`${server}\u0000${params.uri}`, params);
  const model = monaco.editor.getModel(monaco.Uri.parse(params.uri));
  if (model) {
    monaco.editor.setModelMarkers(model, `lsp-${server}`, params.diagnostics.map(toMarker));
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
  scheduleFim();
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
editor.onDidChangeCursorPosition(() => {
  hideFim();
  scheduleFim();
});
editor.onDidBlurEditorWidget(() => hideFim());
editor.onKeyDown((event) => {
  if (!fimText) return;
  if (event.keyCode === monaco.KeyCode.Tab && !event.shiftKey && !event.altKey) {
    event.preventDefault();
    event.stopPropagation();
    acceptFim();
  } else if (event.keyCode === monaco.KeyCode.Escape) {
    event.preventDefault();
    event.stopPropagation();
    hideFim();
  }
});

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
agentSessionRefresh.addEventListener('click', () => {
  void refreshAgentSessions();
});
hunkRefresh.addEventListener('click', () => {
  void refreshHunks();
});
hunkStage.addEventListener('click', () => {
  void stageSelectedHunks();
});
diffReviewClose.addEventListener('click', closeDiffReview);
runBudget.addEventListener('input', updateRunLabels);
sandboxInit.addEventListener('click', () => {
  void initializeSandbox();
});
runConcurrency.addEventListener('input', updateRunLabels);
runForm.addEventListener('submit', (event) => {
  event.preventDefault();
  void startRun();
});
runPause.addEventListener('click', () => {
  void toggleRunPause();
});
runCancel.addEventListener('click', () => {
  void cancelRun();
});
missionFocus.addEventListener('click', () => {
  if (!missionSelection) return;
  missionFocusEnabled = !missionFocusEnabled;
  renderMission();
});
missionRefresh.addEventListener('click', () => {
  void refreshMissionLogs();
});
missionLogSelect.addEventListener('change', () => {
  missionSelection = null;
  void refreshMissionLogs();
});
toggleTerminal.addEventListener('click', () => showBottom('terminal'));
toggleDebug.addEventListener('click', () => showBottom('debug'));
terminalTab.addEventListener('click', () => showBottom('terminal'));
searchTab.addEventListener('click', () => showBottom('search'));
debugTab.addEventListener('click', () => showBottom('debug'));
bottomClose.addEventListener('click', () => bottomPanel.classList.add('hidden'));

function dispatchBinding(action: string): void {
  switch (action) {
    case 'workbench.quickOpen':
      quickOpen.open();
      break;
    case 'workbench.search':
      showBottom('search');
      break;
    case 'workbench.replace':
      showBottom('search');
      searchReplace.focus();
      break;
    case 'workbench.changes':
      activateView('changes');
      break;
    case 'workbench.settings':
      openSettings.click();
      break;
    case 'editor.inlineEdit':
      inlineEdit.open();
      break;
    default:
      break;
  }
}

window.addEventListener('keydown', (event) => {
  if (event.repeat) return;
  const action = keybindings.byChord.get(chordForEvent(event));
  if (!action || action === 'editor.save') return;
  event.preventDefault();
  dispatchBinding(action);
});

inlineEditRun.addEventListener('click', () => void inlineEdit.submit());
inlineEditCancel.addEventListener('click', () => inlineEdit.reject());
inlineEditAccept.addEventListener('click', () => inlineEdit.accept());
inlineEditInput.addEventListener('keydown', (event) => {
  event.stopPropagation();
  if (event.key === 'Enter' && (event.metaKey || event.ctrlKey)) {
    event.preventDefault();
    void inlineEdit.submit();
  } else if (event.key === 'Escape') {
    event.preventDefault();
    inlineEdit.reject();
  }
});
changesRefresh.addEventListener('click', () => {
  void refreshWorkbenchChanges();
});
terminalStart.addEventListener('click', () => {
  void startTerminal();
});
terminalStop.addEventListener('click', () => {
  void stopTerminal();
});
debugStart.addEventListener('click', () => {
  void startDebug();
});
debugStop.addEventListener('click', () => {
  void stopDebug();
});
debugContinue.addEventListener('click', () => {
  void dapStep('continue');
});
debugNext.addEventListener('click', () => {
  void dapStep('next');
});
debugStepIn.addEventListener('click', () => {
  void dapStep('stepIn');
});
debugStepOut.addEventListener('click', () => {
  void dapStep('stepOut');
});
debugBreakpoint.addEventListener('click', () => {
  void toggleBreakpoint();
});

void listen<TerminalOutput>('ryoko-terminal-output', (event) => {
  if (terminalSessionId !== null && event.payload.sessionId === terminalSessionId) {
    ensureTerminal().write(event.payload.data);
  } else if (terminalStarting && terminalSessionId === null) {
    // Buffer the first shell prompt until terminal_start returns its session ID.
    const bytes = pendingTerminalOutput.reduce((sum, output) => sum + output.data.length, 0);
    if (bytes + event.payload.data.length <= 64 * 1024) {
      pendingTerminalOutput.push(event.payload);
    }
  }
});
void listen<FimChunk>('ryoko-fim-chunk', (event) => applyFimChunk(event.payload));
void listen<FimError>('ryoko-fim-error', (event) => {
  if (event.payload.requestId !== String(fimGeneration)) return;
  hideFim(false);
  setStatus(`Completion unavailable: ${event.payload.message}`);
});
void listen<DapMessage>('ryoko-dap-message', (event) => handleDapMessage(event.payload));
void listen<DapLog>('ryoko-dap-log', (event) => {
  if (dapSessionId !== null && event.payload.sessionId === dapSessionId && event.payload.message) {
    appendDebug(`[adapter] ${event.payload.message}`);
  }
});

async function pollTerminalStatus(): Promise<void> {
  if (terminalSessionId === null || terminalStarting || terminalStatusPolling) return;
  terminalStatusPolling = true;
  const observedId = terminalSessionId;
  try {
    const status = await invoke<TerminalStatus>('terminal_status');
    if (!status.running && terminalSessionId === observedId) {
      terminalSessionId = null;
      terminalStart.disabled = false;
      terminalStop.disabled = true;
      bottomStatus.textContent = 'Shell exited';
      xterm?.write('\r\n[Shell exited]\r\n');
    }
  } catch (error) {
    bottomStatus.textContent = 'Terminal status unavailable: ' + String(error);
  } finally {
    terminalStatusPolling = false;
  }
}

async function pollDebugStatus(): Promise<void> {
  if (dapSessionId === null || dapStatusPolling) return;
  dapStatusPolling = true;
  const observedId = dapSessionId;
  try {
    const status = await invoke<DapStatus>('dap_status');
    if (!status.running && dapSessionId === observedId) {
      dapSessionId = null;
      updateDapButtons(false);
      debugStatus.textContent = 'Debug adapter exited';
    }
  } catch (error) {
    debugStatus.textContent = 'Debug status unavailable: ' + String(error);
  } finally {
    dapStatusPolling = false;
  }
}

window.setInterval(() => {
  if (state.root && document.visibilityState === 'visible') {
    void refreshMissionLogs();
    void refreshRunStatus();
  }
}, 1200);

window.setInterval(() => {
  if (document.visibilityState === 'visible') {
    void pollTerminalStatus();
    void pollDebugStatus();
  }
}, 2500);

updateRunLabels();
refreshRunStatus();

window.addEventListener('beforeunload', () => {
  void lsp.stop();
  void invoke('terminal_stop').catch(() => undefined);
  void invoke('dap_stop').catch(() => undefined);
});

setLspState('offline');
renderMissionOptions();
mountGithubQueue(setStatus);
mountConnectors(setStatus);
mountMarketplaceBrowser(setStatus);
mountSearchPanel({ openFile, setStatus });
mountSettingsDialog({ setStatus });
registerMonacoCapabilities(
  monaco,
  lsp,
  ['rust', 'typescript', 'javascript', 'python', 'go', 'c', 'cpp', 'java', 'css', 'shell'],
);
