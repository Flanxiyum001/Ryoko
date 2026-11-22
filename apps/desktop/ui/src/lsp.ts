import { invoke } from '@tauri-apps/api/core';
import { listen, type UnlistenFn } from '@tauri-apps/api/event';
import {
  fileUri,
  isPublishDiagnostics,
  parseJsonRpc,
  type JsonRpcMessage,
  type PublishDiagnosticsParams,
} from './protocol';
import {
  builtinServerRequestResult,
  RYOKO_LSP_ID,
  serversForNotification,
  type LspServerDescriptor,
} from './lsp_routing';

export type LspState = 'offline' | 'starting' | 'ready' | 'error' | 'stopped';

export interface LspServerStatus {
  id: string;
  state: LspState;
}

export interface LspStartReport {
  started: string[];
  failed: string[];
}

export interface LspRegistryReply {
  registry: {
    servers: LspServerDescriptor[];
  };
  registryPath: string | null;
}

export interface LspHandlers {
  onDiagnostics: (params: PublishDiagnosticsParams, server: string) => void;
  /** Status of the built-in `ryoko-lsp` process. */
  onState: (state: LspState, detail?: string) => void;
  /** Status snapshot for every managed server, including `ryoko-lsp`. */
  onServers?: (servers: LspServerStatus[]) => void;
  onLog?: (server: string, line: string) => void;
}

const INITIALIZE_TIMEOUT_MS = 8_000;
const CAPABILITY_TIMEOUT_MS = 3_000;

function withTimeout<T>(promise: Promise<T>, milliseconds: number, fallback?: T): Promise<T | null> {
  return new Promise((resolve, reject) => {
    const timer = setTimeout(() => {
      if (fallback !== undefined) resolve(fallback);
      else reject(new Error(`language server did not answer within ${milliseconds}ms`));
    }, milliseconds);
    promise.then(
      (value) => {
        clearTimeout(timer);
        resolve(value);
      },
      (error: unknown) => {
        clearTimeout(timer);
        reject(error);
      },
    );
  });
}

/**
 * Multi-server JSON-RPC client. The Rust shell owns process lifecycle and
 * framing; this class multiplexes: document notifications fan out to every
 * running server, document requests route to the language owner, and the
 * built-in `ryoko-lsp` keeps workspace-wide requests.
 */
export class LspClient {
  private nextId = 1;
  private registry: LspServerDescriptor[] = [];
  private serverStates = new Map<string, LspState>();
  private pending = new Map<string, Map<number, (message: JsonRpcMessage) => void>>();
  private opened = new Map<string, { languageId: string; servers: Set<string> }>();
  private unlisteners: UnlistenFn[] = [];
  private active = false;
  private workspaceRoot: string | null = null;

  constructor(private readonly handlers: LspHandlers) {}

  /** Remembered right after construction; answers workspaceFolders queries. */
  setWorkspaceRoot(root: string): void {
    this.workspaceRoot = root;
  }

  private emitServers(): void {
    this.handlers.onServers?.(
      [...this.serverStates.entries()].map(([id, state]) => ({ id, state })),
    );
  }

  private setState(id: string, state: LspState, detail?: string): void {
    this.serverStates.set(id, state);
    if (id === RYOKO_LSP_ID) this.handlers.onState(state, detail);
    this.emitServers();
  }

  async start(root: string): Promise<void> {
    await this.stop();
    this.workspaceRoot = root;
    this.handlers.onState('starting');
    this.unlisteners = [
      await listen<{ server: string; message: string }>('lsp-frame', (event) =>
        this.receive(event.payload.server, event.payload.message),
      ),
      await listen<{ server: string; line: string }>('lsp-log', (event) => {
        this.handlers.onLog?.(event.payload.server, event.payload.line);
      }),
      await listen<{ server: string }>('lsp-exit', (event) => {
        const server = event.payload.server;
        this.serverStates.delete(server);
        this.pending.delete(server);
        for (const opened of this.opened.values()) opened.servers.delete(server);
        this.emitServers();
      }),
    ];
    const report = await invoke<LspStartReport>('lsp_start', { root });
    this.setState(RYOKO_LSP_ID, 'starting');
    for (const id of report.started) {
      if (id !== RYOKO_LSP_ID) this.setState(id, 'starting');
    }
    try {
      const reply = await invoke<LspRegistryReply>('lsp_registry');
      this.registry = reply.registry.servers;
      for (const id of report.started) {
        const detail = await this.initializeServer(id, root);
        this.setReady(id, detail);
      }
      if (!this.serverStates.get(RYOKO_LSP_ID)) {
        throw new Error('ryoko-lsp exited before initialization finished');
      }
      this.active = true;
    } catch (error) {
      this.handlers.onState('error', error instanceof Error ? error.message : String(error));
      await this.stop();
    }
  }

  private async initializeServer(id: string, root: string): Promise<string | undefined> {
    const response = await withTimeout(
      this.requestTo(id, 'initialize', {
        processId: null,
        rootUri: fileUri(root),
        capabilities: {
          textDocument: {
            synchronization: {},
            publishDiagnostics: { relatedInformation: true },
            completion: { completionItem: { snippetSupport: true } },
            hover: { contentFormat: ['markdown', 'plaintext'] },
            definition: {},
          },
          workspace: { workspaceFolders: true, configuration: true },
        },
        workspaceFolders: [
          { uri: fileUri(root), name: root.split(/[\\/]/).filter(Boolean).pop() ?? 'workspace' },
        ],
      }),
      INITIALIZE_TIMEOUT_MS,
    );
    if (!response || response.error) {
      this.setState(id, 'error');
      return `${id} did not answer initialize${response?.error?.message ? `: ${response.error.message}` : ''}`;
    }
    this.notifyTo(id, 'initialized', {});
    return undefined;
  }

  private setReady(id: string, detail?: string): void {
    this.setState(id, 'ready');
    if (detail && id === RYOKO_LSP_ID) this.handlers.onState('ready', detail);
  }

  async stop(): Promise<void> {
    if (!this.active && this.unlisteners.length === 0) return;
    this.active = false;
    this.opened.clear();
    this.pending.clear();
    for (const unlisten of this.unlisteners) unlisten();
    this.unlisteners = [];
    try {
      await invoke('lsp_stop');
    } catch {
      // The servers were never started; nothing to stop.
    }
    this.serverStates.clear();
    this.emitServers();
    this.handlers.onState('offline');
  }

  /** Sends `didOpen` once per document to every running server. */
  ensureOpen(uri: string, languageId: string, version: number, text: string): void {
    if (!this.active) return;
    const servers = runningServers(this.serverStates);
    this.opened.set(uri, { languageId, servers: new Set(servers) });
    for (const server of servers) {
      this.notifyTo(server, 'textDocument/didOpen', {
        textDocument: { uri, languageId, version, text },
      });
    }
  }

  /**
   * Ryoko's document store accepts a range-less full replacement, which is
   * what Monaco emits here; standard servers accept full text as one change
   * even when they advertise incremental sync.
   */
  change(uri: string, version: number, text: string): void {
    const opened = this.opened.get(uri);
    if (!opened) return;
    for (const server of opened.servers) {
      this.notifyTo(server, 'textDocument/didChange', {
        textDocument: { uri, version },
        contentChanges: [{ text }],
      });
    }
  }

  close(uri: string): void {
    const opened = this.opened.get(uri);
    if (!opened) return;
    this.opened.delete(uri);
    for (const server of opened.servers) {
      this.notifyTo(server, 'textDocument/didClose', { textDocument: { uri } });
    }
  }

  /**
   * A document-scoped request (completion, hover, definition, ...), routed to
   * the language owner with a bounded timeout. Returns null when no running
   * server can answer.
   */
  async requestForLanguage(
    language: string,
    method: string,
    params: unknown,
  ): Promise<JsonRpcMessage | null> {
    if (!this.active) return null;
    const owner = this.registry.find((server) => server.languages.includes(language))?.id
      ?? RYOKO_LSP_ID;
    if (!runningServers(this.serverStates).includes(owner)) return null;
    return withTimeout(this.requestTo(owner, method, params), CAPABILITY_TIMEOUT_MS, null);
  }

  private requestTo(server: string, method: string, params: unknown): Promise<JsonRpcMessage | null> {
    const id = this.nextId++;
    const byServer = this.pending.get(server) ?? new Map<number, (message: JsonRpcMessage) => void>();
    this.pending.set(server, byServer);
    return new Promise((resolve) => {
      byServer.set(id, resolve);
      this.post(server, { jsonrpc: '2.0', id, method, params });
    });
  }

  private notifyTo(server: string, method: string, params: unknown): void {
    if (!this.serverStates.has(server)) return;
    this.post(server, { jsonrpc: '2.0', method, params });
  }

  private post(server: string, message: unknown): void {
    invoke('lsp_send', { server, message: JSON.stringify(message) }).catch((error: unknown) => {
      this.setState(server, 'error');
      this.handlers.onLog?.(server, `cannot reach ${server}: ${String(error)}`);
    });
  }

  private receive(server: string, text: string): void {
    const message = parseJsonRpc(text);
    if (!message) return;
    if (typeof message.id === 'number' && this.pending.get(server)?.has(message.id)) {
      const resolve = this.pending.get(server)?.get(message.id);
      this.pending.get(server)?.delete(message.id);
      resolve?.(message);
      return;
    }
    if (typeof message.id === 'number' && typeof message.method === 'string') {
      this.respondToServerRequest(server, message.id, message.method, message.params);
      return;
    }
    if (isPublishDiagnostics(message)) {
      this.handlers.onDiagnostics(message.params, server);
    }
  }

  private respondToServerRequest(
    server: string,
    id: number,
    method: string,
    params: unknown,
  ): void {
    const workspaceFolders = this.workspaceRoot
      ? [{
          uri: fileUri(this.workspaceRoot),
          name: this.workspaceRoot.split(/[\\/]/).filter(Boolean).pop() ?? 'workspace',
        }]
      : [];
    const answered = builtinServerRequestResult(method, params, workspaceFolders);
    if (!answered.supported) {
      this.post(server, {
        jsonrpc: '2.0',
        id,
        error: { code: -32601, message: `Ryoko desktop does not support ${method}` },
      });
      return;
    }
    this.post(server, { jsonrpc: '2.0', id, result: answered.result });
  }
}

function runningServers(states: Map<string, LspState>): string[] {
  return [...states.entries()]
    .filter(([, state]) => state === 'ready')
    .map(([id]) => id);
}

// Re-exported so callers can treat routing as part of the client module.
export { RYOKO_LSP_ID, serversForNotification };
