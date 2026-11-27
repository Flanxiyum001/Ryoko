import { beforeEach, describe, expect, it, vi } from 'vitest';
import { invoke } from '@tauri-apps/api/core';
import { DebugViewController } from './debug_view';

vi.mock('@tauri-apps/api/core', () => ({ invoke: vi.fn() }));

interface WireRequest {
  seq: number;
  command: string;
}

function setup(program = 'smoke.py') {
  const controller = new DebugViewController({
    openFileAtLine: async () => undefined,
    getCurrentFile: () => null,
    getEditorLine: () => null,
    getWorkspaceRoot: () => '/workspace',
    setStatus: () => undefined,
    showBottom: () => undefined,
    joinWorkspacePath: (root, relative) => root + '/' + relative,
  });
  // Exercise the actual controller and IPC requests without a native webview.
  Object.assign(controller, {
    adapterInput: { value: 'python' },
    argsInput: { value: '-m debugpy.adapter' },
    programInput: { value: program },
    render: vi.fn(),
    log: vi.fn(),
  });
  const receiver = controller as unknown as {
    handleWireMessage(payload: { sessionId: number; message: string }): void;
  };
  const requests = () => vi.mocked(invoke).mock.calls
    .filter(([command]) => command === 'dap_send')
    .map(([, args]) => JSON.parse((args as { message: string }).message) as WireRequest);
  const receive = (message: object) => receiver.handleWireMessage({
    sessionId: 1,
    message: JSON.stringify(message),
  });
  const initialize = (supportsConfigurationDoneRequest: boolean) => receive({
    type: 'response', command: 'initialize', request_seq: requests()[0].seq,
    success: true, body: { supportsConfigurationDoneRequest },
  });
  const initialized = () => receive({ type: 'event', event: 'initialized' });
  return { controller, requests, initialize, initialized };
}

describe('DAP launch handshake', () => {
  beforeEach(() => {
    vi.mocked(invoke).mockReset();
    vi.mocked(invoke).mockResolvedValue(undefined);
    vi.mocked(invoke).mockResolvedValueOnce({ running: true, sessionId: 1 });
  });

  it('launches before initialized and configures without waiting for launch response', async () => {
    const { controller, requests, initialize, initialized } = setup();
    await controller.startDebug();
    expect(requests().map(r => r.command)).toEqual(['initialize']);
    initialize(true);
    await vi.waitFor(() => expect(requests().map(r => r.command)).toEqual(['initialize', 'launch']));
    initialized();
    await vi.waitFor(() => expect(requests().map(r => r.command))
      .toEqual(['initialize', 'launch', 'configurationDone']));
    initialized();
    expect(requests().map(r => r.command)).toEqual(['initialize', 'launch', 'configurationDone']);
  });

  it('handles early initialized and does not send unsupported configurationDone', async () => {
    const { controller, requests, initialize, initialized } = setup();
    await controller.startDebug();
    initialized();
    expect(requests().map(r => r.command)).toEqual(['initialize']);
    initialize(false);
    await vi.waitFor(() => expect(controller.getState().statusText).toBe('Launch requested'));
    expect(requests().map(r => r.command)).toEqual(['initialize', 'launch']);
  });

  it('never launches without a user-provided program', async () => {
    const { controller, requests, initialize, initialized } = setup('');
    await controller.startDebug();
    initialize(true);
    initialized();
    expect(requests().map(r => r.command)).toEqual(['initialize']);
    expect(controller.getState().statusText).toContain('Set a program path');
  });
});
