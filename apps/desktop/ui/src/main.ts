import { invoke } from '@tauri-apps/api/core';
import { listen } from '@tauri-apps/api/event';
import { open as pickFile } from '@tauri-apps/plugin-dialog';
import { FitAddon } from '@xterm/addon-fit';
import { Terminal } from '@xterm/xterm';
import '@xterm/xterm/css/xterm.css';
import * as monaco from 'monaco-editor';
import EditorWorker from 'monaco-editor/esm/vs/editor/editor.worker?worker';
import './styles.css';
import { mountEditorTabs, selectionAfterClose } from './editor_tabs';
import { LspClient, type LspState, type LspServerStatus } from './lsp';
import { registerMonacoCapabilities } from './lsp_bridge';
import { CommandRegistry, formatChord, type WorkbenchCommand } from './commands';
import { mountCommandPalette } from './command_palette';
import { mountUpdates } from './update_view';
import { mountBugReporter } from './bug_report';
import { mountMenuBar } from './menu_bar';
import { DEFAULT_LAYOUT, loadLayout, resizeBy, saveLayout } from './layout_state';
import { fileIconElement } from './file_icons';
import { workbenchIcon } from './workbench_icons';
import { THEMES, applyThemeToDocument, loadTheme, saveTheme, themeById, type ThemeDefinition } from './themes';
import {
  breadcrumbItems, fromLspDocumentSymbols, symbolChain, type OutlineSymbol,
} from './breadcrumbs';
import { LANGUAGE_IDS, defaultRunProfile, detectEditorLanguage, parseTaskArgs } from './languages';
import { buildTree, changeKind, filterTreePaths } from './tree';
import { mountQuickOpen } from './quick_open_view';
import { mountSearchPanel } from './search_view';
import {
  renderChanges,
  type ChangesSnapshot,
} from './changes_view';
import { mountSettingsDialog } from './settings_view';
import {
  KEYMAP_OPTIONS,
  keymapById,
  loadKeymap,
  saveKeymap,
  shouldDeferToEditor,
  type KeymapPreset,
} from './keymap_preset';
import {
  IMPORTED_THEME_STORAGE,
  loadImportedSettings,
  loadImportedTheme,
  monacoOptionsFor,
  parseVscodeSettings,
  parseVscodeTheme,
  saveImportedSettings,
  saveImportedTheme,
  type ImportedTheme,
} from './vscode_import';
import { mountOnboarding } from './onboarding';
import { createNotifier } from './toasts';
import { InlineEditController } from './inline_edit_view';
import {
  mountAgentChat,
  type CodeSuggestion,
  type EditorSelectionContext,
  type MentionPreview,
  type SymbolSuggestion,
} from './agent_chat';
import { previewAccepted, proposalHunks, type ProposalHunk } from './proposal_review';
import { mountAgentHarness, type StagedChange } from './agent_harness';
import { mountSwarm } from './swarm_view';
import { renderReview, type ReviewReport } from './ryoko_review';
import { DebugViewController } from './debug_view';
import {
  chordForEvent,
  loadOverrides,
  resolveKeybindings,
} from './keybindings';
import { mountConnectors } from './connectors_panel';
import { mountToolStream } from './tool_stream';
import { mountMarketplaceBrowser } from './marketplace_browser';
import { mountGithubQueue } from './github_queue';
import {
  activeMissionTools,
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
  markerSeverityFor,
  normalizeFilePath,
  pathFromFileUri,
  workspaceRelativeFile,
  type LspDiagnostic,
  type PublishDiagnosticsParams,
} from './protocol';
import {
  changeCounts,
  changeStatusLabel,
  sessionMeta,
  sessionTitle,
  type AgentSession,
  type SessionActionResult,
  type SessionChange,
  type SessionFileDiff,
} from './sessions';
import {
  createRecoverySessionId,
  describeRecoverableEntry,
  loadRecoveryEnabled,
  saveRecoveryEnabled,
  type RecoverableEntry,
  type RecoveryListResult,
} from './recovery';

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
  encoding?: string;
  readOnlyEncoding?: string;
  dirty: boolean;
  viewState?: monaco.editor.ICodeEditorViewState | null;
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

interface ReviewFile {
  path: string;
  oldPath?: string;
  state: string;
  stateLabel: string;
  editable: boolean;
  hunkIds: number[];
}

interface ReviewActionReport {
  action: string;
  files: { path: string; hunkIds: number[] }[];
}

interface GitHunkSnapshot {
  patch: string;
  hunks: GitHunk[];
  files: ReviewFile[];
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

interface CodeRunStatus {
  running: boolean;
  sessionId: number | null;
  exitCode: number | null;
}

interface CodeRunOutput {
  sessionId: number;
  stream: 'stdout' | 'stderr' | 'system';
  data: string;
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
const readOnlyBadge = element('readOnlyBadge');
const editorWorkspace = element('editorWorkspace');
const editorHost = element('editor');
const secondaryPane = element('secondaryPane');
const secondaryEditorHost = element('secondaryEditor');
const secondaryFileSelect = element<HTMLSelectElement>('secondaryFileSelect');
const splitEditorBtn = element<HTMLButtonElement>('splitEditorBtn');
const closeSplitEditor = element<HTMLButtonElement>('closeSplitEditor');
const newFileBtn = element<HTMLButtonElement>('newFileBtn');
const newFolderBtn = element<HTMLButtonElement>('newFolderBtn');
const vimStatus = element('vimStatus');
const emptyEditor = element('emptyEditor');
const problemList = element('problemList');
const problemCount = element('problemCount');
const missionLogSelect = element<HTMLSelectElement>('missionLog');
const missionRefresh = element<HTMLButtonElement>('missionRefresh');
const missionFocus = element<HTMLButtonElement>('missionFocus');
const missionGraph = element('missionGraph');
const toolStream = mountToolStream(missionGraph);
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
const diffPrevFile = element<HTMLButtonElement>('diffPrevFile');
const diffNextFile = element<HTMLButtonElement>('diffNextFile');
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
const notify = createNotifier();
const editorStatus = element('editorStatus');
const bugReporter = mountBugReporter({ setStatus: (message: string) => notify(message) });
const usageMeter = element('usageMeter');
const reviewInboxList = element('reviewInboxList');
const reviewInboxCount = element('reviewInboxCount');
const proposalToolbar = element('proposalToolbar');
const proposalHunksNode = element('proposalHunks');
const proposalApply = element<HTMLButtonElement>('proposalApply');
const proposalDiscard = element<HTMLButtonElement>('proposalDiscard');
const cursorPos = element('cursorPos');
const languageMode = element<HTMLSelectElement>('languageMode');
const runActiveFile = element<HTMLButtonElement>('runActiveFile');
const bottomPanel = element('bottomPanel');
const terminalTab = element<HTMLButtonElement>('terminalTab');
const taskTab = element<HTMLButtonElement>('taskTab');
const taskPanel = element('taskPanel');
const taskProgram = element<HTMLInputElement>('taskProgram');
const taskArgs = element<HTMLInputElement>('taskArgs');
const taskRun = element<HTMLButtonElement>('taskRun');
const taskStop = element<HTMLButtonElement>('taskStop');
const taskLabel = element('taskLabel');
const taskOutput = element('taskOutput');
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

type WorkbenchView = 'explorer' | 'editor' | 'chat' | 'mission' | 'changes' | 'problems' | 'settings';

function activateView(view: WorkbenchView): void {
  if (view === 'settings') {
    openSettings.click();
    return;
  }
  workbenchLayout.dataset.activeView = view;
  workbenchLayout.classList.toggle('hide-explorer', view !== 'explorer');
  const narrowLayout = window.matchMedia('(max-width: 720px)').matches;
  workbenchLayout.classList.toggle('hide-inspector', view === 'explorer' || (view === 'editor' && narrowLayout));
  for (const button of railButtons) {
    const active = button.dataset.view === view;
    button.classList.toggle('active', active);
    if (button.dataset.view !== 'settings') button.setAttribute('aria-pressed', String(active));
  }
  if (view === 'changes') {
    changesDetails.open = true;
    changesDetails.scrollIntoView({ block: 'nearest' });
  } else if (view === 'chat') {
    element('agentChat').scrollIntoView({ block: 'nearest' });
    agentChat.focus();
  } else if (view === 'mission') {
    inspectorPane.querySelector('.mission')?.scrollIntoView({ block: 'nearest' });
  } else if (view === 'problems') {
    problemList.scrollIntoView({ block: 'nearest' });
  }
  requestAnimationFrame(() => editor.layout());
}

railButtons.forEach((button) => button.addEventListener('click', () => {
  const view = button.dataset.view;
  if (view === 'explorer' || view === 'editor' || view === 'chat' || view === 'mission' ||
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
let chatInputTokens = 0;
let chatOutputTokens = 0;
let chatEstimatedCostUsd = 0;
let agentSessions: AgentSession[] = [];
let agentSessionPolling = false;
const sessionChanges = new Map<string, SessionChange[]>();
const sessionExpanded = new Set<string>();
interface SessionDiffContext {
  taskId: string;
  branch: string;
  changes: SessionChange[];
  index: number;
}
let sessionDiffContext: SessionDiffContext | null = null;
let sessionDiffTicket = 0;
let gitHunks: GitHunkSnapshot | null = null;
let hunkPolling = false;
let missionPreferredLog: string | null = null;
let runSnapshot: RunStatus | null = null;
let runPolling = false;
let diffWidgets: monaco.editor.IContentWidget[] = [];
let diffOriginalModel: monaco.editor.ITextModel | null = null;
let diffModifiedModel: monaco.editor.ITextModel | null = null;
interface PendingProposal {
  context: EditorSelectionContext;
  hunks: ProposalHunk[];
  accepted: Set<number>;
}
let pendingProposal: PendingProposal | null = null;
let xterm: Terminal | null = null;
let fitAddon: FitAddon | null = null;
let terminalSessionId: number | null = null;
let terminalStarting = false;
let pendingTerminalOutput: TerminalOutput[] = [];
let terminalStatusPolling = false;
let codeSessionId: number | null = null;
let codeStarting = false;
let codeStatusPolling = false;
let pendingCodeOutput: CodeRunOutput[] = [];
let codeOutputText = '';

const openFiles = new Map<string, OpenFile>();
let fileOpenGeneration = 0;
let workspaceSwitching = false;
const savingFiles = new Set<string>();
const createdEmptyDirs = new Set<string>();
let splitOpen = false;
let secondaryPath: string | null = null;
let secondaryEditor: monaco.editor.IStandaloneCodeEditor | null = null;
const renderTabs = mountEditorTabs(element('editorTabs'), selectOpenFile, closeOpenFile);
function updateTabs(): void {
  renderTabs([...openFiles.values()], state.current?.path ?? null);
  renderSecondaryFileOptions();
}
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

const initialTheme = loadTheme(localStorage);
applyThemeToDocument(initialTheme);
const layoutState = loadLayout(localStorage);
const prefersReducedMotion = window.matchMedia('(prefers-reduced-motion: reduce)').matches;
const editor = monaco.editor.create(editorHost, {
  theme: initialTheme.monaco,
  automaticLayout: true,
  fontSize: 13.5,
  lineHeight: 21,
  minimap: { enabled: layoutState.minimap, renderCharacters: true, showSlider: 'mouseover' },
  scrollBeyondLastLine: false,
  renderWhitespace: 'selection',
  padding: { top: 10 },
  smoothScrolling: !prefersReducedMotion,
});

let keymapPreset: KeymapPreset = loadKeymap(localStorage);
let keymapHandle: { dispose(): void } | null = null;
let keymapTicket = 0;
async function applyKeymap(preset: KeymapPreset, announce = true): Promise<void> {
  const ticket = ++keymapTicket;
  keymapHandle?.dispose();
  keymapHandle = null;
  vimStatus.hidden = true;
  vimStatus.classList.add('hidden');
  keymapPreset = preset;
  saveKeymap(localStorage, preset);
  const picker = document.getElementById('settingsKeymap') as HTMLSelectElement | null;
  if (picker) picker.value = preset;
  if (preset === 'vim') {
    try {
      type VimInit = (
        target: monaco.editor.IStandaloneCodeEditor,
        status: HTMLElement,
      ) => { dispose(): void };
      const module = await import('monaco-vim') as unknown as {
        initVimMode?: VimInit;
        default?: { initVimMode?: VimInit };
      };
      if (ticket !== keymapTicket) return;
      const initVimMode = module.initVimMode ?? module.default?.initVimMode;
      if (!initVimMode) throw new Error('monaco-vim did not expose initVimMode');
      keymapHandle = initVimMode(editor, vimStatus);
      vimStatus.hidden = false;
      vimStatus.classList.remove('hidden');
    } catch (error) {
      if (ticket !== keymapTicket) return;
      keymapPreset = 'default';
      setStatus('Vim keymap unavailable: ' + (error instanceof Error ? error.message : String(error)));
      return;
    }
  } else if (preset === 'emacs') {
    try {
      type EmacsConstructor = new (
        target: monaco.editor.IStandaloneCodeEditor,
      ) => { start(): void; dispose(): void };
      const module = await import('monaco-emacs') as unknown as {
        EmacsExtension?: EmacsConstructor;
        default?: EmacsConstructor;
      };
      if (ticket !== keymapTicket) return;
      const EmacsExtension = module.EmacsExtension ?? module.default;
      if (!EmacsExtension) throw new Error('monaco-emacs did not expose EmacsExtension');
      const extension = new EmacsExtension(editor);
      extension.start();
      keymapHandle = extension;
    } catch (error) {
      if (ticket !== keymapTicket) return;
      keymapPreset = 'default';
      setStatus('Emacs keymap unavailable: ' + (error instanceof Error ? error.message : String(error)));
      return;
    }
  }
  if (announce) setStatus('Editor keymap: ' + keymapById(preset).label);
}
// Restoring the saved keymap at startup is silent; only user changes announce.
void applyKeymap(keymapPreset, false);

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
  if (!file || file.readOnlyEncoding || !model || !position || !selection || !selection.isEmpty()) {
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
  if (state.current?.readOnlyEncoding || !position || !model || !fimText) return;
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

const importedSettings = loadImportedSettings(localStorage);
if (importedSettings) {
  editor.updateOptions(monacoOptionsFor(importedSettings));
}
const diffEditor = monaco.editor.createDiffEditor(diffEditorHost, {
  theme: initialTheme.monaco,
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
if (importedSettings) {
  diffEditor.updateOptions(monacoOptionsFor(importedSettings));
}

const lsp = new LspClient({
  onDiagnostics: (params, server) => applyDiagnostics(params, server),
  onState: (connection, detail) => setLspState(connection, detail),
  onServers: (servers) => renderServerPills(servers),
});

const inlineEdit = new InlineEditController({
  editor,
  currentFile: () => (state.current && !state.current.readOnlyEncoding ? { path: state.current.path, model: state.current.model } : null),
  languageId: () => state.current?.languageId ?? 'plaintext',
  setStatus,
  onSaveAfterApply: saveCurrent,
  onReview: openSuggestedReview,
});

function currentSelectionContext(): EditorSelectionContext | null {
  const file = state.current;
  const selection = editor.getSelection();
  if (!file || !selection || selection.isEmpty() || editor.getModel() !== file.model) return null;
  return {
    path: file.path,
    text: file.model.getValueInRange(selection),
    fullText: file.model.getValue(),
    startOffset: file.model.getOffsetAt(selection.getStartPosition()),
    endOffset: file.model.getOffsetAt(selection.getEndPosition()),
  };
}

function updateUsageMeter(): void {
  const totals = missionSnapshot?.totals;
  const missionTokens = (totals?.inputTokens ?? 0) + (totals?.outputTokens ?? 0);
  const total = missionTokens + chatInputTokens + chatOutputTokens;
  const estimatedCost = (totals?.ledgerCostUsd ?? 0) + chatEstimatedCostUsd;
  usageMeter.textContent = `${formatTokens(total)} tokens · est. ${formatLedgerCost(estimatedCost)}`;
  usageMeter.title = 'Usage reported by models and missions. Costs are estimates where pricing is known; unpriced requests may be excluded. Not an invoice.';
}

function showAgentChange(change: StagedChange, line?: number): void {
  clearDiffEditor();
  const languageId = detectEditorLanguage(change.path, change.content, monaco.languages.getLanguages());
  const suffix = encodeURIComponent(change.path).replaceAll('%', '_') + '-' + Date.now();
  diffOriginalModel = monaco.editor.createModel(
    change.original ?? '', languageId, monaco.Uri.parse(`ryoko-agent://original/${suffix}`),
  );
  diffModifiedModel = monaco.editor.createModel(
    change.content, languageId, monaco.Uri.parse(`ryoko-agent://proposed/${suffix}`),
  );
  diffEditor.setModel({ original: diffOriginalModel, modified: diffModifiedModel });
  diffReviewPath.textContent = `${change.path} · agent change (not saved yet)`;
  diffReview.classList.remove('hidden');
  editorWorkspace.classList.add('hidden');
  emptyEditor.classList.add('hidden');
  diffEditor.layout();
  if (line) {
    diffEditor.getModifiedEditor().revealLineInCenter(line);
    diffEditor.getModifiedEditor().setPosition({ lineNumber: line, column: 1 });
  }
  setStatus(`Reviewing the agent's change to ${change.path}. Apply or discard it in the chat.`);
}

function reloadAppliedChanges(changes: StagedChange[]): void {
  for (const change of changes) {
    const open = openFiles.get(change.path);
    if (open && !open.dirty) {
      open.model.setValue(change.content);
      open.savedText = change.content;
      open.dirty = false;
    }
  }
  void loadFiles();
}

const swarmView = mountSwarm({
  setStatus,
  openFile: (path, line) => void openFileAtLine(path, line ?? 1),
  onFinished: () => agentHarness.swarmFinished(),
});

const agentHarness = mountAgentHarness({
  runSwarm: (goal) => swarmView.run(goal),
  setStatus,
  showChange: showAgentChange,
  onApplied: reloadAppliedChanges,
  onCost: (cost) => {
    chatEstimatedCostUsd += cost;
    updateUsageMeter();
  },
});

const changesReview = element<HTMLButtonElement>('changesReview');
const changesReviewResult = element('changesReviewResult');
changesReview.addEventListener('click', async () => {
  changesReview.disabled = true;
  changesReview.textContent = 'Reviewing…';
  changesReviewResult.replaceChildren();
  try {
    const report = await invoke<ReviewReport>('ryoko_review', { source: { source: 'working_tree' } });
    changesReviewResult.append(renderReview(report, (finding) => void openFileAtLine(finding.path, finding.line ?? 1)));
  } catch (error) {
    const hint = document.createElement('p');
    hint.className = 'hint';
    hint.textContent = error instanceof Error ? error.message : String(error);
    changesReviewResult.append(hint);
  } finally {
    changesReview.disabled = false;
    changesReview.textContent = 'Review with Ryoko';
  }
});

const agentChat = mountAgentChat({
  agentMode: () => agentHarness.agentMode(),
  runAgent: (goal) => agentHarness.run(goal),
  files: () => state.files,
  selection: currentSelectionContext,
  review: openSuggestedReview,
  searchSymbols: (query) => invoke<SymbolSuggestion[]>('search_context_symbols', { query }),
  resolveMention: (mention) => invoke<MentionPreview>('resolve_context_mention', {
    kind: mention.kind,
    value: mention.value,
  }),
  onTokens: (input, output, estimatedCostUsd) => {
    chatInputTokens += input;
    chatOutputTokens += output;
    chatEstimatedCostUsd += estimatedCostUsd;
    updateUsageMeter();
  },
  setStatus,
});

function isEditorBufferStatus(text: string): boolean {
  if (text === 'Ready' || text === 'No file open' || text === 'Loading files…') return true;
  if (text.startsWith('Opening ') && text.endsWith('…')) return true;
  if (text.startsWith('Read-only preview: ')) return true;
  if (/^\d+\s+files\s+from\s+/i.test(text)) return true;
  if (/^(?:[A-Za-z]:[\\/]|[\w./\\:-]+(?:\.[\w-]+)+$)/.test(text)) return true;
  return false;
}

function setStatus(message: string): void {
  editorStatus.textContent = message;
  if (!isEditorBufferStatus(message)) {
    notify(message);
  }
}

function showBottom(panel: 'terminal' | 'debug' | 'search' | 'task'): void {
  bottomPanel.classList.remove('hidden');
  const terminal = panel === 'terminal';
  const search = panel === 'search';
  const task = panel === 'task';
  terminalTab.classList.toggle('active', terminal);
  searchTab.classList.toggle('active', search);
  taskTab.classList.toggle('active', task);
  debugTab.classList.toggle('active', panel === 'debug');
  terminalPanel.classList.toggle('hidden', !terminal);
  searchPanel.classList.toggle('hidden', !search);
  taskPanel.classList.toggle('hidden', !task);
  debugPanel.classList.toggle('hidden', panel !== 'debug');
  if (terminal) {
    window.requestAnimationFrame(() => fitTerminal());
  }
  if (search) {
    searchInput.focus();
  }
}

function appendTaskOutput(data: string): void {
  codeOutputText = (codeOutputText + data).slice(-120_000);
  taskOutput.textContent = codeOutputText;
  taskOutput.scrollTop = taskOutput.scrollHeight;
}

function configureTaskForPath(path: string): void {
  const profile = defaultRunProfile(path);
  taskProgram.value = profile?.program ?? '';
  taskArgs.value = profile?.args.join(' ') ?? '';
  taskLabel.textContent = profile
    ? profile.label + ' · Installed toolchain required. Approval is requested before each run.'
    : 'No direct-file runner for this language. Enter a build or run executable and literal arguments. {file} and {workspace} are supported.';
}

async function startCodeTask(): Promise<void> {
  const file = state.current;
  const root = state.root;
  if (!file || !root || file.readOnlyEncoding) {
    setStatus('Open an editable source file before running a task.');
    return;
  }
  if (codeStarting || codeSessionId !== null) {
    setStatus('Stop the active task before starting another.');
    return;
  }
  const program = taskProgram.value.trim();
  if (!program) {
    showBottom('task');
    taskProgram.focus();
    setStatus('Enter an executable, or choose a language with a Run file preset.');
    return;
  }
  let args: string[];
  try {
    args = parseTaskArgs(taskArgs.value);
  } catch (error) {
    setStatus(String(error));
    return;
  }
  if (file.dirty) {
    await saveCurrent();
    if (file.dirty) {
      setStatus('Save this file successfully before running it.');
      return;
    }
  }
  codeStarting = true;
  taskRun.disabled = true;
  runActiveFile.disabled = true;
  pendingCodeOutput = [];
  codeOutputText = '';
  showBottom('task');
  appendTaskOutput('Awaiting native approval for ' + program + '...\n');
  try {
    const status = await invoke<CodeRunStatus>('code_run_start', {
      path: file.path, program, args,
    });
    if (state.root !== root) {
      await invoke('code_run_stop');
      return;
    }
    codeSessionId = status.sessionId;
    codeOutputText = '';
    appendTaskOutput('> ' + program + ' ' + args.join(' ') + '\n');
    for (const output of pendingCodeOutput) {
      if (output.sessionId === codeSessionId) appendTaskOutput(output.data);
    }
    taskStop.disabled = false;
    taskLabel.textContent = 'Running ' + program + ' · ' + file.path;
    setStatus('Code task started: ' + program);
  } catch (error) {
    appendTaskOutput('\n[Not started] ' + String(error) + '\n');
    setStatus('Code task unavailable: ' + String(error));
  } finally {
    codeStarting = false;
    pendingCodeOutput = [];
    if (codeSessionId === null) {
      taskRun.disabled = false;
      runActiveFile.disabled = !state.current;
    }
  }
}

async function stopCodeTask(): Promise<void> {
  try {
    await invoke('code_run_stop');
    if (codeSessionId !== null) appendTaskOutput('\n[Task stopped]\n');
  } catch (error) {
    setStatus('Could not stop code task: ' + String(error));
  } finally {
    codeSessionId = null;
    taskStop.disabled = true;
    taskRun.disabled = false;
    runActiveFile.disabled = !state.current;
  }
}

async function pollCodeStatus(): Promise<void> {
  if (codeSessionId === null || codeStarting || codeStatusPolling || !state.root) return;
  codeStatusPolling = true;
  const observed = codeSessionId;
  try {
    const result = await invoke<CodeRunStatus>('code_run_status');
    if (!result.running && codeSessionId === observed) {
      codeSessionId = null;
      taskStop.disabled = true;
      taskRun.disabled = false;
      runActiveFile.disabled = !state.current;
      const exit = result.exitCode === null ? 'terminated' : 'exit code ' + result.exitCode;
      appendTaskOutput('\n[Task finished: ' + exit + ']\n');
      taskLabel.textContent = 'Finished · ' + exit;
      setStatus('Code task ' + exit);
    }
  } catch (error) {
    setStatus('Code task status unavailable: ' + String(error));
  } finally {
    codeStatusPolling = false;
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
    if (fitAddon && terminalHost.clientWidth > 0 && terminalHost.clientHeight > 0) {
      fitAddon.fit();
    }
    terminalSessionId = null;
    const status = await invoke<TerminalStatus>('terminal_start', {
      shell: null,
      rows: terminal.rows,
      cols: terminal.cols,
    });
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

async function openFileAtLine(path: string, line: number, column?: number): Promise<void> {
  const root = normalizeFilePath(state.root).replace(/\/+$/, '');
  const candidate = normalizeFilePath(path);
  const relative = candidate.startsWith(root + '/') ? candidate.slice(root.length + 1) : candidate;
  await openFile(relative);
  if (line > 0) {
    editor.revealLineInCenter(line);
    editor.setPosition({
      lineNumber: line,
      column: column && column > 0 ? column : 1,
    });
    editor.focus();
  }
}

const debugController = new DebugViewController({
  openFileAtLine,
  getCurrentFile: () => (state.current ? { path: state.current.path } : null),
  getEditorLine: () => editor.getPosition()?.lineNumber ?? null,
  getWorkspaceRoot: () => state.root,
  setStatus,
  showBottom,
  joinWorkspacePath,
});
debugController.mount();

function setLspState(connection: LspState, detail?: string): void {
  lspStatus.className = 'status-pill ' + connection;
  // "offline" just means no workspace is open yet, so it reads as idle
  // rather than as a failure; only a real start failure is shown as an error.
  const label: Record<LspState, string> = {
    offline: 'idle', starting: 'starting…', ready: 'ready', error: 'failed', stopped: 'stopped',
  };
  lspStatus.textContent = 'ryoko-lsp ' + label[connection];
  lspStatus.title = connection === 'offline' ? 'Starts when you open a folder' : detail ?? '';
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
    activePath.textContent = file.path;
  }
  updateTabs();
}

function makeTreeActionBtn(label: string, iconName: 'pencil' | 'trash-2', danger: boolean, onClick: () => void): HTMLButtonElement {
  const btn = document.createElement('button');
  btn.type = 'button';
  btn.className = 'tree-action-btn' + (danger ? ' danger' : '');
  btn.setAttribute('aria-label', label);
  btn.title = label;
  btn.append(workbenchIcon(iconName, 'control-icon'));
  btn.addEventListener('click', (event) => {
    event.stopPropagation();
    onClick();
  });
  return btn;
}

function handleTreeKeydown(event: KeyboardEvent, path: string, isDir: boolean): void {
  const items = Array.from(fileList.querySelectorAll<HTMLButtonElement>('[role="treeitem"]'));
  const idx = items.indexOf(event.currentTarget as HTMLButtonElement);
  if (event.key === 'ArrowDown' && idx >= 0 && idx + 1 < items.length) {
    event.preventDefault();
    items[idx + 1].focus();
  } else if (event.key === 'ArrowUp' && idx > 0) {
    event.preventDefault();
    items[idx - 1].focus();
  } else if (event.key === 'Home' && items.length > 0) {
    event.preventDefault();
    items[0].focus();
  } else if (event.key === 'End' && items.length > 0) {
    event.preventDefault();
    items[items.length - 1].focus();
  } else if (event.key === 'F2') {
    event.preventDefault();
    void renameWorkspaceEntry(path, isDir);
  } else if (event.key === 'Delete') {
    event.preventDefault();
    void deleteWorkspaceEntry(path, isDir);
  }
}

function renderFiles(): void {
  const query = fileFilter.value.trim().toLowerCase();
  const visible = filterTreePaths(state.files, query);
  fileCount.textContent = `${state.files.length} files`;
  fileList.replaceChildren();
  const extraEmptyDirs = query
    ? []
    : [...createdEmptyDirs].filter((dir) => !state.files.some((file) => file.startsWith(`${dir}/`))).sort();
  if (visible.length === 0 && extraEmptyDirs.length === 0) {
    const hint = document.createElement('p');
    hint.className = 'hint';
    hint.textContent = state.files.length === 0 ? 'No files in this workspace.' : 'No matching files.';
    fileList.append(hint);
    return;
  }
  const nodes = buildTree(visible, { expanded: expandedDirectories, changes: workbenchChanges });
  for (const dir of extraEmptyDirs) {
    if (!nodes.some((n) => n.type === 'dir' && n.path === dir)) {
      const parts = dir.split('/');
      nodes.push({ name: parts[parts.length - 1] ?? dir, path: dir, type: 'dir', depth: Math.max(0, parts.length - 1) });
    }
  }
  const fragment = document.createDocumentFragment();
  for (const node of nodes) {
    const row = document.createElement('div');
    row.className = 'tree-row';
    if (node.type === 'dir') {
      const button = document.createElement('button');
      button.type = 'button';
      button.className = 'tree-dir';
      button.setAttribute('role', 'treeitem');
      const expanded = expandedDirectories.has(node.path);
      button.style.paddingLeft = `${4 + node.depth * 12}px`;
      button.setAttribute('aria-expanded', String(expanded));
      const chevron = document.createElement('span');
      chevron.className = 'tree-chevron' + (expanded ? ' open' : '');
      chevron.setAttribute('aria-hidden', 'true');
      const folder = workbenchIcon(expanded ? 'folder-open' : 'folder', 'folder-icon');
      const dirName = document.createElement('span');
      dirName.className = 'tree-label';
      dirName.textContent = node.name;
      button.append(chevron, folder, dirName);
      button.title = node.path;
      button.addEventListener('click', () => {
        if (expandedDirectories.has(node.path)) expandedDirectories.delete(node.path);
        else expandedDirectories.add(node.path);
        renderFiles();
      });
      button.addEventListener('keydown', (event) => handleTreeKeydown(event, node.path, true));
      const actions = document.createElement('div');
      actions.className = 'tree-actions';
      actions.append(
        makeTreeActionBtn(`Rename ${node.path}`, 'pencil', false, () => void renameWorkspaceEntry(node.path, true)),
        makeTreeActionBtn(`Delete ${node.path}`, 'trash-2', true, () => void deleteWorkspaceEntry(node.path, true)),
      );
      row.append(button, actions);
      fragment.append(row);
    } else {
      const button = document.createElement('button');
      button.type = 'button';
      const isSelected = state.current?.path === node.path;
      button.className = 'file-link tree-file'
        + (isSelected ? ' active' : '')
        + (node.change ? ` change-${node.change}` : '');
      button.setAttribute('role', 'treeitem');
      button.setAttribute('aria-selected', String(isSelected));
      button.style.paddingLeft = `${20 + node.depth * 12}px`;
      const fileName = document.createElement('span');
      fileName.className = 'tree-label';
      fileName.textContent = node.name;
      button.append(fileIconElement(node.path), fileName);
      if (node.change) {
        const badge = document.createElement('span');
        badge.className = 'tree-badge';
        badge.textContent = node.change.charAt(0).toUpperCase();
        button.append(badge);
      }
      button.title = node.change ? `${node.path} (${node.change})` : node.path;
      button.addEventListener('click', () => {
        void openFile(node.path);
        if (window.matchMedia('(max-width: 720px)').matches) activateView('editor');
      });
      button.addEventListener('keydown', (event) => handleTreeKeydown(event, node.path, false));
      const actions = document.createElement('div');
      actions.className = 'tree-actions';
      actions.append(
        makeTreeActionBtn(`Rename ${node.path}`, 'pencil', false, () => void renameWorkspaceEntry(node.path, false)),
        makeTreeActionBtn(`Delete ${node.path}`, 'trash-2', true, () => void deleteWorkspaceEntry(node.path, false)),
      );
      row.append(button, actions);
      fragment.append(row);
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

function layoutEditors(): void {
  editor.layout();
  if (splitOpen) secondaryEditor?.layout();
}

function revealEditor(): void {
  editorWorkspace.classList.remove('hidden');
  emptyEditor.classList.add('hidden');
  layoutEditors();
  requestAnimationFrame(layoutEditors);
}

workbenchLayout.addEventListener('transitionend', (event) => {
  if (event.target === workbenchLayout && event.propertyName === 'grid-template-columns') {
    layoutEditors();
  }
});

async function loadFiles(): Promise<void> {
  state.files = await invoke<string[]>('list_files');
  for (const dir of [...createdEmptyDirs]) {
    if (state.files.some((file) => file.startsWith(`${dir}/`))) {
      createdEmptyDirs.delete(dir);
    }
  }
  renderFiles();
}

function promptTextDialog(options: {
  title: string;
  label: string;
  placeholder: string;
  submitLabel: string;
  initialValue?: string;
}): Promise<string | null> {
  return new Promise((resolve) => {
    const returnFocus = document.activeElement instanceof HTMLElement ? document.activeElement : null;
    const dialog = document.createElement('dialog');
    dialog.className = 'merge-dialog';
    dialog.setAttribute('aria-label', options.title);
    const form = document.createElement('form');
    form.method = 'dialog';
    form.className = 'merge-form';
    const heading = document.createElement('h2');
    heading.textContent = options.title;
    const input = document.createElement('input');
    input.type = 'text';
    input.spellcheck = false;
    input.placeholder = options.placeholder;
    input.value = options.initialValue ?? '';
    input.setAttribute('aria-label', options.label);
    const actions = document.createElement('div');
    actions.className = 'merge-actions';
    const cancel = document.createElement('button');
    cancel.type = 'button';
    cancel.className = 'button ghost compact';
    cancel.textContent = 'Cancel';
    const submit = document.createElement('button');
    submit.type = 'submit';
    submit.className = 'button primary compact';
    submit.textContent = options.submitLabel;
    actions.append(cancel, submit);
    form.append(heading, input, actions);
    dialog.append(form);
    let done = false;
    const finish = (value: string | null) => {
      if (done) return;
      done = true;
      dialog.close();
      dialog.remove();
      if (returnFocus && document.contains(returnFocus)) returnFocus.focus();
      resolve(value);
    };
    form.addEventListener('submit', (event) => {
      event.preventDefault();
      const value = input.value.trim();
      finish(value ? value : null);
    });
    cancel.addEventListener('click', () => finish(null));
    dialog.addEventListener('cancel', (event) => {
      event.preventDefault();
      finish(null);
    });
    document.body.append(dialog);
    dialog.showModal();
    input.focus();
    input.select();
  });
}

function confirmActionDialog(options: {
  title: string;
  message: string;
  confirmLabel: string;
  danger?: boolean;
}): Promise<boolean> {
  return new Promise((resolve) => {
    const returnFocus = document.activeElement instanceof HTMLElement ? document.activeElement : null;
    const dialog = document.createElement('dialog');
    dialog.className = 'merge-dialog';
    dialog.setAttribute('aria-label', options.title);
    const form = document.createElement('form');
    form.method = 'dialog';
    form.className = 'merge-form';
    const heading = document.createElement('h2');
    heading.textContent = options.title;
    const body = document.createElement('p');
    body.className = 'hint';
    body.textContent = options.message;
    const actions = document.createElement('div');
    actions.className = 'merge-actions';
    const cancel = document.createElement('button');
    cancel.type = 'button';
    cancel.className = 'button ghost compact';
    cancel.textContent = 'Cancel';
    const confirm = document.createElement('button');
    confirm.type = 'submit';
    confirm.className = options.danger ? 'button danger compact' : 'button primary compact';
    confirm.textContent = options.confirmLabel;
    actions.append(cancel, confirm);
    form.append(heading, body, actions);
    dialog.append(form);
    let done = false;
    const finish = (accepted: boolean) => {
      if (done) return;
      done = true;
      dialog.close();
      dialog.remove();
      if (returnFocus && document.contains(returnFocus)) returnFocus.focus();
      resolve(accepted);
    };
    form.addEventListener('submit', (event) => {
      event.preventDefault();
      finish(true);
    });
    cancel.addEventListener('click', () => finish(false));
    dialog.addEventListener('cancel', (event) => {
      event.preventDefault();
      finish(false);
    });
    document.body.append(dialog);
    dialog.showModal();
    confirm.focus();
  });
}

function normalizeRelativeWorkspacePath(raw: string): string {
  return raw.trim().replace(/\\/g, '/').replace(/^\/+/, '');
}

async function createWorkspaceFile(): Promise<void> {
  if (!requireNativeWorkspace() || !state.root || workspaceSwitching) {
    setStatus('Open a project folder before creating a file.');
    return;
  }
  const entered = await promptTextDialog({
    title: 'New file',
    label: 'New file path',
    placeholder: 'src/example.ts',
    submitLabel: 'Create',
  });
  if (!entered) return;
  const path = normalizeRelativeWorkspacePath(entered);
  if (!path) return;
  try {
    await invoke('create_file', { path });
    const parts = path.split('/');
    for (let i = 1; i < parts.length; i += 1) {
      expandedDirectories.add(parts.slice(0, i).join('/'));
    }
    await loadFiles();
    await openFile(path);
    setStatus(`Created ${path}`);
    void refreshWorkbenchChanges();
  } catch (error) {
    setStatus(`Create file failed: ${error instanceof Error ? error.message : String(error)}`);
  }
}

async function createWorkspaceFolder(): Promise<void> {
  if (!requireNativeWorkspace() || !state.root || workspaceSwitching) {
    setStatus('Open a project folder before creating a folder.');
    return;
  }
  const entered = await promptTextDialog({
    title: 'New folder',
    label: 'New folder path',
    placeholder: 'src/components',
    submitLabel: 'Create',
  });
  if (!entered) return;
  const path = normalizeRelativeWorkspacePath(entered).replace(/\/+$/, '');
  if (!path) return;
  try {
    await invoke('create_directory', { path });
    createdEmptyDirs.add(path);
    const parts = path.split('/');
    for (let i = 1; i <= parts.length; i += 1) {
      expandedDirectories.add(parts.slice(0, i).join('/'));
    }
    await loadFiles();
    setStatus(`Created folder ${path}`);
  } catch (error) {
    setStatus(`Create folder failed: ${error instanceof Error ? error.message : String(error)}`);
  }
}

async function renameWorkspaceEntry(oldPath: string, isDir: boolean): Promise<void> {
  if (!requireNativeWorkspace() || !state.root || workspaceSwitching) return;
  const entered = await promptTextDialog({
    title: isDir ? `Rename folder ${oldPath}` : `Rename ${oldPath}`,
    label: 'New path',
    placeholder: oldPath,
    submitLabel: 'Rename',
    initialValue: oldPath,
  });
  if (!entered) return;
  const newPath = normalizeRelativeWorkspacePath(entered).replace(/\/+$/, '');
  if (!newPath || newPath === oldPath) return;
  try {
    await invoke('rename_path', { oldPath, newPath });
    if (isDir) {
      createdEmptyDirs.delete(oldPath);
      createdEmptyDirs.add(newPath);
      if (expandedDirectories.delete(oldPath)) expandedDirectories.add(newPath);
    }
    const root = state.root;
    const prefix = `${oldPath}/`;
    const affected = [...openFiles.values()].filter((file) =>
      file.path === oldPath || (isDir && file.path.startsWith(prefix)));
    for (const file of affected) {
      const updatedPath = file.path === oldPath ? newPath : `${newPath}/${file.path.slice(prefix.length)}`;
      const updatedUri = fileUri(joinWorkspacePath(root, updatedPath));
      const content = file.model.getValue();
      const viewState = state.current === file ? editor.saveViewState() : file.viewState;
      const languageId = detectEditorLanguage(updatedPath, content, monaco.languages.getLanguages());
      const newModel = monaco.editor.createModel(content, languageId, monaco.Uri.parse(updatedUri));
      const wasActive = state.current === file;
      const wasSecondary = secondaryPath === file.path;
      clearRecoverySnapshot(file.path, file.uri);
      lsp.close(file.uri);
      openFiles.delete(file.uri);
      file.model.dispose();
      const replacement: OpenFile = {
        ...file,
        path: updatedPath,
        uri: updatedUri,
        languageId,
        model: newModel,
        version: file.version + 1,
        viewState,
      };
      openFiles.set(updatedUri, replacement);
      if (!replacement.readOnlyEncoding) {
        lsp.ensureOpen(updatedUri, languageId, replacement.version, content);
      }
      if (wasActive) {
        state.current = replacement;
        editor.setModel(newModel);
        if (viewState) editor.restoreViewState(viewState);
        activePath.textContent = updatedPath;
        languageMode.value = languageId;
      }
      if (wasSecondary) {
        secondaryPath = updatedPath;
        secondaryEditor?.setModel(newModel);
      }
    }
    await loadFiles();
    updateTabs();
    setStatus(`Renamed ${oldPath} to ${newPath}`);
    void refreshWorkbenchChanges();
  } catch (error) {
    setStatus(`Rename failed: ${error instanceof Error ? error.message : String(error)}`);
  }
}

async function deleteWorkspaceEntry(path: string, isDir: boolean): Promise<void> {
  if (!requireNativeWorkspace() || !state.root || workspaceSwitching) return;
  const confirmed = await confirmActionDialog({
    title: isDir ? `Delete folder ${path}?` : `Delete ${path}?`,
    message: isDir
      ? `This permanently deletes folder "${path}" and all files inside it from disk.`
      : `This permanently deletes "${path}" from disk.`,
    confirmLabel: 'Delete',
    danger: true,
  });
  if (!confirmed) return;
  try {
    await invoke('delete_path', { path });
    if (isDir) {
      createdEmptyDirs.delete(path);
      expandedDirectories.delete(path);
    }
    const prefix = `${path}/`;
    const affected = [...openFiles.values()].filter((file) =>
      file.path === path || (isDir && file.path.startsWith(prefix)));
    for (const file of affected) {
      file.dirty = false;
      closeOpenFile(file.path, true);
    }
    await loadFiles();
    setStatus(`Deleted ${path}`);
    void refreshWorkbenchChanges();
  } catch (error) {
    setStatus(`Delete failed: ${error instanceof Error ? error.message : String(error)}`);
  }
}

function ensureSecondaryEditor(): monaco.editor.IStandaloneCodeEditor {
  if (secondaryEditor) return secondaryEditor;
  secondaryEditor = monaco.editor.create(secondaryEditorHost, {
    theme: loadTheme(localStorage).monaco,
    automaticLayout: true,
    fontSize: 13.5,
    lineHeight: 21,
    minimap: { enabled: false },
    scrollBeyondLastLine: false,
    renderWhitespace: 'selection',
    padding: { top: 10 },
    smoothScrolling: !prefersReducedMotion,
  });
  return secondaryEditor;
}

function renderSecondaryFileOptions(): void {
  const files = [...openFiles.values()];
  secondaryFileSelect.replaceChildren();
  for (const file of files) {
    const opt = document.createElement('option');
    opt.value = file.path;
    opt.textContent = file.path;
    secondaryFileSelect.append(opt);
  }
  if (secondaryPath && files.some((f) => f.path === secondaryPath)) {
    secondaryFileSelect.value = secondaryPath;
  }
  splitEditorBtn.disabled = files.length === 0;
}

function setSecondaryFile(path: string): void {
  const file = [...openFiles.values()].find((candidate) => candidate.path === path);
  if (!file) return;
  secondaryPath = file.path;
  const pane = ensureSecondaryEditor();
  pane.updateOptions({ readOnly: !!file.readOnlyEncoding });
  pane.setModel(file.model);
  renderSecondaryFileOptions();
}

function openSplitPane(): void {
  const files = [...openFiles.values()];
  if (files.length === 0) {
    setStatus('Open a file before splitting the editor.');
    return;
  }
  splitOpen = true;
  editorWorkspace.classList.add('split');
  secondaryPane.classList.remove('hidden');
  splitEditorBtn.setAttribute('aria-pressed', 'true');
  const candidate = files.find((file) => file.path !== state.current?.path) ?? state.current ?? files[0];
  setSecondaryFile(candidate.path);
  revealEditor();
  secondaryEditor?.focus();
}

function closeSplitPane(): void {
  if (!splitOpen) return;
  splitOpen = false;
  secondaryPath = null;
  secondaryEditor?.setModel(null);
  secondaryPane.classList.add('hidden');
  editorWorkspace.classList.remove('split');
  splitEditorBtn.setAttribute('aria-pressed', 'false');
  layoutEditors();
  if (state.current) editor.focus();
}

function toggleSplitPane(): void {
  if (splitOpen) closeSplitPane();
  else openSplitPane();
}

newFileBtn.addEventListener('click', () => void createWorkspaceFile());
newFolderBtn.addEventListener('click', () => void createWorkspaceFolder());
splitEditorBtn.addEventListener('click', toggleSplitPane);
closeSplitEditor.addEventListener('click', closeSplitPane);
secondaryFileSelect.addEventListener('change', () => setSecondaryFile(secondaryFileSelect.value));

let recoveryEnabled = loadRecoveryEnabled(localStorage);
const recoverySessionId = createRecoverySessionId();
const recoveryTimers = new Map<string, number>();

function clearRecoverySnapshot(path: string, uri?: string): void {
  if (uri) {
    const timer = recoveryTimers.get(uri);
    if (timer !== undefined) {
      window.clearTimeout(timer);
      recoveryTimers.delete(uri);
    }
  }
  if (!state.root) return;
  void invoke('recovery_clear', { path }).catch(() => undefined);
}

function scheduleRecoverySnapshot(file: OpenFile): void {
  if (!recoveryEnabled || file.readOnlyEncoding || !state.root) return;
  const existing = recoveryTimers.get(file.uri);
  if (existing !== undefined) window.clearTimeout(existing);
  if (!file.dirty) {
    clearRecoverySnapshot(file.path, file.uri);
    return;
  }
  const root = state.root;
  recoveryTimers.set(file.uri, window.setTimeout(() => {
    recoveryTimers.delete(file.uri);
    if (!recoveryEnabled || state.root !== root || !file.dirty || file.readOnlyEncoding) return;
    void invoke('recovery_save', {
      path: file.path,
      fileUri: file.uri,
      content: file.model.getValue(),
      baseContent: file.savedText,
      revision: file.version,
      sessionId: recoverySessionId,
    }).catch(() => undefined);
  }, 1500));
}

async function applyRecoveredEntryToEditor(entry: RecoverableEntry): Promise<void> {
  if (!state.root) return;
  const uri = fileUri(joinWorkspacePath(state.root, entry.path));
  let file = openFiles.get(uri);
  if (!file) {
    if (entry.diskExists) {
      await openFile(entry.path);
      file = openFiles.get(uri);
    }
    if (!file) {
      const languageId = detectEditorLanguage(entry.path, entry.content, monaco.languages.getLanguages());
      const model = monaco.editor.createModel(entry.diskContent, languageId, monaco.Uri.parse(uri));
      file = {
        path: entry.path,
        uri,
        languageId,
        model,
        version: 1,
        savedText: entry.diskContent,
        dirty: false,
      };
      openFiles.set(uri, file);
    }
  }
  if (file.readOnlyEncoding) return;
  // Never silently write to disk: load recovered text into the unsaved editor
  // buffer so the user reviews and saves explicitly.
  file.model.setValue(entry.content);
  file.version += 1;
  markDirty(file);
  selectOpenFile(file.path);
  clearRecoverySnapshot(entry.path, file.uri);
  setStatus(`Recovered unsaved buffer for ${entry.path} (not saved to disk yet).`);
}

function previewRecoveredDiff(entry: RecoverableEntry): void {
  clearDiffEditor();
  const languageId = detectEditorLanguage(entry.path, entry.content, monaco.languages.getLanguages());
  const suffix = encodeURIComponent(`recovery/${entry.path}`).replaceAll('%', '_');
  diffOriginalModel = monaco.editor.createModel(
    entry.diskContent,
    languageId,
    monaco.Uri.parse(`ryoko-diff://recovery/disk/${suffix}`),
  );
  diffModifiedModel = monaco.editor.createModel(
    entry.content,
    languageId,
    monaco.Uri.parse(`ryoko-diff://recovery/buffer/${suffix}`),
  );
  diffEditor.setModel({ original: diffOriginalModel, modified: diffModifiedModel });
  diffReviewPath.textContent = `${entry.path} · Disk vs. Recovered Snapshot`;
  diffPrevFile.hidden = true;
  diffNextFile.hidden = true;
  proposalToolbar.classList.add('hidden');
  editorWorkspace.classList.add('hidden');
  emptyEditor.classList.add('hidden');
  diffReview.classList.remove('hidden');
  diffEditor.layout();
}

async function promptRecoverySnapshots(expectedRoot: string): Promise<void> {
  if (!requireNativeWorkspace()) return;
  let result: RecoveryListResult | null;
  try {
    result = await invoke<RecoveryListResult | null>('recovery_list', {
      currentSessionId: recoverySessionId,
    });
  } catch {
    return;
  }
  if (!result || !Array.isArray(result.entries) || state.root !== expectedRoot) return;
  if (typeof result.corruptedCount === 'number' && result.corruptedCount > 0) {
    setStatus(`Skipped ${result.corruptedCount} corrupted recovery snapshot(s); disk files are untouched.`);
  }
  if (!result.entries.length) return;

  const dialog = document.createElement('dialog');
  dialog.className = 'merge-dialog';
  dialog.setAttribute('aria-label', 'Recover unsaved editor buffers');
  const form = document.createElement('form');
  form.method = 'dialog';
  form.className = 'merge-form';
  const heading = document.createElement('h2');
  heading.textContent = `Recover ${result.entries.length} unsaved buffer(s)`;
  const intro = document.createElement('p');
  intro.className = 'hint';
  intro.textContent =
    'Ryoko found unsaved edits from a previous session. Review the diff before restoring. Cancel or discard leaves disk files untouched.';
  const list = document.createElement('div');
  list.className = 'merge-form';
  for (const entry of result.entries) {
    const row = document.createElement('div');
    row.className = 'settings-actions';
    const label = document.createElement('span');
    label.textContent = describeRecoverableEntry(entry);
    const preview = document.createElement('button');
    preview.type = 'button';
    preview.className = 'button ghost compact';
    preview.textContent = 'Preview diff';
    preview.addEventListener('click', () => {
      dialog.close();
      previewRecoveredDiff(entry);
    });
    const restore = document.createElement('button');
    restore.type = 'button';
    restore.className = 'button primary compact';
    restore.textContent = 'Restore to editor';
    restore.addEventListener('click', () => {
      row.remove();
      void applyRecoveredEntryToEditor(entry);
      if (list.childElementCount === 0) dialog.close();
    });
    const discard = document.createElement('button');
    discard.type = 'button';
    discard.className = 'button ghost compact';
    discard.textContent = 'Discard';
    discard.addEventListener('click', () => {
      row.remove();
      clearRecoverySnapshot(entry.path);
      setStatus(`Discarded recovery snapshot for ${entry.path}. Disk file unchanged.`);
      if (list.childElementCount === 0) dialog.close();
    });
    row.append(label, preview, restore, discard);
    list.append(row);
  }
  const actions = document.createElement('div');
  actions.className = 'settings-actions';
  const cancel = document.createElement('button');
  cancel.type = 'submit';
  cancel.className = 'button ghost compact';
  cancel.textContent = 'Close';
  actions.append(cancel);
  form.append(heading, intro, list, actions);
  dialog.append(form);
  dialog.addEventListener('close', () => dialog.remove());
  document.body.append(dialog);
  dialog.showModal();
}

let onboarding: ReturnType<typeof mountOnboarding> | null = null;

// Local filesystem access is only available through the native Tauri shell.
// A Vite browser preview (or an incomplete webview bridge) must fail closed.
function requireNativeWorkspace(): boolean {
  const bridge = (window as Window & { __TAURI_INTERNALS__?: { invoke?: unknown } }).__TAURI_INTERNALS__;
  if (typeof bridge?.invoke === 'function') return true;
  setStatus('Cannot access local files: the Ryoko desktop connection is missing. Open the installed Ryoko app or run npm run tauri dev from apps/desktop, not npm run dev in a browser.');
  return false;
}

async function openFolder(selectedPath?: string): Promise<void> {
  if (!requireNativeWorkspace()) return;
  if (workspaceSwitching || savingFiles.size) {
    setStatus('Wait for the current workspace operation to finish.');
    return;
  }
  workspaceSwitching = true;
  try {
    // Native picker attached to the main window (see pick_workspace_folder).
    const selected = selectedPath ?? await invoke<string | null>('pick_workspace_folder');
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
    fileOpenGeneration += 1;
    const info = await invoke<WorkspaceInfo>('open_workspace', { path: selected });
    onboarding?.workspaceOpened(info.root);
    void invoke('onboarding_remember_folder', { path: info.root }).catch(() => undefined);
    // Open the new workspace successfully before discarding any old editor
    // buffers. Changing a folder must not erase unsaved content on failure.
    inlineEdit.reject();
    await lsp.stop();
    quickOpen.close();
    await stopTerminal();
    await debugController.resetOnWorkspaceSwitch();
    await stopCodeTask();
    for (const timer of changeTimers.values()) window.clearTimeout(timer);
    changeTimers.clear();
    for (const timer of recoveryTimers.values()) window.clearTimeout(timer);
    recoveryTimers.clear();
    for (const file of openFiles.values()) file.model.dispose();
    openFiles.clear();
    createdEmptyDirs.clear();
    closeSplitPane();
    diagnostics.clear();
    workbenchChanges.clear();
    expandedDirectories.clear();
    changesSummary.textContent = 'no folder';
    changesList.replaceChildren();
    state.current = null;
    updateTabs();
    closeDiffReview();
    agentChat.reset();
    chatInputTokens = 0;
    chatOutputTokens = 0;
    chatEstimatedCostUsd = 0;
    updateUsageMeter();
    editorWorkspace.classList.add('hidden');
    emptyEditor.classList.remove('hidden');
    readOnlyBadge.textContent = '';
    readOnlyBadge.classList.add('hidden');
    missionLogs = [];
    missionSnapshot = null;
    toolStream.show([]);
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
    sessionChanges.clear();
    sessionExpanded.clear();
    sessionDiffTicket += 1;
    sessionDiffContext = null;
    agentSessionList.replaceChildren();
    reviewInboxList.replaceChildren();
    reviewInboxCount.textContent = '0';
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
    runActiveFile.disabled = true;
    activePath.textContent = 'No file open';
    languageMode.value = 'plaintext';
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
    connectors.reload();
    syncWorkspaceButtons();
    void promptRecoverySnapshots(info.root);
    await lsp.start(info.root);
  } catch (error) {
    setStatus(error instanceof Error ? error.message : String(error));
  } finally {
    workspaceSwitching = false;
  }
}

function sessionStateLabel(session: AgentSession): string {
  return session.state === 'active' ? 'working' : session.state;
}

function sessionActionButton(
  label: string,
  title: string,
  className: string,
  run: () => void,
): HTMLButtonElement {
  const button = document.createElement('button');
  button.type = 'button';
  button.className = className;
  button.textContent = label;
  button.title = title;
  button.addEventListener('click', () => {
    button.disabled = true;
    run();
  });
  return button;
}

function renderSessionChanges(
  container: HTMLElement,
  session: AgentSession,
  changes: SessionChange[],
): void {
  container.replaceChildren();
  if (changes.length === 0) {
    const hint = document.createElement('p');
    hint.className = 'hint';
    hint.textContent = 'This published branch has no text changes against its base.';
    container.append(hint);
    return;
  }
  changes.forEach((change, index) => {
    const row = document.createElement('div');
    row.className = 'agent-session-change';
    const status = document.createElement('span');
    status.className = `agent-session-change-status status-${change.status}`;
    status.textContent = changeStatusLabel(change.status);
    const path = document.createElement('code');
    path.className = 'agent-session-change-path';
    path.textContent = change.path;
    path.title = change.path;
    const counts = document.createElement('span');
    counts.className = 'agent-session-change-counts';
    counts.textContent = changeCounts(change);
    const diff = sessionActionButton(
      'Diff',
      'Open a read-only diff of this file on the published branch',
      'button ghost compact',
      () => void openSessionDiff(session, changes, index),
    );
    diff.disabled = change.status === 'binary';
    row.append(status, path, counts, diff);
    container.append(row);
  });
}

async function loadSessionChanges(session: AgentSession, container: HTMLElement): Promise<void> {
  const cached = sessionChanges.get(session.id);
  if (cached) {
    renderSessionChanges(container, session, cached);
    return;
  }
  container.replaceChildren();
  const loading = document.createElement('p');
  loading.className = 'hint';
  loading.textContent = 'Reading the published branch diff…';
  container.append(loading);
  const openedRoot = state.root;
  try {
    const changes = await invoke<SessionChange[]>('list_agent_session_changes', {
      taskId: session.id,
    });
    if (openedRoot !== state.root) return;
    sessionChanges.set(session.id, changes);
    renderSessionChanges(container, session, changes);
  } catch (error) {
    if (openedRoot !== state.root) return;
    const message = error instanceof Error ? error.message : String(error);
    container.replaceChildren();
    const hint = document.createElement('p');
    hint.className = 'hint';
    hint.textContent = 'Could not read the session diff: ' + message;
    container.append(hint);
  }
}

interface MergeOptions {
  target: string;
  checkCommand: string;
}

function promptFolderPath(file = false): Promise<string | null> {
  // Fallback for Linux desktops where the native GTK/portal folder picker
  // never appears (for example some Wayland or tiling window managers).
  return new Promise((resolve) => {
    const dialog = document.createElement('dialog');
    dialog.className = 'merge-dialog';
    dialog.setAttribute('aria-label', file ? 'Open file by path' : 'Open folder by path');
    const form = document.createElement('form');
    form.method = 'dialog';
    form.className = 'merge-form';
    const heading = document.createElement('h2');
    heading.textContent = file ? 'Open file by path' : 'Open folder by path';
    const input = document.createElement('input');
    input.type = 'text';
    input.spellcheck = false;
    input.placeholder = file ? 'src/main.rs or absolute file path' : '/home/you/projects/my-app';
    input.setAttribute('aria-label', file ? 'File path' : 'Absolute folder path');
    const actions = document.createElement('div');
    actions.className = 'merge-actions';
    const cancel = document.createElement('button');
    cancel.type = 'button';
    cancel.className = 'button ghost compact';
    cancel.textContent = 'Cancel';
    const submit = document.createElement('button');
    submit.type = 'submit';
    submit.className = 'button primary compact';
    submit.textContent = 'Open';
    actions.append(cancel, submit);
    form.append(heading, input, actions);
    dialog.append(form);
    let done = false;
    const finish = (value: string | null) => {
      if (done) return;
      done = true;
      dialog.close();
      dialog.remove();
      resolve(value);
    };
    form.addEventListener('submit', (event) => {
      event.preventDefault();
      const value = input.value.trim();
      finish(value ? value : null);
    });
    cancel.addEventListener('click', () => finish(null));
    dialog.addEventListener('cancel', (event) => {
      event.preventDefault();
      finish(null);
    });
    document.body.append(dialog);
    dialog.showModal();
    input.focus();
  });
}

async function openFolderByPath(): Promise<void> {
  if (!requireNativeWorkspace()) return;
  const path = await promptFolderPath();
  if (path) await openFolder(path);
}

async function openWorkspaceFile(byPath = false): Promise<void> {
  if (!requireNativeWorkspace() || workspaceSwitching) return;
  const root = state.root;
  try {
    const selected = byPath
      ? await promptFolderPath(true)
      : await pickFile({
          title: 'Open File',
          directory: false,
          multiple: false,
          ...(root ? { defaultPath: normalizeFilePath(root) } : {}),
        });
    if (typeof selected !== 'string' || workspaceSwitching) return;
    if (root) {
      const relative = workspaceRelativeFile(root, selected);
      if (relative) {
        await openFile(relative);
        return;
      }
    }
    const parentDir = selected.replace(/[\\/][^\\/]+$/, '');
    if (parentDir) {
      await openFolder(parentDir);
      const relative = state.root ? workspaceRelativeFile(state.root, selected) : null;
      if (relative) await openFile(relative);
    }
  } catch (error) {
    const message = 'Could not open file: ' + String(error) + '. Try File: Open File by Path.';
    setStatus(message);
    notify(message);
  }
}

function promptMergeOptions(session: AgentSession): Promise<MergeOptions | null> {
  return new Promise((resolve) => {
    const dialog = document.createElement('dialog');
    dialog.className = 'merge-dialog';
    dialog.setAttribute('aria-label', 'Merge published agent branch');
    const form = document.createElement('form');
    form.method = 'dialog';
    form.className = 'merge-form';
    const heading = document.createElement('h2');
    heading.textContent = `Merge ${session.branch}`;
    const explanation = document.createElement('p');
    explanation.className = 'marketplace-description';
    explanation.textContent =
      `A passing sandboxed check is required. Ryoko merges only if the check passes, then removes the isolated worktree. Sandbox image: ${runSandbox.value.trim()}.`;
    const targetLabel = document.createElement('label');
    targetLabel.textContent = 'Target branch';
    const target = document.createElement('input');
    target.type = 'text';
    target.required = true;
    target.maxLength = 200;
    target.value = !session.baseBranch || session.baseBranch === 'HEAD' ? 'main' : session.baseBranch;
    targetLabel.append(target);
    const checkLabel = document.createElement('label');
    checkLabel.textContent = 'Optional check command';
    const check = document.createElement('input');
    check.type = 'text';
    check.maxLength = 2000;
    check.placeholder = 'Leave empty to auto-detect';
    checkLabel.append(check);
    const note = document.createElement('p');
    note.className = 'marketplace-browser-message';
    const actions = document.createElement('div');
    actions.className = 'merge-actions';
    const cancel = document.createElement('button');
    cancel.type = 'button';
    cancel.className = 'button ghost';
    cancel.textContent = 'Cancel';
    const submit = document.createElement('button');
    submit.type = 'submit';
    submit.className = 'button primary';
    submit.textContent = 'Validate and merge';
    actions.append(cancel, submit);
    form.append(heading, explanation, targetLabel, checkLabel, note, actions);
    dialog.append(form);
    let settled = false;
    const finish = (value: MergeOptions | null): void => {
      if (settled) return;
      settled = true;
      dialog.remove();
      resolve(value);
    };
    form.addEventListener('submit', (event) => {
      event.preventDefault();
      const targetValue = target.value.trim();
      if (!targetValue) {
        note.textContent = 'Enter a target branch.';
        target.focus();
        return;
      }
      dialog.close();
      finish({ target: targetValue, checkCommand: check.value.trim() });
    });
    cancel.addEventListener('click', () => {
      dialog.close();
      finish(null);
    });
    dialog.addEventListener('cancel', (event) => {
      event.preventDefault();
      dialog.close();
      finish(null);
    });
    document.body.append(dialog);
    dialog.showModal();
    target.focus();
  });
}

async function runSessionAction(
  session: AgentSession,
  action: 'release' | 'discard' | 'merge',
): Promise<void> {
  if (action === 'merge') {
    const sandboxImage = runSandbox.value.trim();
    if (!sandboxImage) {
      setStatus('Set a reviewed OCI sandbox image before merging; validation cannot be skipped.');
      renderAgentSessions();
      return;
    }
    const options = await promptMergeOptions(session);
    if (!options) {
      setStatus('Merge cancelled; nothing changed.');
      renderAgentSessions();
      return;
    }
    try {
      const result = await invoke<SessionActionResult>('merge_agent_session', {
        taskId: session.id,
        targetBranch: options.target,
        sandboxImage,
        checkCommand: options.checkCommand || null,
        timeoutSecs: 300,
      });
      if (result.passed === false) {
        setStatus('Merge blocked by validation: ' + (result.summary ?? result.message));
      } else if (result.merge_commit) {
        setStatus(`Merged ${session.branch} at ${result.merge_commit}. ${result.message}`);
      } else {
        setStatus(result.message);
      }
    } catch (error) {
      const message = error instanceof Error ? error.message : String(error);
      if (message.includes('cancelled')) {
        setStatus('Merge cancelled; nothing changed.');
        await refreshAgentSessions();
        return;
      }
      setStatus('Merge failed: ' + message);
    }
  } else {
    try {
      const result = await invoke<SessionActionResult>(
        action === 'release' ? 'release_agent_session' : 'discard_agent_session',
        { taskId: session.id },
      );
      setStatus(result.message);
    } catch (error) {
      const message = error instanceof Error ? error.message : String(error);
      if (message.includes('cancelled')) {
        setStatus(`Session ${action} cancelled; nothing changed.`);
        await refreshAgentSessions();
        return;
      }
      setStatus(`Session ${action} failed: ` + message);
    }
  }
  sessionChanges.delete(session.id);
  sessionExpanded.delete(session.id);
  await refreshAgentSessions();
}

function renderAgentSessions(): void {
  agentSessionList.replaceChildren();
  agentSessionSummary.textContent = agentSessions.length === 1
    ? '1 session'
    : `${agentSessions.length} sessions`;
  if (agentSessions.length === 0) {
    const hint = document.createElement('p');
    hint.className = 'hint';
    hint.textContent = 'No agent sessions yet. Open Mission Control, describe a goal and choose Run.';
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
    if (session.state === 'published') {
      const actions = document.createElement('div');
      actions.className = 'agent-session-actions';
      const expanded = sessionExpanded.has(session.id);
      actions.append(
        sessionActionButton(
          expanded ? 'Hide files' : 'Files & diffs',
          'List changed files and open read-only diffs',
          'button ghost compact',
          () => {
            if (sessionExpanded.has(session.id)) sessionExpanded.delete(session.id);
            else sessionExpanded.add(session.id);
            renderAgentSessions();
          },
        ),
        sessionActionButton(
          'Detach',
          'Remove the isolated worktree from disk but keep its branch',
          'button ghost compact',
          () => void runSessionAction(session, 'release'),
        ),
        sessionActionButton(
          'Discard',
          'Delete the isolated worktree and its branch. Irreversible.',
          'button ghost compact danger',
          () => void runSessionAction(session, 'discard'),
        ),
        sessionActionButton(
          'Merge…',
          'Run the sandboxed check and merge only if it passes',
          'button primary compact',
          () => void runSessionAction(session, 'merge'),
        ),
      );
      item.append(actions);
      if (expanded) {
        const changes = document.createElement('div');
        changes.className = 'agent-session-changes';
        item.append(changes);
        void loadSessionChanges(session, changes);
      }
    }
    agentSessionList.append(item);
  }
}

interface AgentCandidate {
  path: string;
  branch: string;
  content: string;
}

function renderReviewInbox(): void {
  reviewInboxList.replaceChildren();
  const published = agentSessions.filter((session) => session.state === 'published');
  const count = published.reduce((sum, session) => sum + session.files.length, 0);
  reviewInboxCount.textContent = String(count);
  if (count === 0) {
    const hint = document.createElement('p');
    hint.className = 'hint';
    hint.textContent = 'Published agent branches awaiting review will appear here. Nothing is merged automatically.';
    reviewInboxList.append(hint);
    return;
  }
  for (const session of published) {
    const item = document.createElement('article');
    item.className = 'review-inbox-item';
    const title = document.createElement('p');
    title.className = 'review-inbox-heading';
    title.textContent = `${session.agent} · ${session.branch}`;
    item.append(title);
    for (const path of session.files) {
      const row = document.createElement('div');
      row.className = 'review-inbox-path';
      const label = document.createElement('code');
      label.textContent = path;
      const button = document.createElement('button');
      button.type = 'button';
      button.className = 'button ghost compact';
      button.textContent = 'Review diff';
      button.addEventListener('click', () => void previewAgentCandidate(session, path));
      row.append(label, button);
      item.append(row);
    }
    reviewInboxList.append(item);
  }
}

async function previewAgentCandidate(session: AgentSession, path: string): Promise<void> {
  if (!state.root) return;
  const root = state.root;
  try {
    const candidate = await invoke<AgentCandidate>('read_agent_candidate', {
      taskId: session.id, path,
    });
    if (root !== state.root) return;
    if (!state.files.includes(path)) {
      setStatus('New agent-created files require Git review; only existing files can be applied through the editor.');
      return;
    }
    await openFile(path);
    const file = state.current;
    if (root !== state.root || !file || file.path !== path) return;
    const original = file.model.getValue();
    openSuggestedReview({
      context: {
        path,
        text: original,
        fullText: original,
        startOffset: 0,
        endOffset: original.length,
      },
      replacement: candidate.content,
    });
  } catch (error) {
    setStatus('Agent proposal review failed: ' + (error instanceof Error ? error.message : String(error)));
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
    const live = new Set(sessions.map((session) => session.id));
    for (const key of sessionChanges.keys()) {
      if (!live.has(key)) sessionChanges.delete(key);
    }
    for (const key of sessionExpanded) {
      if (!live.has(key)) sessionExpanded.delete(key);
    }
    renderAgentSessions();
    renderReviewInbox();
  } catch (error) {
    agentSessions = [];
    renderReviewInbox();
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
    hunkCount.textContent = String(
      snapshot.hunks.length + snapshot.files.filter((file) => !file.editable).length,
    );
    for (const file of snapshot.files) {
      if (file.editable) continue;
      const item = document.createElement('div');
      item.className = 'hunk-item hunk-file-lock';
      const title = document.createElement('span');
      title.className = 'hunk-title';
      title.textContent = '[' + file.state + '] ' + file.path + ' — ' + file.stateLabel;
      item.append(title);
      hunkList.append(item);
    }
    if (snapshot.hunks.length === 0) {
      const hint = document.createElement('p');
      hint.className = 'hint';
      hint.textContent = snapshot.files.some((file) => !file.editable)
        ? 'Only the non-editable changes above are present. Selectable text hunks appear here after refresh.'
        : 'No unstaged selectable text hunks. Binary, rename and mode changes stay listed above.';
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
        void openHunkDiff(hunk, snapshot, openedRoot);
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
    const report = await invoke<ReviewActionReport>('stage_review_hunks', {
      ids,
      expectedPatch: gitHunks.patch,
    });
    void refreshWorkbenchChanges();
    const files = report.files.map((file) => file.path).join(', ');
    setStatus(
      'Staged ' + ids.length + ' selected text hunk(s) in Git index (files: ' + files
      + '). No commit or merge was made.',
    );
    await refreshHunks();
  } catch (error) {
    setStatus('Staging denied or failed: ' + (error instanceof Error ? error.message : String(error)));
    updateHunkStageButton();
  }
}

function openSuggestedReview(suggestion: CodeSuggestion): void {
  const { context, replacement } = suggestion;
  const file = state.current;
  if (!file || file.path !== context.path || file.model.getValue() !== context.fullText
    || context.fullText.slice(context.startOffset, context.endOffset) !== context.text) {
    setStatus('Proposal is stale or its source file changed. Reopen the edit instead.');
    return;
  }
  if (replacement.length > 256 * 1024 || context.fullText.length > 256 * 1024) {
    setStatus('Proposal exceeds the 256 KiB diff review limit.');
    return;
  }
  const hunks = proposalHunks(context.text, replacement);
  if (!hunks.length) {
    setStatus('The suggestion does not change the selected code.');
    return;
  }
  clearDiffEditor();
  pendingProposal = { context, hunks, accepted: new Set(hunks.map((hunk) => hunk.id)) };
  const suffix = encodeURIComponent(context.path).replaceAll('%', '_');
  diffOriginalModel = monaco.editor.createModel(
    context.fullText, file.languageId, monaco.Uri.parse(`ryoko-proposal://base/${suffix}`),
  );
  diffModifiedModel = monaco.editor.createModel(
    previewAccepted(context.fullText, context.startOffset, context.endOffset, hunks, pendingProposal.accepted),
    file.languageId, monaco.Uri.parse(`ryoko-proposal://suggested/${suffix}`),
  );
  diffEditor.setModel({ original: diffOriginalModel, modified: diffModifiedModel });
  proposalToolbar.classList.remove('hidden');
  proposalHunksNode.replaceChildren();
  for (const hunk of hunks) {
    const item = document.createElement('div');
    item.className = 'proposal-hunk';
    const checkbox = document.createElement('input');
    checkbox.type = 'checkbox';
    checkbox.checked = true;
    checkbox.id = 'proposal-hunk-' + hunk.id;
    const label = document.createElement('label');
    label.htmlFor = checkbox.id;
    label.textContent = `Hunk ${hunk.id}: -${hunk.before.split('\n').filter(Boolean).length} +${hunk.after.split('\n').filter(Boolean).length}`;
    checkbox.addEventListener('change', () => {
      if (!pendingProposal) return;
      if (checkbox.checked) pendingProposal.accepted.add(hunk.id);
      else pendingProposal.accepted.delete(hunk.id);
      proposalApply.disabled = pendingProposal.accepted.size === 0;
      try {
        diffModifiedModel?.setValue(previewAccepted(
          context.fullText, context.startOffset, context.endOffset,
          pendingProposal.hunks, pendingProposal.accepted,
        ));
      } catch (error) { setStatus(String(error)); }
    });
    item.append(checkbox, label);
    proposalHunksNode.append(item);
  }
  proposalApply.disabled = false;
  diffReviewPath.textContent = `${context.path} · proposed code (not applied)`;
  diffReview.classList.remove('hidden');
  editorWorkspace.classList.add('hidden');
  emptyEditor.classList.add('hidden');
  diffEditor.layout();
  setStatus(`Review ${hunks.length} suggested hunk(s). Uncheck any changes you reject.`);
}

function applyApprovedProposal(): void {
  const proposal = pendingProposal;
  const file = state.current;
  if (!proposal || !file || file.path !== proposal.context.path
    || file.model.getValue() !== proposal.context.fullText) {
    setStatus('The editor changed after review began; proposed code was not applied.');
    closeDiffReview();
    return;
  }
  if (!proposal.accepted.size) return;
  let updated: string;
  try {
    updated = previewAccepted(proposal.context.fullText, proposal.context.startOffset,
      proposal.context.endOffset, proposal.hunks, proposal.accepted);
  } catch (error) {
    setStatus('Unsafe or stale diff: ' + String(error));
    return;
  }
  file.model.pushEditOperations(
    [],
    [{ range: file.model.getFullModelRange(), text: updated }],
    () => null,
  );
  const count = proposal.accepted.size;
  closeDiffReview();
  setStatus(`Applied ${count} approved hunk(s) to the editor buffer only. Save explicitly to write to disk.`);
}

function clearDiffEditor(): void {
  sessionDiffTicket += 1;
  sessionDiffContext = null;
  diffPrevFile.hidden = true;
  diffNextFile.hidden = true;
  pendingProposal = null;
  proposalToolbar.classList.add('hidden');
  proposalHunksNode.replaceChildren();
  proposalApply.disabled = true;
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
    const languageId = detectEditorLanguage(path, review.modified, monaco.languages.getLanguages());
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
    editorWorkspace.classList.add('hidden');
    emptyEditor.classList.add('hidden');
    diffEditor.layout();
    setStatus(`Reviewing ${path} against HEAD`);
  } catch (error) {
    setStatus('Diff review failed: ' + (error instanceof Error ? error.message : String(error)));
  }
}

async function openSessionDiffAt(index: number): Promise<void> {
  const context = sessionDiffContext;
  if (!context || context.changes.length === 0) return;
  const next = Math.max(0, Math.min(index, context.changes.length - 1));
  const session = agentSessions.find((entry) => entry.id === context.taskId);
  if (!session) {
    setStatus('The agent session disappeared while reviewing its diff.');
    return;
  }
  await openSessionDiff(session, context.changes, next);
}

async function openSessionDiff(
  session: AgentSession,
  changes: SessionChange[],
  index: number,
): Promise<void> {
  if (!state.root) return;
  if ([...openFiles.values()].some((file) => file.dirty)) {
    setStatus('Save or discard unsaved Monaco edits before reviewing an agent branch.');
    return;
  }
  const change = changes[index];
  if (!change) return;
  const openedRoot = state.root;
  const ticket = ++sessionDiffTicket;
  try {
    const review = await invoke<SessionFileDiff>('read_agent_session_file', {
      taskId: session.id,
      path: change.path,
    });
    if (openedRoot !== state.root || ticket !== sessionDiffTicket) return;
    clearDiffEditor();
    sessionDiffContext = { taskId: session.id, branch: session.branch, changes, index };
    const languageId = detectEditorLanguage(change.path, review.modified, monaco.languages.getLanguages());
    const suffix = encodeURIComponent(`${session.id}/${change.path}`).replaceAll('%', '_');
    diffOriginalModel = monaco.editor.createModel(
      review.original,
      languageId,
      monaco.Uri.parse(`ryoko-session://original/${suffix}`),
    );
    diffModifiedModel = monaco.editor.createModel(
      review.modified,
      languageId,
      monaco.Uri.parse(`ryoko-session://modified/${suffix}`),
    );
    diffEditor.setModel({ original: diffOriginalModel, modified: diffModifiedModel });
    diffReviewPath.textContent = `${session.branch} · ${change.path} (${index + 1}/${changes.length})`;
    diffReview.classList.remove('hidden');
    editorWorkspace.classList.add('hidden');
    emptyEditor.classList.add('hidden');
    diffEditor.layout();
    diffPrevFile.hidden = false;
    diffNextFile.hidden = false;
    diffPrevFile.disabled = index <= 0;
    diffNextFile.disabled = index >= changes.length - 1;
    setStatus(`Reviewing ${change.path} on ${session.branch} (read-only)`);
  } catch (error) {
    setStatus('Session diff failed: ' + (error instanceof Error ? error.message : String(error)));
  }
}

function closeDiffReview(): void {
  clearDiffEditor();
  diffReview.classList.add('hidden');
  if (state.current) {
    revealEditor();
  } else {
    editorWorkspace.classList.add('hidden');
    emptyEditor.classList.remove('hidden');
  }
}

function hunkWidget(hunk: GitHunk, snapshot: GitHunkSnapshot, root: string): monaco.editor.IContentWidget {
  const dom = document.createElement('div');
  dom.className = 'hunk-inline-actions';
  const accept = document.createElement('button');
  accept.type = 'button';
  accept.className = 'button primary compact';
  accept.textContent = 'Accept hunk';
  accept.title = 'Stage only this hunk after native confirmation';
  accept.addEventListener('click', () => void actOnHunk('accept', hunk, snapshot, root));
  const reject = document.createElement('button');
  reject.type = 'button';
  reject.className = 'button ghost compact';
  reject.textContent = 'Reject hunk';
  reject.title = 'Revert only this hunk from the working files after native confirmation';
  reject.addEventListener('click', () => void actOnHunk('reject', hunk, snapshot, root));
  const revise = document.createElement('button');
  revise.type = 'button';
  revise.className = 'button ghost compact';
  revise.textContent = 'Ask agent to revise';
  revise.title = 'Start a new validated worktree run with this hunk as context';
  revise.addEventListener('click', () => void actOnHunk('revise', hunk, snapshot, root));
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

async function openHunkDiff(hunk: GitHunk, snapshot: GitHunkSnapshot, root: string): Promise<void> {
  if (state.root !== root) return;
  if ([...openFiles.values()].some((file) => file.dirty)) {
    setStatus('Save or discard unsaved Monaco edits before reviewing hunks.');
    return;
  }
  try {
    const review = await invoke<ReviewFileContent>('read_review_file', { path: hunk.path });
    const current = await invoke<GitHunkSnapshot>('list_review_hunks');
    if (state.root !== root || current.patch !== snapshot.patch) {
      setStatus('Review changed while opening the diff. Refresh and reselect the hunk.');
      return;
    }
    clearDiffEditor();
    const languageId = detectEditorLanguage(hunk.path, review.modified, monaco.languages.getLanguages());
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
    editorWorkspace.classList.add('hidden');
    emptyEditor.classList.add('hidden');
    diffEditor.layout();
    const modified = diffEditor.getModifiedEditor();
    for (const candidate of snapshot.hunks.filter((item) => item.path === hunk.path)) {
      const widget = hunkWidget(candidate, snapshot, root);
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

async function actOnHunk(
  action: 'accept' | 'reject' | 'revise', hunk: GitHunk, snapshot: GitHunkSnapshot, root: string,
): Promise<void> {
  if (state.root !== root) {
    setStatus('Workspace changed. Reopen the diff before acting.');
    return;
  }
  if ([...openFiles.values()].some((file) => file.dirty)) {
    setStatus('Save or discard unsaved Monaco edits before applying hunks.');
    return;
  }
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
      const report = await invoke<ReviewActionReport>('stage_review_hunks', {
        ids: [hunk.id],
        expectedPatch: snapshot.patch,
      });
      void refreshWorkbenchChanges();
      setStatus(
        `Accepted hunk #${hunk.id} in ${report.files.map((file) => file.path).join(', ') || hunk.path}`
        + ' into the Git index. No commit or merge was made.',
      );
    } else {
      const report = await invoke<ReviewActionReport>('reject_review_hunks', {
        ids: [hunk.id],
        expectedPatch: snapshot.patch,
      });
      void refreshWorkbenchChanges();
      setStatus(
        `Rejected hunk #${hunk.id} in ${report.files.map((file) => file.path).join(', ') || hunk.path}`
        + '; only those working-file edits were reverted.',
      );
    }
    await refreshHunks();
    // IDs can be reassigned after a mutation; require an explicit new selection.
    closeDiffReview();
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
  if (status.eventLog === missionLogSelect.value && (!status.running || status.paused)) {
    toolStream.show([]);
  }
  runStart.disabled = status.running;
  runPause.disabled = !status.running;
  const pauseLabel = status.paused ? 'Resume' : 'Pause';
  if (runPause.textContent !== pauseLabel) {
    runPause.replaceChildren(workbenchIcon(status.paused ? 'play' : 'pause', 'control-icon'), pauseLabel);
  }
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
      toolStream.show([]);
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
    updateUsageMeter();
    renderMission();
  } catch (error) {
    toolStream.show([]);
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
  const inactive = runSnapshot?.eventLog === missionLogSelect.value
    && (!runSnapshot.running || runSnapshot.paused);
  toolStream.show(inactive ? [] : activeMissionTools(snapshot));
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
  updateUsageMeter();
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
    const usageLine = document.createElement('span');
    usageLine.className = 'mission-node-usage';
    const tokenCount = (node.task.usage?.inputTokens ?? 0) + (node.task.usage?.outputTokens ?? 0);
    usageLine.textContent = `${formatTokens(tokenCount)} tok · ${formatLedgerCost(node.task.usage?.ledgerCostUsd ?? 0)}`;
    const filesLine = document.createElement('span');
    filesLine.className = 'mission-node-usage';
    const touched = node.task.pullRequest?.files
      ?? agentSessions.find((session) => session.id === node.task.id)?.files
      ?? [];
    filesLine.textContent = touched.slice(0, 2).join(' · ') || 'No files published';
    button.append(role, label, description, usageLine, filesLine);
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
  if (task.pendingTool && activeMissionTools(missionSnapshot).some((entry) => entry.taskId === task.id)) {
    addDetailRow(missionDetail, 'Last recorded pending tool request', task.pendingTool);
  }
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

function selectOpenFile(path: string): void {
  const file = [...openFiles.values()].find((candidate) => candidate.path === path);
  if (!file) return;
  fileOpenGeneration += 1;
  if (state.current && state.current !== file) inlineEdit.reject();
  if (state.current) state.current.viewState = editor.saveViewState();
  if (!diffReview.classList.contains('hidden')) closeDiffReview();
  state.current = file;
  hideFim();
  editor.updateOptions({ readOnly: !!file.readOnlyEncoding });
  editor.setModel(file.model);
  if (file.viewState) editor.restoreViewState(file.viewState);
  if (window.matchMedia('(max-width: 720px)').matches) activateView('editor');
  else workbenchLayout.classList.remove('hide-inspector');
  revealEditor();
  languageMode.value = file.languageId;
  configureTaskForPath(path);
  runActiveFile.disabled = codeSessionId !== null || !!file.readOnlyEncoding;
  activePath.textContent = file.readOnlyEncoding ? `${file.path} · ${file.readOnlyEncoding} · Read-only` : file.path;
  readOnlyBadge.textContent = file.readOnlyEncoding ? `Read-only · ${file.readOnlyEncoding}` : '';
  readOnlyBadge.classList.toggle('hidden', !file.readOnlyEncoding);
  dirtyFlag.classList.toggle('hidden', !file.dirty);
  saveButton.disabled = !!file.readOnlyEncoding;
  renderFiles();
  updateTabs();
  updateCursor();
  setStatus(file.readOnlyEncoding ? `Read-only preview: ${file.path} (${file.readOnlyEncoding}). Original bytes are unchanged.` : file.path);
  quickOpen.remember(file.path);
  editor.focus();
}

function closeOpenFile(path: string, skipConfirm = false): void {
  const file = [...openFiles.values()].find((candidate) => candidate.path === path);
  if (!file) return;
  if (savingFiles.has(file.uri)) { setStatus('Wait for this file to finish saving.'); return; }
  if (!skipConfirm && file.dirty && !window.confirm(`Discard unsaved changes to ${file.path}?`)) return;
  fileOpenGeneration += 1;
  const next = selectionAfterClose([...openFiles.values()].map((item) => item.path), state.current?.path ?? null, path);
  const wasActive = state.current === file;
  const wasSecondary = secondaryPath === file.path;
  const timer = changeTimers.get(file.uri);
  if (timer !== undefined) window.clearTimeout(timer);
  changeTimers.delete(file.uri);
  clearRecoverySnapshot(file.path, file.uri);
  lsp.close(file.uri);
  openFiles.delete(file.uri);
  if (wasActive) {
    inlineEdit.reject();
    hideFim();
    state.current = null;
    editor.setModel(null);
    closeDiffReview();
  }
  if (wasSecondary) {
    const remaining = [...openFiles.values()][0];
    if (remaining && splitOpen) setSecondaryFile(remaining.path);
    else closeSplitPane();
  }
  file.model.dispose();
  if (wasActive && next) selectOpenFile(next);
  else if (wasActive) {
    editorWorkspace.classList.add('hidden');
    emptyEditor.classList.remove('hidden');
    activePath.textContent = 'No file open';
    readOnlyBadge.textContent = '';
    readOnlyBadge.classList.add('hidden');
    dirtyFlag.classList.add('hidden');
    saveButton.disabled = true;
    languageMode.value = 'plaintext';
    configureTaskForPath('');
    runActiveFile.disabled = true;
    cursorPos.textContent = '';
    setStatus('Ready');
  }
  updateTabs();
  renderFiles();
}

async function openFile(path: string): Promise<void> {
  if (!state.root || workspaceSwitching) return;
  const root = state.root;
  const uri = fileUri(joinWorkspacePath(root, path));
  // Switching an existing tab is a buffer operation, not a disk reload.
  // This also keeps a dirty buffer accessible after its disk file is removed.
  if (openFiles.has(uri)) { selectOpenFile(path); return; }
  if (!requireNativeWorkspace()) return;
  const ticket = ++fileOpenGeneration;
  try {
    setStatus(`Opening ${path}…`);
    let content: string;
    let encoding: string | undefined;
    let readOnlyEncoding: string | undefined;
    try {
      content = await invoke<string>('read_file', { path });
    } catch (error) {
      const message = String(error);
      if (!message.includes('as UTF-8:') && !message.includes('binary or unsupported text encoding:')) throw error;
      const preview = await invoke<{
        content: string;
        encoding: string;
        editable?: boolean;
        encodingId?: string;
      }>('read_encoded_file_preview', { path });
      content = preview.content;
      if (preview.editable === true && preview.encodingId) {
        encoding = preview.encodingId;
      } else {
        readOnlyEncoding = preview.encoding;
      }
    }
    if (ticket !== fileOpenGeneration || state.root !== root || workspaceSwitching) return;
    const languageId = detectEditorLanguage(path, content, monaco.languages.getLanguages());
    const modelUri = monaco.Uri.parse(uri);
    monaco.editor.getModel(modelUri)?.dispose();
    const model = monaco.editor.createModel(content, languageId, modelUri);
    const file: OpenFile = {
      path,
      uri,
      languageId,
      model,
      version: 1,
      savedText: content,
      encoding,
      readOnlyEncoding,
      dirty: false,
    };
    openFiles.set(uri, file);
    if (!readOnlyEncoding) lsp.ensureOpen(uri, languageId, file.version, content);
    selectOpenFile(path);
  } catch (error) {
    if (ticket === fileOpenGeneration) {
      const message = error instanceof Error ? error.message : String(error);
      setStatus(message);
      notify(message);
    }
  }
}

function updateCursor(): void {
  const position = editor.getPosition();
  if (position) cursorPos.textContent = `Ln ${position.lineNumber}, Col ${position.column}`;
}

async function saveCurrent(): Promise<void> {
  const file = state.current;
  if (!file || file.readOnlyEncoding || !file.dirty || savingFiles.has(file.uri) || workspaceSwitching) return;
  savingFiles.add(file.uri);
  try {
    const content = file.model.getValue();
    await invoke('write_file', {
      path: file.path,
      content,
      expectedContent: file.savedText,
      ...(file.encoding ? { encoding: file.encoding } : {}),
    });
    file.savedText = content;
    file.version += 1;
    markDirty(file);
    clearRecoverySnapshot(file.path, file.uri);
    lsp.change(file.uri, file.version, content);
    renderFiles();
    setStatus(`Saved ${file.path}`);
    void refreshWorkbenchChanges();
    void refreshHunks();
  } catch (error) {
    setStatus(`Save failed: ${error instanceof Error ? error.message : String(error)}`);
  } finally {
    savingFiles.delete(file.uri);
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
  const root = normalizeFilePath(state.root).replace(/\/+$/, '');
  const candidate = normalizeFilePath(absolute);
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
  if (!file || file.readOnlyEncoding) return;
  scheduleFim();
  file.version += 1;
  markDirty(file);
  scheduleRecoverySnapshot(file);
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
element<HTMLButtonElement>('welcomeOpenPath').addEventListener('click', () => {
  void openFolderByPath();
});
saveButton.addEventListener('click', () => {
  void saveCurrent();
});
fileFilter.addEventListener('input', renderFiles);
element('railSearch').addEventListener('click', () => { activateView('editor'); showBottom('search'); });
element('railExtensions').addEventListener('click', () => element('openMarketplace').click());
element('quickOpenButton').addEventListener('click', () => quickOpen.open());
element('welcomeQuickOpen').addEventListener('click', () => quickOpen.open());
element('welcomeSettings').addEventListener('click', () => openSettings.click());
element('welcomeMission').addEventListener('click', () => activateView('mission'));

// Actions that only make sense inside an open workspace are disabled until one
// is open, so they never look clickable while silently doing nothing.
const workspaceOnlyButtons = ['refreshFiles', 'agentSessionRefresh', 'missionRefresh', 'hunkRefresh', 'changesRefresh']
  .map((id) => element<HTMLButtonElement>(id));
function syncWorkspaceButtons(): void {
  const open = Boolean(state.root);
  for (const button of workspaceOnlyButtons) {
    if (!open) button.disabled = true;
    else if (button.dataset.workspaceGated === 'off') button.disabled = false;
    button.dataset.workspaceGated = open ? 'on' : 'off';
    button.title = open ? '' : 'Open a folder first';
  }
}
syncWorkspaceButtons();

element('refreshFiles').addEventListener('click', () => {
  if (state.root) void loadFiles().catch((error) => setStatus(String(error)));
});

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
diffPrevFile.addEventListener('click', () => {
  if (sessionDiffContext) void openSessionDiffAt(sessionDiffContext.index - 1);
});
diffNextFile.addEventListener('click', () => {
  if (sessionDiffContext) void openSessionDiffAt(sessionDiffContext.index + 1);
});
proposalApply.addEventListener('click', applyApprovedProposal);
proposalDiscard.addEventListener('click', () => {
  closeDiffReview();
  setStatus('Discarded agent proposal without changing the editor.');
});
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
  toolStream.show([]);
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
    case 'workbench.debug':
      showBottom('debug');
      break;
    case 'workbench.settings':
      openSettings.click();
      break;
    case 'editor.close':
      if (state.current) closeOpenFile(state.current.path);
      break;
    case 'editor.inlineEdit':
      inlineEdit.open();
      break;
    default:
      void commands.execute(action);
      break;
  }
}

window.addEventListener('keydown', (event) => {
  if (event.repeat) return;
  if (event.key === 'F5' && !event.shiftKey && !event.ctrlKey && !event.metaKey) {
    if (debugController.getState().running) {
      event.preventDefault();
      void debugController.dapStep('continue');
      return;
    }
  } else if (event.key === 'F10' && !event.ctrlKey && !event.metaKey) {
    if (debugController.getState().running) {
      event.preventDefault();
      void debugController.dapStep('next');
      return;
    }
  } else if (event.key === 'F11' && !event.shiftKey && !event.ctrlKey && !event.metaKey) {
    if (debugController.getState().running) {
      event.preventDefault();
      void debugController.dapStep('stepIn');
      return;
    }
  } else if (event.key === 'F11' && event.shiftKey) {
    if (debugController.getState().running) {
      event.preventDefault();
      void debugController.dapStep('stepOut');
      return;
    }
  } else if (event.key === 'F9') {
    event.preventDefault();
    void debugController.toggleCurrentBreakpoint();
    return;
  }
  const chord = chordForEvent(event);
  const inEditor = event.target instanceof Node && editorHost.contains(event.target);
  if (shouldDeferToEditor(keymapPreset, chord, inEditor)) return;
  const action = keybindings.byChord.get(chord);
  if (!action) return;
  if (action === 'editor.save') {
    if (inEditor) return;
    event.preventDefault();
    void saveCurrent();
    return;
  }
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
taskTab.addEventListener('click', () => showBottom('task'));
taskRun.addEventListener('click', () => void startCodeTask());
taskStop.addEventListener('click', () => void stopCodeTask());
runActiveFile.addEventListener('click', () => {
  if (!state.current) return;
  configureTaskForPath(state.current.path);
  void startCodeTask();
});
terminalStart.addEventListener('click', () => {
  void startTerminal();
});
terminalStop.addEventListener('click', () => {
  void stopTerminal();
});

void listen<CodeRunOutput>('ryoko-code-output', (event) => {
  if (event.payload.sessionId === codeSessionId) {
    appendTaskOutput(event.payload.data);
  } else if (codeStarting && codeSessionId === null) {
    const bytes = pendingCodeOutput.reduce((sum, item) => sum + item.data.length, 0);
    if (bytes + event.payload.data.length <= 64 * 1024) pendingCodeOutput.push(event.payload);
  }
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
void listen<{ sessionId: number }>('ryoko-terminal-exit', (event) => {
  if (terminalSessionId !== null && event.payload.sessionId === terminalSessionId) {
    terminalSessionId = null;
    terminalStart.disabled = false;
    terminalStop.disabled = true;
    bottomStatus.textContent = 'Shell exited';
    xterm?.write('\r\n[Shell exited]\r\n');
  }
});
void listen<FimChunk>('ryoko-fim-chunk', (event) => applyFimChunk(event.payload));
void listen<FimError>('ryoko-fim-error', (event) => {
  if (event.payload.requestId !== String(fimGeneration)) return;
  hideFim(false);
  setStatus(`Completion unavailable: ${event.payload.message}`);
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
  debugController.pollStatus();
}

window.setInterval(() => {
  if (state.root && document.visibilityState === 'visible') {
    void refreshMissionLogs();
    void refreshRunStatus();
  }
}, 1200);

window.setInterval(() => {
  if (state.root && document.visibilityState === 'visible') {
    void refreshAgentSessions();
  }
}, 5000);

window.setInterval(() => {
  if (document.visibilityState === 'visible') {
    void pollTerminalStatus();
    void pollDebugStatus();
    void pollCodeStatus();
  }
}, 2500);

updateRunLabels();
refreshRunStatus();

window.addEventListener('beforeunload', () => {
  void invoke('recovery_retire_session', { sessionId: recoverySessionId }).catch(() => undefined);
  void lsp.stop();
  debugController.unmount();
  void invoke('terminal_stop').catch(() => undefined);
  void invoke('dap_stop').catch(() => undefined);
  void invoke('code_run_stop').catch(() => undefined);
});

setLspState('offline');
renderMissionOptions();
mountGithubQueue(setStatus);
const connectors = mountConnectors(setStatus);
mountMarketplaceBrowser(setStatus);
mountSearchPanel({ openFile, setStatus });
mountSettingsDialog({ setStatus });
onboarding = mountOnboarding({
  openFolder: () => openFolder(),
  openFolderPath: (path) => openFolder(path),
  openMission: () => activateView('mission'),
  hasWorkspace: () => Boolean(state.root),
  prepareTask: (goal) => { runGoal.value = goal; },
  notify: setStatus,
});
const registeredLanguages = new Set(monaco.languages.getLanguages().map((language) => language.id));
for (const language of LANGUAGE_IDS) {
  if (!registeredLanguages.has(language)) monaco.languages.register({ id: language });
}
const availableLanguages = [...new Set(monaco.languages.getLanguages().map((language) => language.id))]
  .sort();
for (const id of availableLanguages) {
  const option = document.createElement('option');
  option.value = id;
  option.textContent = id;
  languageMode.append(option);
}
const customLanguageOption = document.createElement('option');
customLanguageOption.value = '__custom__';
customLanguageOption.textContent = 'Custom language…';
languageMode.append(customLanguageOption);
languageMode.value = 'plaintext';
languageMode.addEventListener('change', () => {
  const file = state.current;
  if (!file) { languageMode.value = 'plaintext'; return; }
  let newLanguage = languageMode.value;
  if (newLanguage === '__custom__') {
    const entered = window.prompt(
      'Custom LSP language ID (lowercase letters, digits, - or +):',
      file.languageId,
    );
    if (entered === null) { languageMode.value = file.languageId; return; }
    const requested = entered.trim().toLowerCase();
    if (!/^[a-z0-9][a-z0-9+-]{0,31}$/.test(requested)) {
      languageMode.value = file.languageId;
      setStatus('Language IDs must be 1–32 lowercase letters, digits, - or +.');
      return;
    }
    if (!monaco.languages.getLanguages().some((mode) => mode.id === requested)) {
      monaco.languages.register({ id: requested });
      registerMonacoCapabilities(monaco, lsp, [requested]);
    }
    if (!Array.from(languageMode.options).some((option) => option.value === requested)) {
      const option = document.createElement('option');
      option.value = requested;
      option.textContent = requested;
      languageMode.insertBefore(option, customLanguageOption);
    }
    newLanguage = requested;
    languageMode.value = requested;
  }
  if (file.languageId === newLanguage) return;
  lsp.close(file.uri);
  monaco.editor.setModelLanguage(file.model, newLanguage);
  file.languageId = newLanguage;
  file.version += 1;
  if (!file.readOnlyEncoding) lsp.ensureOpen(file.uri, file.languageId, file.version, file.model.getValue());
  setStatus('Language mode: ' + newLanguage + ' (syntax support depends on installed language tooling)');
});
registerMonacoCapabilities(monaco, lsp, availableLanguages);

// ---------------------------------------------------------------------------
// Workbench chrome: commands, menus, resizable layout, breadcrumbs, themes.
// ---------------------------------------------------------------------------

const isMac = /Mac|iPhone|iPad/.test(navigator.platform);
const commands = new CommandRegistry();
let currentTheme: ThemeDefinition = initialTheme;

function shortcutFor(id: string): string | null {
  if (id === 'editor.save') return formatChord('MOD+S', isMac);
  if (id === 'editor.undo') return formatChord('MOD+Z', isMac);
  if (id === 'editor.redo') return formatChord(isMac ? 'MOD+SHIFT+Z' : 'MOD+Y', isMac);
  if (id === 'editor.find') return formatChord('MOD+F', isMac);
  if (id === 'editor.replace') return formatChord(isMac ? 'ALT+MOD+F' : 'MOD+H', isMac);
  const chord = keybindings.byAction.get(id);
  return chord ? formatChord(chord, isMac) : null;
}

let importedTheme: ImportedTheme | null = loadImportedTheme(localStorage);
let importedChromeVariables: string[] = [];

function clearImportedChrome(): void {
  for (const variable of importedChromeVariables) {
    document.documentElement.style.removeProperty(variable);
  }
  importedChromeVariables = [];
}

function refreshImportedThemeOption(): void {
  const picker = document.getElementById('settingsTheme') as HTMLSelectElement | null;
  if (!picker) return;
  picker.querySelector('option[value="imported"]')?.remove();
  if (!importedTheme) return;
  const option = document.createElement('option');
  option.value = 'imported';
  option.textContent = `Imported: ${importedTheme.name}`;
  picker.append(option);
}

function applyImportedTheme(theme: ImportedTheme): void {
  clearImportedChrome();
  monaco.editor.defineTheme('ryoko-imported', theme.monacoTheme);
  monaco.editor.setTheme('ryoko-imported');
  for (const [variable, value] of Object.entries(theme.chrome)) {
    document.documentElement.style.setProperty(variable, value);
    importedChromeVariables.push(variable);
  }
  document.documentElement.style.colorScheme = theme.monacoTheme.base === 'vs' ? 'light' : 'dark';
  importedTheme = theme;
  refreshImportedThemeOption();
  setStatus(`Imported VS Code theme: ${theme.name}`);
}

function resetImportedTheme(): void {
  clearImportedChrome();
  importedTheme = null;
  try {
    localStorage.removeItem(IMPORTED_THEME_STORAGE);
  } catch {
    // Storage is optional.
  }
  applyThemeToDocument(currentTheme);
  monaco.editor.setTheme(currentTheme.monaco);
  refreshImportedThemeOption();
  const picker = document.getElementById('settingsTheme') as HTMLSelectElement | null;
  if (picker) picker.value = currentTheme.id;
  setStatus('Imported theme removed. Built-in theme restored.');
}

function setTheme(theme: ThemeDefinition): void {
  currentTheme = theme;
  applyThemeToDocument(theme);
  if (importedTheme) {
    clearImportedChrome();
    importedTheme = null;
    try {
      localStorage.removeItem(IMPORTED_THEME_STORAGE);
    } catch {
      // Storage is optional.
    }
  }
  monaco.editor.setTheme(theme.monaco);
  saveTheme(localStorage, theme.id);
  refreshImportedThemeOption();
  const picker = document.getElementById('settingsTheme') as HTMLSelectElement | null;
  if (picker) picker.value = theme.id;
  setStatus(`Color theme: ${theme.label}`);
}

function persistLayout(): void {
  saveLayout(localStorage, layoutState);
}

function applyLayout(): void {
  workbenchLayout.style.setProperty('--sidebar-w', `${layoutState.sidebarWidth}px`);
  workbenchLayout.style.setProperty('--inspector-w', `${layoutState.inspectorWidth}px`);
  bottomPanel.style.setProperty('--panel-h', `${layoutState.panelHeight}px`);
  requestAnimationFrame(() => { editor.layout(); fitTerminal(); });
}

function sidebarVisible(): boolean {
  return !workbenchLayout.classList.contains('hide-explorer');
}

function toggleSidebar(): void {
  if (sidebarVisible()) {
    workbenchLayout.classList.add('hide-explorer');
    layoutState.sidebarCollapsed = true;
  } else {
    activateView('explorer');
    layoutState.sidebarCollapsed = false;
  }
  persistLayout();
  requestAnimationFrame(() => editor.layout());
}

function togglePanel(): void {
  if (bottomPanel.classList.contains('hidden')) showBottom('terminal');
  else bottomPanel.classList.add('hidden');
  requestAnimationFrame(() => editor.layout());
}

function toggleMinimap(): void {
  layoutState.minimap = !layoutState.minimap;
  editor.updateOptions({ minimap: { enabled: layoutState.minimap, renderCharacters: true } });
  persistLayout();
  setStatus(layoutState.minimap ? 'Minimap shown' : 'Minimap hidden');
}

function resetLayout(): void {
  Object.assign(layoutState, DEFAULT_LAYOUT);
  editor.updateOptions({ minimap: { enabled: layoutState.minimap, renderCharacters: true } });
  applyLayout();
  persistLayout();
  activateView('explorer');
  setStatus('Layout reset');
}

/** Wires a drag handle. `sign` is +1 when moving right/down grows the pane. */
function wireSash(
  sash: HTMLElement,
  key: 'sidebarWidth' | 'inspectorWidth' | 'panelHeight',
  axis: 'x' | 'y',
  sign: 1 | -1,
  onCollapse?: () => void,
): void {
  sash.addEventListener('pointerdown', (event) => {
    if (event.button !== 0) return;
    event.preventDefault();
    sash.setPointerCapture(event.pointerId);
    const origin = axis === 'x' ? event.clientX : event.clientY;
    const start = layoutState[key];
    document.body.classList.add(axis === 'x' ? 'resizing-x' : 'resizing-y');
    sash.classList.add('active');
    const move = (moveEvent: PointerEvent): void => {
      const delta = ((axis === 'x' ? moveEvent.clientX : moveEvent.clientY) - origin) * sign;
      const next = resizeBy(key, start, delta, !!onCollapse);
      if (next.collapse && onCollapse) {
        layoutState[key] = start;
        finish();
        onCollapse();
        return;
      }
      layoutState[key] = next.size;
      applyLayout();
    };
    const finish = (): void => {
      sash.removeEventListener('pointermove', move);
      sash.removeEventListener('pointerup', finish);
      sash.removeEventListener('pointercancel', finish);
      document.body.classList.remove('resizing-x', 'resizing-y');
      sash.classList.remove('active');
      sash.blur();
      persistLayout();
    };
    sash.addEventListener('pointermove', move);
    sash.addEventListener('pointerup', finish);
    sash.addEventListener('pointercancel', finish);
  });
  sash.addEventListener('dblclick', () => {
    layoutState[key] = DEFAULT_LAYOUT[key];
    applyLayout();
    persistLayout();
  });
  sash.addEventListener('keydown', (event) => {
    const grow = axis === 'x' ? ['ArrowRight', 'ArrowLeft'] : ['ArrowDown', 'ArrowUp'];
    const index = grow.indexOf(event.key);
    if (index < 0) return;
    event.preventDefault();
    const step = (index === 0 ? 1 : -1) * sign * (event.shiftKey ? 50 : 10);
    layoutState[key] = resizeBy(key, layoutState[key], step).size;
    applyLayout();
    persistLayout();
  });
}

wireSash(element('sidebarSash'), 'sidebarWidth', 'x', 1, () => {
  workbenchLayout.classList.add('hide-explorer');
  layoutState.sidebarCollapsed = true;
  persistLayout();
  requestAnimationFrame(() => editor.layout());
});
wireSash(element('inspectorSash'), 'inspectorWidth', 'x', -1);
wireSash(element('panelSash'), 'panelHeight', 'y', -1);
applyLayout();
if (layoutState.sidebarCollapsed) workbenchLayout.classList.add('hide-explorer');

// Breadcrumbs ---------------------------------------------------------------

const breadcrumbsHost = element('breadcrumbs');
let outline: { uri: string; version: number; symbols: OutlineSymbol[] } | null = null;
let outlineTicket = 0;
let outlineTimer: number | undefined;

/**
 * Symbols come from the language's server (`textDocument/documentSymbol`).
 * Without a running server that supports it, breadcrumbs show the path only.
 */
async function loadOutline(file: OpenFile): Promise<OutlineSymbol[]> {
  const response = await lsp.requestForLanguage(file.model.getLanguageId(), 'textDocument/documentSymbol', {
    textDocument: { uri: file.uri },
  });
  return response && !response.error ? fromLspDocumentSymbols(response.result) : [];
}

function scheduleOutline(): void {
  window.clearTimeout(outlineTimer);
  outlineTimer = window.setTimeout(() => void refreshOutline(), 350);
}

async function refreshOutline(): Promise<void> {
  const file = state.current;
  if (!file) { outline = null; renderBreadcrumbs(); return; }
  const version = file.model.getVersionId();
  if (outline && outline.uri === file.uri && outline.version === version) { renderBreadcrumbs(); return; }
  const ticket = ++outlineTicket;
  try {
    const symbols = await loadOutline(file);
    if (ticket !== outlineTicket || state.current !== file) return;
    outline = { uri: file.uri, version, symbols };
  } catch {
    if (ticket === outlineTicket) outline = { uri: file.uri, version, symbols: [] };
  }
  renderBreadcrumbs();
}

function renderBreadcrumbs(): void {
  const file = state.current;
  breadcrumbsHost.replaceChildren();
  if (!file) return;
  const position = editor.getPosition();
  const chain = outline && outline.uri === file.uri && position
    ? symbolChain(outline.symbols, position.lineNumber, position.column)
    : [];
  const items = breadcrumbItems(file.path, chain);
  const list = document.createElement('ol');
  items.forEach((item, index) => {
    const entry = document.createElement('li');
    entry.className = `crumb crumb-${item.kind}`;
    if (item.kind === 'file') entry.append(fileIconElement(file.path));
    if (item.kind === 'symbol') {
      const button = document.createElement('button');
      button.type = 'button';
      button.textContent = item.label;
      button.title = `Go to ${item.label}`;
      button.addEventListener('click', () => {
        if (item.line === undefined) return;
        const target = { lineNumber: item.line, column: item.column ?? 1 };
        editor.setPosition(target);
        editor.revealPositionInCenterIfOutsideViewport(target);
        editor.focus();
      });
      entry.append(button);
    } else {
      const label = document.createElement('span');
      label.textContent = item.label;
      entry.append(label);
    }
    if (index === items.length - 1) entry.setAttribute('aria-current', 'location');
    list.append(entry);
  });
  breadcrumbsHost.append(list);
}

editor.onDidChangeModel(() => { outline = null; renderBreadcrumbs(); scheduleOutline(); });
editor.onDidChangeModelContent(scheduleOutline);
editor.onDidChangeCursorPosition(renderBreadcrumbs);

// Commands ------------------------------------------------------------------

function showWelcome(): void {
  editorWorkspace.classList.add('hidden');
  emptyEditor.classList.remove('hidden');
}

const definitions: WorkbenchCommand[] = [
  { id: 'workbench.commandPalette', category: 'View', title: 'Command Palette…', run: () => palette.open() },
  { id: 'workbench.quickOpen', category: 'File', title: 'Go to File…', run: () => quickOpen.open() },
  { id: 'workspace.openFolder', category: 'File', title: 'Open Folder…', run: () => openFolderButton.click() },
  { id: 'workspace.newFile', category: 'File', title: 'New File…', run: () => { void createWorkspaceFile(); } },
  { id: 'workspace.newFolder', category: 'File', title: 'New Folder…', run: () => { void createWorkspaceFolder(); } },
  { id: 'workspace.openFile', category: 'File', title: 'Open File…', run: () => openWorkspaceFile() },
  { id: 'workspace.openFileByPath', category: 'File', title: 'Open File by Path…', run: () => openWorkspaceFile(true) },
  { id: 'workspace.openFolderByPath', category: 'File', title: 'Open Folder by Path…', run: () => { void openFolderByPath(); } },
  { id: 'editor.save', category: 'File', title: 'Save', run: () => saveButton.click(), enabled: () => !saveButton.disabled },
  { id: 'editor.close', category: 'File', title: 'Close Editor', run: () => { if (state.current) closeOpenFile(state.current.path); }, enabled: () => !!state.current },
  { id: 'workbench.settings', category: 'Preferences', title: 'Open Settings', run: () => openSettings.click() },
  { id: 'editor.undo', category: 'Edit', title: 'Undo', run: () => editor.trigger('menu', 'undo', null), enabled: () => !!state.current },
  { id: 'editor.redo', category: 'Edit', title: 'Redo', run: () => editor.trigger('menu', 'redo', null), enabled: () => !!state.current },
  { id: 'editor.find', category: 'Edit', title: 'Find', run: () => { editor.focus(); void editor.getAction('actions.find')?.run(); }, enabled: () => !!state.current },
  { id: 'editor.replace', category: 'Edit', title: 'Replace', run: () => { editor.focus(); void editor.getAction('editor.action.startFindReplaceAction')?.run(); }, enabled: () => !!state.current },
  { id: 'workbench.search', category: 'Search', title: 'Find in Files', run: () => dispatchBinding('workbench.search') },
  { id: 'workbench.replace', category: 'Search', title: 'Replace in Files', run: () => dispatchBinding('workbench.replace') },
  { id: 'editor.inlineEdit', category: 'Agent', title: 'Inline Edit with Agent', run: () => inlineEdit.open(), enabled: () => !!state.current },
  { id: 'view.explorer', category: 'View', title: 'Explorer', run: () => activateView('explorer') },
  { id: 'workbench.changes', category: 'View', title: 'Source Control Changes', run: () => activateView('changes') },
  { id: 'view.mission', category: 'View', title: 'Mission Control', run: () => activateView('mission') },
  { id: 'view.problems', category: 'View', title: 'Problems', run: () => activateView('problems') },
  { id: 'view.extensions', category: 'View', title: 'Extensions', run: () => element('openMarketplace').click() },
  { id: 'view.toggleSidebar', category: 'View', title: 'Toggle Primary Side Bar', run: toggleSidebar },
  { id: 'view.toggleInspector', category: 'View', title: 'Toggle Mission Control Side Bar', run: () => activateView(workbenchLayout.classList.contains('hide-inspector') ? 'mission' : 'editor') },
  { id: 'view.togglePanel', category: 'View', title: 'Toggle Panel', run: togglePanel },
  { id: 'editor.splitRight', category: 'View', title: 'Split Editor Right', run: toggleSplitPane, enabled: () => openFiles.size > 0 },
  { id: 'view.toggleMinimap', category: 'View', title: 'Toggle Minimap', run: toggleMinimap },
  { id: 'view.resetLayout', category: 'View', title: 'Reset Layout', run: resetLayout },
  { id: 'theme.pick', category: 'Preferences', title: 'Color Theme…', run: () => palette.open('Theme: ') },
  ...THEMES.map((theme): WorkbenchCommand => ({
    id: `theme.${theme.id.replace(/-(\w)/g, (_, char: string) => char.toUpperCase())}`,
    category: 'Theme',
    title: theme.label,
    run: () => setTheme(theme),
  })),
  { id: 'run.activeFile', category: 'Run', title: 'Run Active File', run: () => runActiveFile.click(), enabled: () => !runActiveFile.disabled },
  { id: 'workbench.debug', category: 'Run', title: 'Toggle Debug Panel', run: () => showBottom('debug') },
  { id: 'terminal.show', category: 'Terminal', title: 'Show Terminal', run: () => showBottom('terminal') },
  { id: 'help.welcome', category: 'Help', title: 'Welcome', run: showWelcome },
  { id: 'help.reportBug', category: 'Help', title: 'Report a Bug…', run: () => bugReporter.open() },
  {
    id: 'help.keyboardShortcuts',
    category: 'Help',
    title: 'Keyboard Shortcuts',
    run: () => {
      openSettings.click();
      document.getElementById('settingsKeybindings')?.scrollIntoView({ block: 'start' });
    },
  },
];
const updates = mountUpdates({
  setStatus,
  unsavedFiles: () => [...openFiles.values()].filter((file) => file.dirty).map((file) => file.path),
  statusHost: document.querySelector<HTMLElement>('.statusbar')!,
  settingsHost: document.getElementById('settingsUpdates'),
});
definitions.push({ id: 'help.checkForUpdates', category: 'Help', title: 'Check for Updates…', run: () => updates.checkNow() });
for (const command of definitions) commands.register(command);

const palette = mountCommandPalette({ registry: commands, shortcutFor, setStatus });
mountMenuBar(element('menuBar'), { registry: commands, shortcutFor, setStatus });

// Settings › Color theme.
const themePicker = element<HTMLSelectElement>('settingsTheme');
for (const theme of THEMES) {
  const option = document.createElement('option');
  option.value = theme.id;
  option.textContent = theme.label;
  themePicker.append(option);
}
themePicker.value = currentTheme.id;
refreshImportedThemeOption();
if (importedTheme) {
  applyImportedTheme(importedTheme);
  themePicker.value = 'imported';
}
themePicker.addEventListener('change', () => {
  if (themePicker.value === 'imported' && importedTheme) applyImportedTheme(importedTheme);
  else setTheme(themeById(themePicker.value));
});

// Settings › Editor mode.
const keymapPicker = element<HTMLSelectElement>('settingsKeymap');
for (const option of KEYMAP_OPTIONS) {
  const entry = document.createElement('option');
  entry.value = option.id;
  entry.textContent = option.label;
  entry.title = option.description;
  keymapPicker.append(entry);
}
keymapPicker.value = keymapPreset;
keymapPicker.addEventListener('change', () => void applyKeymap(keymapPicker.value as KeymapPreset));

// Settings › Crash recovery (opt-in).
const recoveryToggle = element<HTMLInputElement>('settingsRecoveryToggle');
recoveryToggle.checked = recoveryEnabled;
recoveryToggle.addEventListener('change', () => {
  recoveryEnabled = recoveryToggle.checked;
  saveRecoveryEnabled(localStorage, recoveryEnabled);
  if (!recoveryEnabled) {
    for (const timer of recoveryTimers.values()) window.clearTimeout(timer);
    recoveryTimers.clear();
    void invoke('recovery_retire_session', { sessionId: recoverySessionId }).catch(() => undefined);
    setStatus('Crash recovery snapshots disabled; active session snapshots cleared.');
  } else {
    setStatus('Crash recovery snapshots enabled (local, non-sensitive buffers only).');
  }
});

// Settings › Import from VS Code.
const vscodeImportFile = element<HTMLInputElement>('vscodeImportFile');
const vscodeImportThemeButton = element<HTMLButtonElement>('vscodeImportTheme');
const vscodeImportSettingsButton = element<HTMLButtonElement>('vscodeImportSettings');
const vscodeResetThemeButton = element<HTMLButtonElement>('vscodeResetTheme');
const vscodeImportNote = element('vscodeImportNote');

async function readImportFile(): Promise<string> {
  const file = vscodeImportFile.files?.[0];
  if (!file) throw new Error('choose a .json file first');
  if (file.size > 512 * 1024) throw new Error('file exceeds the 512 KiB import limit');
  return file.text();
}

vscodeImportThemeButton.addEventListener('click', () => {
  void (async () => {
    try {
      const theme = parseVscodeTheme(await readImportFile());
      saveImportedTheme(localStorage, theme);
      applyImportedTheme(theme);
      themePicker.value = 'imported';
      vscodeImportNote.textContent = `Imported “${theme.name}”.`
        + (theme.warnings.length ? ' ' + theme.warnings.slice(0, 3).join(' ') : '');
    } catch (error) {
      vscodeImportNote.textContent = 'Theme import failed: '
        + (error instanceof Error ? error.message : String(error));
    }
  })();
});

vscodeImportSettingsButton.addEventListener('click', () => {
  void (async () => {
    try {
      const parsed = parseVscodeSettings(await readImportFile());
      saveImportedSettings(localStorage, parsed.settings);
      editor.updateOptions(monacoOptionsFor(parsed.settings));
      diffEditor.updateOptions(monacoOptionsFor(parsed.settings));
      const applied = Object.keys(parsed.settings).filter((key) => parsed.settings[key as keyof typeof parsed.settings] !== undefined);
      vscodeImportNote.textContent = applied.length
        ? `Applied: ${applied.join(', ')}.` + (parsed.warnings.length ? ' ' + parsed.warnings.slice(0, 3).join(' ') : '')
        : 'No supported editor settings were found.';
    } catch (error) {
      vscodeImportNote.textContent = 'Settings import failed: '
        + (error instanceof Error ? error.message : String(error));
    }
  })();
});

vscodeResetThemeButton.addEventListener('click', () => {
  resetImportedTheme();
  vscodeImportNote.textContent = 'Imported theme removed.';
});

// macOS shows the native application menu (built in Rust with the same
// command ids) instead of the in-window menu bar. Menu events carry only a
// command id; unknown ids are ignored by the registry.
if (isMac && '__TAURI_INTERNALS__' in window) {
  document.body.classList.add('native-menu');
  void listen<string>('ryoko://menu', (event) => {
    void commands.execute(event.payload);
  });
}

