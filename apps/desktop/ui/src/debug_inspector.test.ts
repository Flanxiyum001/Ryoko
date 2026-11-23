import { describe, expect, it } from 'vitest';
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
  validateBreakpoints,
  validateScopes,
  validateStackFrames,
  validateVariables,
  visibleVariablesForScope,
  MAX_STACK_FRAMES,
  MAX_VARIABLE_VALUE_LENGTH,
  MAX_LOG_LINES,
} from './debug_inspector';

describe('DAP message parsing & validation', () => {
  it('parses valid JSON objects and rejects invalid/non-object payloads', () => {
    expect(parseDapMessage('{"type":"event","event":"stopped"}')).toMatchObject({
      type: 'event',
      event: 'stopped',
    });
    expect(parseDapMessage('not json')).toBeNull();
    expect(parseDapMessage('')).toBeNull();
    expect(parseDapMessage('   ')).toBeNull();
    expect(parseDapMessage('42')).toBeNull();
    expect(parseDapMessage('true')).toBeNull();
    expect(parseDapMessage('[1, 2, 3]')).toBeNull();
  });

  it('validates and bounds stack frames safely', () => {
    expect(validateStackFrames(null)).toEqual([]);
    expect(validateStackFrames({})).toEqual([]);
    expect(validateStackFrames({ stackFrames: 'not-array' })).toEqual([]);

    const body = {
      stackFrames: [
        {
          id: 1,
          name: 'compute_hash',
          line: 42,
          column: 8,
          source: { name: 'hash.rs', path: '/src/hash.rs' },
        },
        {
          id: 2,
          name: '',
          line: 10,
        },
        {
          // Invalid: missing id
          name: 'invalid_frame',
          line: 5,
        },
      ],
    };
    const frames = validateStackFrames(body);
    expect(frames).toHaveLength(2);
    expect(frames[0]).toEqual({
      id: 1,
      name: 'compute_hash',
      line: 42,
      column: 8,
      source: { name: 'hash.rs', path: '/src/hash.rs' },
    });
    expect(frames[1].name).toBe('anonymous');
    expect(frames[1].line).toBe(10);
  });

  it('bounds stack frames array to MAX_STACK_FRAMES', () => {
    const rawFrames = Array.from({ length: 150 }, (_, i) => ({
      id: i,
      name: `frame_${i}`,
      line: i + 1,
    }));
    const frames = validateStackFrames({ stackFrames: rawFrames });
    expect(frames.length).toBe(MAX_STACK_FRAMES);
  });

  it('validates scopes with variableReference and expensive flags', () => {
    expect(validateScopes(null)).toEqual([]);
    expect(validateScopes({ scopes: [] })).toEqual([]);

    const body = {
      scopes: [
        { name: 'Locals', variablesReference: 100, expensive: false },
        { name: 'Registers', variablesReference: 200, expensive: true },
        { name: 'BadScope', variablesReference: 'invalid' },
      ],
    };
    const scopes = validateScopes(body);
    expect(scopes).toHaveLength(2);
    expect(scopes[0]).toEqual({ name: 'Locals', variablesReference: 100, expensive: false });
    expect(scopes[1]).toEqual({ name: 'Registers', variablesReference: 200, expensive: true });
  });

  it('validates variables and bounds oversized string values', () => {
    expect(validateVariables(null)).toEqual([]);

    const oversizedValue = 'x'.repeat(MAX_VARIABLE_VALUE_LENGTH + 500);
    const body = {
      variables: [
        { name: 'answer', value: '42', type: 'i32', variablesReference: 0 },
        { name: 'large_data', value: oversizedValue, type: 'String', variablesReference: 0 },
        { name: 'nested', value: 'Object', variablesReference: 105 },
      ],
    };
    const vars = validateVariables(body);
    expect(vars).toHaveLength(3);
    expect(vars[0]).toEqual({ name: 'answer', value: '42', type: 'i32', variablesReference: 0 });
    expect(vars[1].value.length).toBe(MAX_VARIABLE_VALUE_LENGTH);
    expect(vars[2].variablesReference).toBe(105);
  });

  it('validates breakpoints from DAP responses', () => {
    const body = {
      breakpoints: [
        { id: 1, line: 15, verified: true },
        { id: 2, line: 30, verified: false, message: 'Unresolved line' },
      ],
    };
    const bps = validateBreakpoints(body, '/src/main.rs');
    expect(bps).toHaveLength(2);
    expect(bps[0]).toEqual({
      id: 1,
      path: '/src/main.rs',
      line: 15,
      verified: true,
      message: undefined,
    });
    expect(bps[1]).toEqual({
      id: 2,
      path: '/src/main.rs',
      line: 30,
      verified: false,
      message: 'Unresolved line',
    });
  });
});

describe('formatting locations and breakpoints', () => {
  it('formats source locations accurately', () => {
    expect(
      formatSourceLocation({
        id: 1,
        name: 'test',
        line: 42,
        column: 5,
        source: { name: 'main.rs', path: '/src/main.rs' },
      }),
    ).toBe('main.rs:42:5');

    expect(
      formatSourceLocation({
        id: 2,
        name: 'test2',
        line: 10,
        source: { path: '/src/lib.rs' },
      }),
    ).toBe('lib.rs:10');

    expect(
      formatSourceLocation({
        id: 3,
        name: 'test3',
        line: 0,
      }),
    ).toBe('unknown');
  });

  it('formats breakpoints', () => {
    expect(formatBreakpoint({ path: '/src/core/parser.rs', line: 120, verified: true })).toBe(
      'parser.rs:120',
    );
  });
});

describe('DAP inspector state lifecycle', () => {
  it('tracks session start and clean stop without losing user breakpoints', () => {
    let state = createInitialDebugState();
    expect(state.running).toBe(false);

    // Set a breakpoint
    const bpResult = toggleBreakpointInState(state, '/src/main.rs', 24);
    state = bpResult.nextState;
    expect(bpResult.action).toBe('added');
    expect(state.breakpoints.get('/src/main.rs')).toHaveLength(1);

    // Start session
    state = handleSessionStarted(state, 101);
    expect(state.running).toBe(true);
    expect(state.sessionId).toBe(101);

    // Stop session
    state = handleSessionStopped(state);
    expect(state.running).toBe(false);
    expect(state.sessionId).toBeNull();
    // User breakpoints should remain preserved across sessions!
    expect(state.breakpoints.get('/src/main.rs')).toHaveLength(1);
  });

  it('clears all state including breakpoints on workspace reset', () => {
    let state = createInitialDebugState();
    state = toggleBreakpointInState(state, '/src/main.rs', 24).nextState;
    state = handleSessionStarted(state, 101);
    state = appendDebugLog(state, 'Log line');

    state = handleWorkspaceReset();
    expect(state.running).toBe(false);
    expect(state.sessionId).toBeNull();
    expect(state.breakpoints.size).toBe(0);
    expect(state.logLines).toHaveLength(0);
  });

  it('handles stopped event and stack trace loading', () => {
    let state = createInitialDebugState();
    state = handleSessionStarted(state, 1);

    const stopped = handleStoppedEvent(state, {
      reason: 'breakpoint',
      description: 'Hit breakpoint at line 42',
      threadId: 2,
    });
    state = stopped.nextState;
    expect(state.isPaused).toBe(true);
    expect(state.threadId).toBe(2);
    expect(state.statusText).toBe('Stopped: Hit breakpoint at line 42');

    // Receive stack trace
    const stackRes = handleStackTraceResponse(state, {
      stackFrames: [
        { id: 10, name: 'compute()', line: 42, source: { name: 'math.rs' } },
        { id: 11, name: 'main()', line: 12, source: { name: 'main.rs' } },
      ],
    });
    state = stackRes.nextState;
    expect(state.frames).toHaveLength(2);
    expect(state.selectedFrameId).toBe(10);
    expect(stackRes.topFrame?.name).toBe('compute()');

    // Scopes response
    const scopesRes = handleScopesResponse(state, {
      scopes: [
        { name: 'Locals', variablesReference: 100, expensive: false },
        { name: 'Globals', variablesReference: 200, expensive: true },
      ],
    });
    state = scopesRes.nextState;
    expect(scopesRes.primaryScopeRef).toBe(100);

    // Variables response
    state = handleVariablesResponse(state, 100, {
      variables: [
        { name: 'x', value: '10', type: 'i32', variablesReference: 0 },
        { name: 'obj', value: 'Object', variablesReference: 101 },
      ],
    });
    expect(state.variables.get(100)).toHaveLength(2);

    // Variable expansion
    const toggle1 = toggleVariableExpanded(state, 101);
    expect(toggle1.shouldFetch).toBe(true);
    state = toggle1.nextState;
    expect(state.expandedVarRefs.has(101)).toBe(true);

    // Supply child variables
    state = handleVariablesResponse(state, 101, {
      variables: [{ name: 'child_prop', value: 'true', type: 'bool', variablesReference: 0 }],
    });

    // Collapse variable
    const toggle2 = toggleVariableExpanded(state, 101);
    expect(toggle2.shouldFetch).toBe(false);
    state = toggle2.nextState;
    expect(state.expandedVarRefs.has(101)).toBe(false);
  });

  it('selects frame and resets variables for that frame', () => {
    let state = createInitialDebugState();
    state = handleStackTraceResponse(state, {
      stackFrames: [
        { id: 10, name: 'f1', line: 1 },
        { id: 11, name: 'f2', line: 2 },
      ],
    }).nextState;

    state = handleVariablesResponse(state, 100, {
      variables: [{ name: 'v', value: '1', variablesReference: 0 }],
    });
    expect(state.variables.size).toBe(1);

    const selectResult = selectStackFrame(state, 11);
    state = selectResult.nextState;
    expect(state.selectedFrameId).toBe(11);
    expect(selectResult.selectedFrame?.name).toBe('f2');
    expect(state.variables.size).toBe(0);
  });

  it('toggles breakpoints and updates verification status', () => {
    let state = createInitialDebugState();

    // Toggle on line 10
    const res1 = toggleBreakpointInState(state, '/test.rs', 10);
    state = res1.nextState;
    expect(res1.action).toBe('added');
    expect(res1.lines).toEqual([10]);

    // Toggle on line 20
    const res2 = toggleBreakpointInState(state, '/test.rs', 20);
    state = res2.nextState;
    expect(res2.action).toBe('added');
    expect(res2.lines).toEqual([10, 20]);

    // Update with DAP setBreakpoints response
    state = updateBreakpointsFromResponse(state, '/test.rs', {
      breakpoints: [
        { id: 1, line: 10, verified: true },
        { id: 2, line: 20, verified: false, message: 'Unmapped' },
      ],
    });
    const bps = state.breakpoints.get('/test.rs')!;
    expect(bps[0].verified).toBe(true);
    expect(bps[1].verified).toBe(false);
    expect(bps[1].message).toBe('Unmapped');

    // Toggle line 10 off
    const res3 = toggleBreakpointInState(state, '/test.rs', 10);
    state = res3.nextState;
    expect(res3.action).toBe('removed');
    expect(res3.lines).toEqual([20]);
  });

  it('bounds debug log buffer to MAX_LOG_LINES', () => {
    let state = createInitialDebugState();
    for (let i = 0; i < MAX_LOG_LINES + 50; i++) {
      state = appendDebugLog(state, `Line ${i}`);
    }
    expect(state.logLines.length).toBe(MAX_LOG_LINES);
    expect(state.logLines[state.logLines.length - 1]).toBe(`Line ${MAX_LOG_LINES + 49}`);
  });
});

describe('DAP variable tree visibility', () => {
  it('renders nested values once, hides collapsed refs, and ignores unrelated cached scopes', () => {
    const state = createInitialDebugState();
    state.scopes = [{ name: 'Locals', variablesReference: 1, expensive: false }];
    state.variables.set(1, [
      { name: 'user', value: 'Object', variablesReference: 2 },
      { name: 'count', value: '1', variablesReference: 0 },
    ]);
    state.variables.set(2, [{ name: 'address', value: 'Object', variablesReference: 3 }]);
    state.variables.set(3, [{ name: 'city', value: 'Hyderabad', variablesReference: 0 }]);
    state.variables.set(999, [{ name: 'unrelated', value: 'hidden', variablesReference: 0 }]);

    expect(visibleVariablesForScope(state, 1).map((row) => row.variable.name))
      .toEqual(['user', 'count']);
    state.expandedVarRefs.add(2);
    expect(visibleVariablesForScope(state, 1).map((row) => row.variable.name))
      .toEqual(['user', 'address', 'count']);
    state.expandedVarRefs.add(3);
    expect(visibleVariablesForScope(state, 1).map((row) => [row.variable.name, row.depth]))
      .toEqual([['user', 0], ['address', 1], ['city', 2], ['count', 0]]);
  });

  it('stops cyclic adapter references without duplicating root variables', () => {
    const state = createInitialDebugState();
    state.variables.set(1, [{ name: 'loop', value: 'Object', variablesReference: 1 }]);
    state.expandedVarRefs.add(1);
    expect(visibleVariablesForScope(state, 1).map((row) => row.variable.name)).toEqual(['loop']);
  });
});
