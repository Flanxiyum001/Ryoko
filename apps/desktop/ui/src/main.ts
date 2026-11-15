import { invoke } from '@tauri-apps/api/core';
import { open } from '@tauri-apps/plugin-dialog';
import * as monaco from 'monaco-editor';
import EditorWorker from 'monaco-editor/esm/vs/editor/editor.worker?worker';
import './styles.css';
import { LspClient, type LspState } from './lsp';
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
const lspStatus = element('lspStatus');
const editorStatus = element('editorStatus');
const cursorPos = element('cursorPos');
const languageMode = element('languageMode');

const state: {
  root: string;
  files: string[];
  current: OpenFile | null;
} = { root: '', files: [], current: null };

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
    await lsp.start(info.root);
  } catch (error) {
    setStatus(error instanceof Error ? error.message : String(error));
  }
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

window.addEventListener('beforeunload', () => {
  void lsp.stop();
});

setLspState('offline');
