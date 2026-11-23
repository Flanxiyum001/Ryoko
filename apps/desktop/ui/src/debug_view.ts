//! Debug panel view and controller.
//! Wires the DAP toolbar, call stack, variables inspector, breakpoints list and log
//! with keyboard navigation and safe, size-bounded event handling.

import { invoke } from '@tauri-apps/api/core';
import { listen, type UnlistenFn } from '@tauri-apps/api/event';
import {
  appendDebugLog,
  createInitialDebugState,
  formatBreakpoint,
  formatSourceLocation,
  handleScopesResponse,
  handleSessionStarted,
  handleSessionStopped,
  handleStackTraceResponse,
  handleStoppedEvent,
  handleVariablesResponse,
  handleWorkspaceReset,
  parseDapMessage,
  selectStackFrame,
  toggleBreakpointInState,
  toggleVariableExpanded,
  updateBreakpointsFromResponse,
  type DapBreakpoint,
  type DapStackFrame,
  type DapVariable,
  type DebugState,
} from './debug_inspector';

export interface DebugViewDeps {
  openFileAtLine: (path: string, line: number, column?: number) => Promise<void>;
  getCurrentFile: () => { path: string } | null;
  getEditorLine: () => number | null;
  getWorkspaceRoot: () => string;
  setStatus: (message: string) => void;
  showBottom: (panel: 'terminal' | 'debug' | 'search') => void;
  joinWorkspacePath: (root: string, relative: string) => string;
}

export interface DapStatus {
  running: boolean;
  sessionId: number | null;
}

interface DapWireEvent {
  sessionId: number;
  message: string;
}

interface DapLogEvent {
  sessionId: number;
  message: string;
}

export class DebugViewController {
  private state: DebugState = createInitialDebugState();
  private deps: DebugViewDeps;
  private dapSeq = 0;
  private dapStatusPolling = false;
  private unlistenMessage: UnlistenFn | null = null;
  private unlistenLog: UnlistenFn | null = null;

  // DOM elements
  private adapterInput!: HTMLInputElement;
  private argsInput!: HTMLInputElement;
  private programInput!: HTMLInputElement;
  private startBtn!: HTMLButtonElement;
  private stopBtn!: HTMLButtonElement;
  private restartBtn!: HTMLButtonElement;
  private continueBtn!: HTMLButtonElement;
  private nextBtn!: HTMLButtonElement;
  private stepInBtn!: HTMLButtonElement;
  private stepOutBtn!: HTMLButtonElement;
  private breakpointBtn!: HTMLButtonElement;
  private statusText!: HTMLElement;
  private stackCount!: HTMLElement;
  private stackList!: HTMLElement;
  private variablesCount!: HTMLElement;
  private variablesList!: HTMLElement;
  private breakpointsCount!: HTMLElement;
  private breakpointsList!: HTMLElement;
  private debugLog!: HTMLElement;
  private clearLogBtn!: HTMLButtonElement;

  constructor(deps: DebugViewDeps) {
    this.deps = deps;
  }

  public mount(): void {
    this.bindElements();
    this.attachEventListeners();
    this.render();
    void this.setupIpcListeners();
  }

  public unmount(): void {
    if (this.unlistenMessage) {
      this.unlistenMessage();
      this.unlistenMessage = null;
    }
    if (this.unlistenLog) {
      this.unlistenLog();
      this.unlistenLog = null;
    }
  }

  public getState(): Readonly<DebugState> {
    return this.state;
  }

  private bindElements(): void {
    const byId = <T extends HTMLElement>(id: string): T => {
      const el = document.getElementById(id);
      if (!el) throw new Error(`Missing debug panel element #${id}`);
      return el as T;
    };

    this.adapterInput = byId<HTMLInputElement>('debugAdapter');
    this.argsInput = byId<HTMLInputElement>('debugArgs');
    this.programInput = byId<HTMLInputElement>('debugProgram');
    this.startBtn = byId<HTMLButtonElement>('debugStart');
    this.stopBtn = byId<HTMLButtonElement>('debugStop');
    this.restartBtn = byId<HTMLButtonElement>('debugRestart');
    this.continueBtn = byId<HTMLButtonElement>('debugContinue');
    this.nextBtn = byId<HTMLButtonElement>('debugNext');
    this.stepInBtn = byId<HTMLButtonElement>('debugStepIn');
    this.stepOutBtn = byId<HTMLButtonElement>('debugStepOut');
    this.breakpointBtn = byId<HTMLButtonElement>('debugBreakpoint');
    this.statusText = byId<HTMLElement>('debugStatus');

    this.stackCount = byId<HTMLElement>('debugStackCount');
    this.stackList = byId<HTMLElement>('debugStackList');
    this.variablesCount = byId<HTMLElement>('debugVariablesCount');
    this.variablesList = byId<HTMLElement>('debugVariablesList');
    this.breakpointsCount = byId<HTMLElement>('debugBreakpointsCount');
    this.breakpointsList = byId<HTMLElement>('debugBreakpointsList');
    this.debugLog = byId<HTMLElement>('debugLog');
    this.clearLogBtn = byId<HTMLButtonElement>('debugClearLog');
  }

  private attachEventListeners(): void {
    this.startBtn.addEventListener('click', () => {
      void this.startDebug();
    });

    this.stopBtn.addEventListener('click', () => {
      void this.stopDebug();
    });

    this.restartBtn.addEventListener('click', () => {
      void this.restartDebug();
    });

    this.continueBtn.addEventListener('click', () => {
      void this.dapStep('continue');
    });

    this.nextBtn.addEventListener('click', () => {
      void this.dapStep('next');
    });

    this.stepInBtn.addEventListener('click', () => {
      void this.dapStep('stepIn');
    });

    this.stepOutBtn.addEventListener('click', () => {
      void this.dapStep('stepOut');
    });

    this.breakpointBtn.addEventListener('click', () => {
      void this.toggleCurrentBreakpoint();
    });

    this.clearLogBtn.addEventListener('click', () => {
      this.state.logLines = [];
      this.debugLog.textContent = '';
    });

    // Enter in adapter / program input triggers start if idle
    this.programInput.addEventListener('keydown', (e) => {
      if (e.key === 'Enter' && !this.state.running) {
        void this.startDebug();
      }
    });

    // Keyboard navigation within the lists
    this.stackList.addEventListener('keydown', (e) => this.handleStackKeydown(e));
    this.variablesList.addEventListener('keydown', (e) => this.handleVariablesKeydown(e));
    this.breakpointsList.addEventListener('keydown', (e) => this.handleBreakpointsKeydown(e));
  }

  private async setupIpcListeners(): Promise<void> {
    try {
      this.unlistenMessage = await listen<DapWireEvent>('ryoko-dap-message', (event) => {
        this.handleWireMessage(event.payload);
      });
      this.unlistenLog = await listen<DapLogEvent>('ryoko-dap-log', (event) => {
        if (this.state.sessionId !== null && event.payload.sessionId === this.state.sessionId) {
          this.log(`[adapter] ${event.payload.message}`);
        }
      });
    } catch {
      // Running outside Tauri (e.g. unit tests) gracefully handles missing events
    }
  }

  public async startDebug(): Promise<void> {
    const adapter = this.adapterInput.value.trim();
    if (!adapter) {
      this.statusText.textContent = 'Enter a DAP adapter executable first.';
      return;
    }
    try {
      this.deps.showBottom('debug');
      const args = this.argsInput.value.trim().split(/\s+/).filter(Boolean);
      const status = await invoke<DapStatus>('dap_start', { adapter, args });
      const sessionId = status.sessionId ?? 1;
      this.state = handleSessionStarted(this.state, sessionId);
      this.dapSeq = 0;
      this.render();

      await this.sendDap('initialize', {
        adapterID: 'ryoko',
        clientID: 'ryoko-desktop',
        linesStartAt1: true,
        columnsStartAt1: true,
        pathFormat: 'path',
        supportsRunInTerminalRequest: false,
      });
    } catch (error) {
      await invoke('dap_stop').catch(() => undefined);
      this.state = handleSessionStopped(
        this.state,
        `Debug adapter unavailable: ${error instanceof Error ? error.message : String(error)}`,
      );
      this.render();
    }
  }

  public async stopDebug(): Promise<void> {
    await invoke('dap_stop').catch(() => undefined);
    this.state = handleSessionStopped(this.state, 'Debug adapter stopped');
    this.render();
  }

  public async restartDebug(): Promise<void> {
    await this.stopDebug();
    await this.startDebug();
  }

  /** Called when switching workspaces to clean up all active sessions and state. */
  public async resetOnWorkspaceSwitch(): Promise<void> {
    await invoke('dap_stop').catch(() => undefined);
    this.state = handleWorkspaceReset();
    this.dapSeq = 0;
    this.render();
  }

  private async launchDap(): Promise<void> {
    const program = this.programInput.value.trim();
    if (!program) {
      this.state.statusText = 'Adapter initialized. Set a program path and restart to launch it.';
      this.render();
      return;
    }
    const root = this.deps.getWorkspaceRoot();
    const isAbsolute = program.startsWith('/') || /^[A-Za-z]:[\\/]/.test(program);
    const resolvedProgram = isAbsolute ? program : this.deps.joinWorkspacePath(root, program);

    try {
      // Sync configured breakpoints before launching
      await this.syncAllBreakpoints();

      await this.sendDap('launch', {
        program: resolvedProgram,
        cwd: root,
        args: [],
        stopOnEntry: false,
      });
      await this.sendDap('configurationDone');
      this.state.statusText = 'Launch requested';
      this.render();
    } catch (error) {
      this.state.statusText = `Launch failed: ${error instanceof Error ? error.message : String(error)}`;
      this.render();
    }
  }

  public async dapStep(command: string): Promise<void> {
    if (!this.state.running) return;
    try {
      this.state.isPaused = false;
      this.render();
      await this.sendDap(command, { threadId: this.state.threadId });
    } catch (error) {
      this.state.statusText = String(error);
      this.render();
    }
  }

  public async toggleCurrentBreakpoint(): Promise<void> {
    const file = this.deps.getCurrentFile();
    const line = this.deps.getEditorLine();
    if (!file || !line) return;

    const root = this.deps.getWorkspaceRoot();
    const absolute = file.path.startsWith('/') || /^[A-Za-z]:[\\/]/.test(file.path)
      ? file.path
      : this.deps.joinWorkspacePath(root, file.path);

    const result = toggleBreakpointInState(this.state, absolute, line);
    this.state = result.nextState;
    this.renderBreakpoints();

    if (this.state.running) {
      try {
        await this.sendDap('setBreakpoints', {
          source: { path: absolute, name: file.path },
          breakpoints: result.lines.map((l) => ({ line: l })),
        });
      } catch (error) {
        this.log(`[error] setBreakpoints failed: ${String(error)}`);
      }
    }
  }

  private async syncAllBreakpoints(): Promise<void> {
    for (const [path, bps] of this.state.breakpoints) {
      const filename = path.split(/[\\/]/).pop() || path;
      await this.sendDap('setBreakpoints', {
        source: { path, name: filename },
        breakpoints: bps.map((b) => ({ line: b.line })),
      });
    }
  }

  private async sendDap(command: string, args?: Record<string, unknown>): Promise<void> {
    this.dapSeq += 1;
    const message = JSON.stringify({
      seq: this.dapSeq,
      type: 'request',
      command,
      ...(args ? { arguments: args } : {}),
    });
    this.log(`→ ${message}`);
    await invoke('dap_send', { message });
  }

  private handleWireMessage(payload: DapWireEvent): void {
    if (this.state.sessionId === null || payload.sessionId !== this.state.sessionId) return;
    this.log(`← ${payload.message}`);

    const msg = parseDapMessage(payload.message);
    if (!msg) return;

    const type = msg.type;
    if (type === 'event') {
      this.handleDapEvent(String(msg.event ?? ''), msg.body);
    } else if (type === 'response') {
      this.handleDapResponse(msg);
    }
  }

  private handleDapEvent(event: string, body: unknown): void {
    if (event === 'initialized') {
      this.state.statusText = 'Adapter initialized';
      this.render();
      void this.launchDap();
    } else if (event === 'stopped') {
      const stoppedResult = handleStoppedEvent(this.state, body);
      this.state = stoppedResult.nextState;
      this.render();
      // Fetch stack trace automatically when paused
      void this.sendDap('stackTrace', {
        threadId: stoppedResult.threadId,
        startFrame: 0,
        levels: 20,
      });
    } else if (event === 'continued') {
      this.state.statusText = 'Running';
      this.state.isPaused = false;
      this.state.frames = [];
      this.state.scopes = [];
      this.state.variables.clear();
      this.state.expandedVarRefs.clear();
      this.render();
    } else if (event === 'terminated' || event === 'exited') {
      this.state = handleSessionStopped(this.state, 'Session ended');
      this.render();
    } else if (event === 'breakpoint') {
      if (body && typeof body === 'object') {
        const bp = (body as { breakpoint?: unknown }).breakpoint;
        if (bp && typeof bp === 'object') {
          const rawSource = (bp as { source?: unknown }).source;
          const path = (rawSource as { path?: unknown })?.path;
          if (typeof path === 'string') {
            this.state = updateBreakpointsFromResponse(this.state, path, { breakpoints: [bp] });
            this.renderBreakpoints();
          }
        }
      }
    }
  }

  private handleDapResponse(msg: Record<string, unknown>): void {
    const command = String(msg.command ?? '');
    const success = Boolean(msg.success);

    if (!success) {
      const errorMsg = String(msg.message ?? 'failed');
      this.state.statusText = `DAP ${command} failed: ${errorMsg}`;
      this.render();
      return;
    }

    if (command === 'stackTrace') {
      const res = handleStackTraceResponse(this.state, msg.body);
      this.state = res.nextState;
      this.renderStack();
      if (res.topFrame) {
        // Navigate editor to the top paused frame
        this.navigateToFrame(res.topFrame);
        // Request scopes for the top frame
        void this.sendDap('scopes', { frameId: res.topFrame.id });
      }
    } else if (command === 'scopes') {
      const res = handleScopesResponse(this.state, msg.body);
      this.state = res.nextState;
      if (res.primaryScopeRef !== null) {
        void this.sendDap('variables', { variablesReference: res.primaryScopeRef });
      }
    } else if (command === 'variables') {
      // In DAP, request_seq matches the request seq
      // For variables, we can store in state and re-render
      const rawArgs = msg.arguments as { variablesReference?: unknown } | undefined;
      // Some adapters echo arguments in response or we read variables from body
      // We also track pending request or fetch for known references
      const scopes = this.state.scopes;
      const ref = typeof rawArgs?.variablesReference === 'number'
        ? rawArgs.variablesReference
        : (scopes[0]?.variablesReference ?? 0);

      this.state = handleVariablesResponse(this.state, ref, msg.body);
      this.renderVariables();
    } else if (command === 'setBreakpoints') {
      // Find matching path from state
      for (const path of this.state.breakpoints.keys()) {
        this.state = updateBreakpointsFromResponse(this.state, path, msg.body);
      }
      this.renderBreakpoints();
    }
  }

  private navigateToFrame(frame: DapStackFrame): void {
    const path = frame.source?.path || frame.source?.name;
    if (!path || frame.line <= 0) return;
    void this.deps.openFileAtLine(path, frame.line, frame.column);
  }

  private async selectFrame(frameId: number): Promise<void> {
    const res = selectStackFrame(this.state, frameId);
    this.state = res.nextState;
    this.renderStack();
    this.renderVariables();
    if (res.selectedFrame) {
      this.navigateToFrame(res.selectedFrame);
      await this.sendDap('scopes', { frameId });
    }
  }

  private async toggleVariable(variable: DapVariable): Promise<void> {
    if (variable.variablesReference <= 0) return;
    const res = toggleVariableExpanded(this.state, variable.variablesReference);
    this.state = res.nextState;
    this.renderVariables();
    if (res.shouldFetch) {
      await this.sendDap('variables', { variablesReference: variable.variablesReference });
    }
  }

  private log(message: string): void {
    this.state = appendDebugLog(this.state, message);
    this.debugLog.textContent = this.state.logLines.join('\n');
    this.debugLog.scrollTop = this.debugLog.scrollHeight;
  }

  public pollStatus(): void {
    if (this.state.sessionId === null || this.dapStatusPolling) return;
    this.dapStatusPolling = true;
    const observedId = this.state.sessionId;

    void invoke<DapStatus>('dap_status')
      .then((status) => {
        if (!status.running && this.state.sessionId === observedId) {
          this.state = handleSessionStopped(this.state, 'Debug adapter exited');
          this.render();
        }
      })
      .catch((error) => {
        this.state.statusText = `Debug status unavailable: ${String(error)}`;
        this.render();
      })
      .finally(() => {
        this.dapStatusPolling = false;
      });
  }

  // --- Rendering routines ---

  public render(): void {
    const running = this.state.running;
    const isPaused = this.state.isPaused;

    this.startBtn.disabled = running;
    this.stopBtn.disabled = !running;
    this.restartBtn.disabled = !running;
    this.continueBtn.disabled = !running || !isPaused;
    this.nextBtn.disabled = !running || !isPaused;
    this.stepInBtn.disabled = !running || !isPaused;
    this.stepOutBtn.disabled = !running || !isPaused;
    this.breakpointBtn.disabled = !running;

    this.statusText.textContent = this.state.statusText;

    this.renderStack();
    this.renderVariables();
    this.renderBreakpoints();
  }

  private renderStack(): void {
    const frames = this.state.frames;
    this.stackCount.textContent = `${frames.length} frame${frames.length === 1 ? '' : 's'}`;
    this.stackList.replaceChildren();

    if (frames.length === 0) {
      const hint = document.createElement('p');
      hint.className = 'hint';
      hint.textContent = this.state.isPaused ? 'No stack frames.' : 'Not paused.';
      this.stackList.append(hint);
      return;
    }

    for (const frame of frames) {
      const row = document.createElement('div');
      row.className = 'debug-item debug-frame-item';
      if (frame.id === this.state.selectedFrameId) {
        row.classList.add('active');
      }
      row.tabIndex = 0;
      row.setAttribute('role', 'button');
      row.setAttribute('aria-label', `Frame ${frame.name} at ${formatSourceLocation(frame)}`);

      const nameSpan = document.createElement('span');
      nameSpan.className = 'debug-frame-name';
      nameSpan.textContent = frame.name;

      const locSpan = document.createElement('span');
      locSpan.className = 'debug-frame-loc';
      locSpan.textContent = formatSourceLocation(frame);

      row.append(nameSpan, locSpan);
      row.addEventListener('click', () => {
        void this.selectFrame(frame.id);
      });

      this.stackList.append(row);
    }
  }

  private renderVariables(): void {
    this.variablesList.replaceChildren();
    let totalVars = 0;

    for (const vars of this.state.variables.values()) {
      totalVars += vars.length;
    }
    this.variablesCount.textContent = `${totalVars} var${totalVars === 1 ? '' : 's'}`;

    if (this.state.variables.size === 0) {
      const hint = document.createElement('p');
      hint.className = 'hint';
      hint.textContent = this.state.isPaused ? 'No variables in scope.' : 'No variables.';
      this.variablesList.append(hint);
      return;
    }

    for (const [ref, vars] of this.state.variables) {
      for (const variable of vars) {
        const row = document.createElement('div');
        row.className = 'debug-item debug-var-item';
        row.tabIndex = 0;
        row.setAttribute('role', 'treeitem');

        const isExpandable = variable.variablesReference > 0;
        const isExpanded = isExpandable && this.state.expandedVarRefs.has(variable.variablesReference);

        if (isExpandable) {
          row.classList.add('expandable');
          if (isExpanded) row.classList.add('expanded');
        }

        const prefixSpan = document.createElement('span');
        prefixSpan.className = 'debug-var-toggle';
        prefixSpan.textContent = isExpandable ? (isExpanded ? '▼ ' : '▶ ') : '  ';

        const nameSpan = document.createElement('span');
        nameSpan.className = 'debug-var-name';
        nameSpan.textContent = variable.name;

        const valSpan = document.createElement('span');
        valSpan.className = 'debug-var-value';
        valSpan.textContent = `: ${variable.value}`;

        row.append(prefixSpan, nameSpan, valSpan);

        if (variable.type) {
          const typeSpan = document.createElement('span');
          typeSpan.className = 'debug-var-type';
          typeSpan.textContent = ` (${variable.type})`;
          row.append(typeSpan);
        }

        if (isExpandable) {
          row.addEventListener('click', () => {
            void this.toggleVariable(variable);
          });
        }

        this.variablesList.append(row);

        // If expanded, render children underneath
        if (isExpanded) {
          const children = this.state.variables.get(variable.variablesReference) ?? [];
          for (const child of children) {
            const childRow = document.createElement('div');
            childRow.className = 'debug-item debug-var-item debug-var-child';
            childRow.tabIndex = 0;
            childRow.setAttribute('role', 'treeitem');

            const childName = document.createElement('span');
            childName.className = 'debug-var-name';
            childName.textContent = child.name;

            const childVal = document.createElement('span');
            childVal.className = 'debug-var-value';
            childVal.textContent = `: ${child.value}`;

            childRow.append(childName, childVal);
            this.variablesList.append(childRow);
          }
        }
      }
    }
  }

  private renderBreakpoints(): void {
    let totalBps = 0;
    this.breakpointsList.replaceChildren();

    for (const bps of this.state.breakpoints.values()) {
      totalBps += bps.length;
    }
    this.breakpointsCount.textContent = String(totalBps);

    if (totalBps === 0) {
      const hint = document.createElement('p');
      hint.className = 'hint';
      hint.textContent = 'No breakpoints set.';
      this.breakpointsList.append(hint);
      return;
    }

    for (const [path, bps] of this.state.breakpoints) {
      for (const bp of bps) {
        const row = document.createElement('div');
        row.className = 'debug-item debug-bp-item';
        row.tabIndex = 0;
        row.setAttribute('role', 'button');
        row.setAttribute('aria-label', `Breakpoint at ${formatBreakpoint(bp)}`);

        const indicator = document.createElement('span');
        indicator.className = `debug-bp-indicator ${bp.verified ? 'verified' : 'pending'}`;
        indicator.title = bp.verified ? 'Verified breakpoint' : (bp.message ?? 'Unverified breakpoint');

        const label = document.createElement('span');
        label.className = 'debug-bp-label';
        label.textContent = formatBreakpoint(bp);

        const removeBtn = document.createElement('button');
        removeBtn.className = 'button ghost compact debug-bp-remove';
        removeBtn.type = 'button';
        removeBtn.textContent = '×';
        removeBtn.title = 'Remove breakpoint';
        removeBtn.addEventListener('click', (e) => {
          e.stopPropagation();
          void this.removeBreakpoint(bp);
        });

        row.append(indicator, label, removeBtn);
        row.addEventListener('click', () => {
          void this.deps.openFileAtLine(bp.path, bp.line);
        });

        this.breakpointsList.append(row);
      }
    }
  }

  private async removeBreakpoint(bp: DapBreakpoint): Promise<void> {
    const res = toggleBreakpointInState(this.state, bp.path, bp.line);
    this.state = res.nextState;
    this.renderBreakpoints();

    if (this.state.running) {
      const filename = bp.path.split(/[\\/]/).pop() || bp.path;
      try {
        await this.sendDap('setBreakpoints', {
          source: { path: bp.path, name: filename },
          breakpoints: res.lines.map((l) => ({ line: l })),
        });
      } catch (error) {
        this.log(`[error] setBreakpoints failed: ${String(error)}`);
      }
    }
  }

  // --- Keyboard navigation helpers ---

  private handleStackKeydown(e: KeyboardEvent): void {
    const items = Array.from(this.stackList.querySelectorAll<HTMLElement>('.debug-item'));
    if (items.length === 0) return;
    const currentIndex = items.indexOf(document.activeElement as HTMLElement);

    if (e.key === 'ArrowDown') {
      e.preventDefault();
      const next = items[Math.min(items.length - 1, currentIndex + 1)] ?? items[0];
      next.focus();
    } else if (e.key === 'ArrowUp') {
      e.preventDefault();
      const prev = items[Math.max(0, currentIndex - 1)] ?? items[items.length - 1];
      prev.focus();
    } else if (e.key === 'Enter' || e.key === ' ') {
      e.preventDefault();
      (document.activeElement as HTMLElement)?.click();
    }
  }

  private handleVariablesKeydown(e: KeyboardEvent): void {
    const items = Array.from(this.variablesList.querySelectorAll<HTMLElement>('.debug-item'));
    if (items.length === 0) return;
    const currentIndex = items.indexOf(document.activeElement as HTMLElement);

    if (e.key === 'ArrowDown') {
      e.preventDefault();
      const next = items[Math.min(items.length - 1, currentIndex + 1)] ?? items[0];
      next.focus();
    } else if (e.key === 'ArrowUp') {
      e.preventDefault();
      const prev = items[Math.max(0, currentIndex - 1)] ?? items[items.length - 1];
      prev.focus();
    } else if (e.key === 'Enter' || e.key === ' ' || e.key === 'ArrowRight' || e.key === 'ArrowLeft') {
      e.preventDefault();
      (document.activeElement as HTMLElement)?.click();
    }
  }

  private handleBreakpointsKeydown(e: KeyboardEvent): void {
    const items = Array.from(this.breakpointsList.querySelectorAll<HTMLElement>('.debug-item'));
    if (items.length === 0) return;
    const currentIndex = items.indexOf(document.activeElement as HTMLElement);

    if (e.key === 'ArrowDown') {
      e.preventDefault();
      const next = items[Math.min(items.length - 1, currentIndex + 1)] ?? items[0];
      next.focus();
    } else if (e.key === 'ArrowUp') {
      e.preventDefault();
      const prev = items[Math.max(0, currentIndex - 1)] ?? items[items.length - 1];
      prev.focus();
    } else if (e.key === 'Enter' || e.key === ' ') {
      e.preventDefault();
      (document.activeElement as HTMLElement)?.click();
    }
  }
}
