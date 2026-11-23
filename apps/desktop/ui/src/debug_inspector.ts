//! Pure domain types, message validators and state reducers for the DAP inspector
//! (call stack frames, variables tree, breakpoints and session lifecycle).

export const MAX_STACK_FRAMES = 100;
export const MAX_SCOPES = 20;
export const MAX_VARIABLES = 100;
export const MAX_VARIABLE_VALUE_LENGTH = 1_000;
export const MAX_LOG_LINES = 300;

export interface DapSource {
  name?: string;
  path?: string;
}

export interface DapStackFrame {
  id: number;
  name: string;
  line: number;
  column?: number;
  source?: DapSource;
}

export interface DapScope {
  name: string;
  variablesReference: number;
  expensive: boolean;
}

export interface DapVariable {
  name: string;
  value: string;
  type?: string;
  variablesReference: number;
}

export interface DapBreakpoint {
  id?: number;
  path: string;
  line: number;
  verified: boolean;
  message?: string;
}

export interface DebugState {
  running: boolean;
  sessionId: number | null;
  statusText: string;
  isPaused: boolean;
  pauseReason?: string;
  threadId: number;
  frames: DapStackFrame[];
  selectedFrameId: number | null;
  scopes: DapScope[];
  variables: Map<number, DapVariable[]>;
  breakpoints: Map<string, DapBreakpoint[]>;
  expandedVarRefs: Set<number>;
  logLines: string[];
}

export function createInitialDebugState(): DebugState {
  return {
    running: false,
    sessionId: null,
    statusText: 'Start a DAP adapter over stdio. No debugger is emulated.',
    isPaused: false,
    threadId: 1,
    frames: [],
    selectedFrameId: null,
    scopes: [],
    variables: new Map(),
    breakpoints: new Map(),
    expandedVarRefs: new Set(),
    logLines: [],
  };
}

/** Parses raw DAP payload string into JSON object safely without throwing. */
export function parseDapMessage(raw: string): Record<string, unknown> | null {
  if (typeof raw !== 'string' || raw.trim().length === 0) return null;
  try {
    const parsed = JSON.parse(raw) as unknown;
    if (parsed && typeof parsed === 'object' && !Array.isArray(parsed)) {
      return parsed as Record<string, unknown>;
    }
  } catch {
    // Malformed JSON is safely dropped without crashing
  }
  return null;
}

/** Validates and bounds stack frames from a stackTrace response body. */
export function validateStackFrames(body: unknown): DapStackFrame[] {
  if (!body || typeof body !== 'object') return [];
  const rawFrames = (body as { stackFrames?: unknown }).stackFrames;
  if (!Array.isArray(rawFrames)) return [];

  const results: DapStackFrame[] = [];
  for (const item of rawFrames.slice(0, MAX_STACK_FRAMES)) {
    if (!item || typeof item !== 'object') continue;
    const id = (item as { id?: unknown }).id;
    if (typeof id !== 'number' || !Number.isFinite(id)) continue;

    const rawName = (item as { name?: unknown }).name;
    const name = typeof rawName === 'string' && rawName.trim() ? rawName.slice(0, 300) : 'anonymous';

    const rawLine = (item as { line?: unknown }).line;
    const line = typeof rawLine === 'number' && Number.isFinite(rawLine) && rawLine >= 0 ? Math.floor(rawLine) : 0;

    const rawCol = (item as { column?: unknown }).column;
    const column = typeof rawCol === 'number' && Number.isFinite(rawCol) && rawCol >= 0 ? Math.floor(rawCol) : undefined;

    let source: DapSource | undefined;
    const rawSource = (item as { source?: unknown }).source;
    if (rawSource && typeof rawSource === 'object') {
      const srcName = (rawSource as { name?: unknown }).name;
      const srcPath = (rawSource as { path?: unknown }).path;
      source = {
        name: typeof srcName === 'string' ? srcName.slice(0, 500) : undefined,
        path: typeof srcPath === 'string' ? srcPath.slice(0, 1000) : undefined,
      };
    }

    results.push({ id, name, line, column, source });
  }
  return results;
}

/** Validates and bounds scopes from a scopes response body. */
export function validateScopes(body: unknown): DapScope[] {
  if (!body || typeof body !== 'object') return [];
  const rawScopes = (body as { scopes?: unknown }).scopes;
  if (!Array.isArray(rawScopes)) return [];

  const results: DapScope[] = [];
  for (const item of rawScopes.slice(0, MAX_SCOPES)) {
    if (!item || typeof item !== 'object') continue;
    const name = typeof (item as { name?: unknown }).name === 'string'
      ? (item as { name: string }).name.slice(0, 100)
      : 'Scope';
    const ref = (item as { variablesReference?: unknown }).variablesReference;
    if (typeof ref !== 'number' || !Number.isFinite(ref) || ref < 0) continue;
    const expensive = Boolean((item as { expensive?: unknown }).expensive);

    results.push({
      name,
      variablesReference: Math.floor(ref),
      expensive,
    });
  }
  return results;
}

/** Validates and bounds variables from a variables response body. */
export function validateVariables(body: unknown): DapVariable[] {
  if (!body || typeof body !== 'object') return [];
  const rawVars = (body as { variables?: unknown }).variables;
  if (!Array.isArray(rawVars)) return [];

  const results: DapVariable[] = [];
  for (const item of rawVars.slice(0, MAX_VARIABLES)) {
    if (!item || typeof item !== 'object') continue;
    const rawName = (item as { name?: unknown }).name;
    const name = typeof rawName === 'string' && rawName.trim() ? rawName.slice(0, 200) : '(unnamed)';

    const rawVal = (item as { value?: unknown }).value;
    const value = typeof rawVal === 'string'
      ? rawVal.slice(0, MAX_VARIABLE_VALUE_LENGTH)
      : String(rawVal ?? '').slice(0, MAX_VARIABLE_VALUE_LENGTH);

    const rawType = (item as { type?: unknown }).type;
    const type = typeof rawType === 'string' ? rawType.slice(0, 100) : undefined;

    const ref = (item as { variablesReference?: unknown }).variablesReference;
    const variablesReference = typeof ref === 'number' && Number.isFinite(ref) && ref >= 0
      ? Math.floor(ref)
      : 0;

    results.push({ name, value, type, variablesReference });
  }
  return results;
}

/** Validates and bounds breakpoints returned from a setBreakpoints response or event. */
export function validateBreakpoints(body: unknown, fallbackPath: string): DapBreakpoint[] {
  if (!body || typeof body !== 'object') return [];
  const rawBreakpoints = (body as { breakpoints?: unknown }).breakpoints;
  if (!Array.isArray(rawBreakpoints)) return [];

  const results: DapBreakpoint[] = [];
  for (const item of rawBreakpoints) {
    if (!item || typeof item !== 'object') continue;
    const id = typeof (item as { id?: unknown }).id === 'number' ? (item as { id: number }).id : undefined;
    const rawLine = (item as { line?: unknown }).line;
    const line = typeof rawLine === 'number' && Number.isFinite(rawLine) ? Math.floor(rawLine) : 1;
    const verified = Boolean((item as { verified?: unknown }).verified);
    const rawMsg = (item as { message?: unknown }).message;
    const message = typeof rawMsg === 'string' ? rawMsg.slice(0, 500) : undefined;

    const rawSource = (item as { source?: unknown }).source;
    let path = fallbackPath;
    if (rawSource && typeof rawSource === 'object') {
      const srcPath = (rawSource as { path?: unknown }).path;
      if (typeof srcPath === 'string' && srcPath.trim().length > 0) {
        path = srcPath;
      }
    }

    results.push({ id, path, line, verified, message });
  }
  return results;
}

/** Formats a stack frame's source location string, e.g. "order.rs:42:5". */
export function formatSourceLocation(frame: DapStackFrame): string {
  const file = frame.source?.name || frame.source?.path?.split(/[\\/]/).pop() || 'unknown';
  if (frame.line <= 0) return file;
  if (typeof frame.column === 'number' && frame.column > 0) {
    return `${file}:${frame.line}:${frame.column}`;
  }
  return `${file}:${frame.line}`;
}

/** Formats a breakpoint for display, e.g. "order.rs:42". */
export function formatBreakpoint(bp: DapBreakpoint): string {
  const filename = bp.path.split(/[\\/]/).pop() || bp.path;
  return `${filename}:${bp.line}`;
}

/** Handles starting a session. */
export function handleSessionStarted(state: DebugState, sessionId: number): DebugState {
  return {
    ...state,
    running: true,
    sessionId,
    statusText: 'Adapter started; initializing…',
    isPaused: false,
    frames: [],
    selectedFrameId: null,
    scopes: [],
    variables: new Map(),
    expandedVarRefs: new Set(),
  };
}

/** Handles stopping a session without discarding configured breakpoints. */
export function handleSessionStopped(state: DebugState, statusMessage = 'Debug adapter stopped'): DebugState {
  return {
    ...state,
    running: false,
    sessionId: null,
    statusText: statusMessage,
    isPaused: false,
    pauseReason: undefined,
    frames: [],
    selectedFrameId: null,
    scopes: [],
    variables: new Map(),
    expandedVarRefs: new Set(),
  };
}

/** Handles a workspace reset (clearing all breakpoints, logs and inspector state). */
export function handleWorkspaceReset(): DebugState {
  return createInitialDebugState();
}

/** Handles a DAP `stopped` event. */
export function handleStoppedEvent(
  state: DebugState,
  body: unknown,
): { nextState: DebugState; threadId: number } {
  let threadId = state.threadId;
  let reason = 'paused';
  let description: string | undefined;

  if (body && typeof body === 'object') {
    const rawThreadId = (body as { threadId?: unknown }).threadId;
    if (typeof rawThreadId === 'number' && Number.isFinite(rawThreadId)) {
      threadId = rawThreadId;
    }
    const rawReason = (body as { reason?: unknown }).reason;
    if (typeof rawReason === 'string' && rawReason.trim().length > 0) {
      reason = rawReason.trim();
    }
    const rawDesc = (body as { description?: unknown }).description;
    if (typeof rawDesc === 'string' && rawDesc.trim().length > 0) {
      description = rawDesc.trim();
    }
  }

  const statusText = description ? `Stopped: ${description}` : `Stopped: ${reason}`;
  return {
    nextState: {
      ...state,
      isPaused: true,
      pauseReason: reason,
      threadId,
      statusText,
      frames: [],
      selectedFrameId: null,
      scopes: [],
      variables: new Map(),
      expandedVarRefs: new Set(),
    },
    threadId,
  };
}

/** Handles a DAP `stackTrace` response. */
export function handleStackTraceResponse(
  state: DebugState,
  body: unknown,
): { nextState: DebugState; topFrame: DapStackFrame | null } {
  const frames = validateStackFrames(body);
  const topFrame = frames[0] ?? null;
  return {
    nextState: {
      ...state,
      frames,
      selectedFrameId: topFrame?.id ?? null,
    },
    topFrame,
  };
}

/** Handles selecting a stack frame in the inspector. */
export function selectStackFrame(
  state: DebugState,
  frameId: number,
): { nextState: DebugState; selectedFrame: DapStackFrame | null } {
  const selectedFrame = state.frames.find((frame) => frame.id === frameId) ?? null;
  return {
    nextState: {
      ...state,
      selectedFrameId: frameId,
      scopes: [],
      variables: new Map(),
      expandedVarRefs: new Set(),
    },
    selectedFrame,
  };
}

/** Handles a DAP `scopes` response. */
export function handleScopesResponse(
  state: DebugState,
  body: unknown,
): { nextState: DebugState; primaryScopeRef: number | null } {
  const scopes = validateScopes(body);
  const primaryScope = scopes.find((s) => !s.expensive) ?? scopes[0] ?? null;
  return {
    nextState: {
      ...state,
      scopes,
    },
    primaryScopeRef: primaryScope ? primaryScope.variablesReference : null,
  };
}

/** Handles a DAP `variables` response. */
export function handleVariablesResponse(
  state: DebugState,
  variablesReference: number,
  body: unknown,
): DebugState {
  const vars = validateVariables(body);
  const nextVariables = new Map(state.variables);
  nextVariables.set(variablesReference, vars);
  return {
    ...state,
    variables: nextVariables,
  };
}

/** Toggles expansion of a structured variable. */
export function toggleVariableExpanded(
  state: DebugState,
  variablesReference: number,
): { nextState: DebugState; shouldFetch: boolean } {
  const nextExpanded = new Set(state.expandedVarRefs);
  let shouldFetch = false;
  if (nextExpanded.has(variablesReference)) {
    nextExpanded.delete(variablesReference);
  } else {
    nextExpanded.add(variablesReference);
    if (!state.variables.has(variablesReference)) {
      shouldFetch = true;
    }
  }
  return {
    nextState: {
      ...state,
      expandedVarRefs: nextExpanded,
    },
    shouldFetch,
  };
}

/** Returns a bounded tree of variables reachable from a DAP scope.
 * The response cache may include collapsed or unrelated references: never render
 * those as additional roots. Cycle detection handles broken adapters safely.
 */
export function visibleVariablesForScope(
  state: DebugState,
  scopeReference: number,
): Array<{ variable: DapVariable; depth: number }> {
  const visible: Array<{ variable: DapVariable; depth: number }> = [];
  const visit = (reference: number, depth: number, ancestors: Set<number>): void => {
    if (ancestors.has(reference) || depth > 8 || visible.length >= 800) return;
    const nextAncestors = new Set(ancestors);
    nextAncestors.add(reference);
    for (const variable of state.variables.get(reference) ?? []) {
      if (visible.length >= 800) break;
      visible.push({ variable, depth });
      const childReference = variable.variablesReference;
      if (childReference > 0 && state.expandedVarRefs.has(childReference)) {
        visit(childReference, depth + 1, nextAncestors);
      }
    }
  };
  visit(scopeReference, 0, new Set());
  return visible;
}

/** Toggles a breakpoint in local state and returns the updated lines for DAP `setBreakpoints`. */
export function toggleBreakpointInState(
  state: DebugState,
  path: string,
  line: number,
): { nextState: DebugState; action: 'added' | 'removed'; lines: number[] } {
  const nextBreakpoints = new Map(state.breakpoints);
  const currentList = [...(nextBreakpoints.get(path) ?? [])];
  const existingIndex = currentList.findIndex((bp) => bp.line === line);

  let action: 'added' | 'removed';
  if (existingIndex >= 0) {
    currentList.splice(existingIndex, 1);
    action = 'removed';
  } else {
    currentList.push({
      path,
      line,
      verified: false,
    });
    currentList.sort((a, b) => a.line - b.line);
    action = 'added';
  }

  if (currentList.length === 0) {
    nextBreakpoints.delete(path);
  } else {
    nextBreakpoints.set(path, currentList);
  }

  const lines = currentList.map((bp) => bp.line);
  return {
    nextState: {
      ...state,
      breakpoints: nextBreakpoints,
    },
    action,
    lines,
  };
}

/** Updates breakpoint verification info from a `setBreakpoints` response. */
export function updateBreakpointsFromResponse(
  state: DebugState,
  path: string,
  body: unknown,
): DebugState {
  const verifiedList = validateBreakpoints(body, path);
  if (verifiedList.length === 0) return state;

  const nextBreakpoints = new Map(state.breakpoints);
  const currentList = [...(nextBreakpoints.get(path) ?? [])];

  for (const verifiedBp of verifiedList) {
    const existing = currentList.find((b) => b.line === verifiedBp.line);
    if (existing) {
      existing.verified = verifiedBp.verified;
      existing.id = verifiedBp.id;
      existing.message = verifiedBp.message;
    } else {
      currentList.push(verifiedBp);
    }
  }
  currentList.sort((a, b) => a.line - b.line);
  nextBreakpoints.set(path, currentList);

  return {
    ...state,
    breakpoints: nextBreakpoints,
  };
}

/** Appends a line to the bounded debug log ring buffer. */
export function appendDebugLog(state: DebugState, line: string, maxLines = MAX_LOG_LINES): DebugState {
  const boundedLine = line.length > 2000 ? `${line.slice(0, 2000)} [truncated]` : line;
  const nextLogs = [...state.logLines, boundedLine];
  if (nextLogs.length > maxLines) {
    nextLogs.splice(0, nextLogs.length - maxLines);
  }
  return {
    ...state,
    logLines: nextLogs,
  };
}
