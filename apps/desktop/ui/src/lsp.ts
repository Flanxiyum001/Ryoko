import { invoke } from '@tauri-apps/api/core';
import { listen, type UnlistenFn } from '@tauri-apps/api/event';
import {
  fileUri,
  isPublishDiagnostics,
  parseJsonRpc,
  type JsonRpcMessage,
  type PublishDiagnosticsParams,
} from './protocol';

export type LspState = 'offline' | 'starting' | 'ready' | 'error' | 'stopped';

export interface LspHandlers {
  onDiagnostics: (params: PublishDiagnosticsParams) => void;
  onState: (state: LspState, detail?: string) => void;
}

const INITIALIZE_TIMEOUT_MS = 8_000;

function withTimeout<T>(promise: Promise<T>, milliseconds: number): Promise<T> {
  return new Promise((resolve, reject) => {
    const timer = setTimeout(
      () => reject(new Error(`language server did not answer within ${milliseconds}ms`)),
      milliseconds,
    );
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
 * Typed JSON-RPC client for the `ryoko-lsp` process the Rust shell spawns.
 * Messages cross the Tauri boundary as JSON strings; the Rust side owns the
 * `Content-Length` framing and the child process lifecycle.
 */
export class LspClient {
  private nextId = 1;
  private pending = new Map<number, (message: JsonRpcMessage) => void>();
  private unlisteners: UnlistenFn[] = [];
  private opened = new Set<string>();
  private active = false;

  constructor(private readonly handlers: LspHandlers) {}

  async start(root: string): Promise<void> {
    await this.stop();
    this.handlers.onState('starting');
    this.unlisteners = [
      await listen<string>('ryoko-lsp-message', (event) => this.receive(event.payload)),
      await listen<string>('ryoko-lsp-log', (event) => console.debug('[ryoko-lsp]', event.payload)),
      await listen('ryoko-lsp-closed', () => {
        if (this.active) this.handlers.onState('stopped');
      }),
    ];
    try {
      await invoke('lsp_start', { root });
      const response = await withTimeout(
        this.request('initialize', {
          processId: null,
          rootUri: fileUri(root),
          capabilities: {
            textDocument: {
              synchronization: {},
              publishDiagnostics: { relatedInformation: true },
            },
            workspace: { workspaceFolders: true },
          },
          workspaceFolders: [
            { uri: fileUri(root), name: root.split(/[\\/]/).filter(Boolean).pop() ?? 'workspace' },
          ],
        }),
        INITIALIZE_TIMEOUT_MS,
      );
      if (response.error) throw new Error(response.error.message ?? 'initialize failed');
      this.active = true;
      this.notify('initialized', {});
      this.handlers.onState('ready');
    } catch (error) {
      this.handlers.onState('error', error instanceof Error ? error.message : String(error));
      await this.stop();
    }
  }

  async stop(): Promise<void> {
    if (!this.active && this.unlisteners.length === 0) return;
    this.active = false;
    this.opened.clear();
    for (const unlisten of this.unlisteners) unlisten();
    this.unlisteners = [];
    this.pending.clear();
    try {
      await invoke('lsp_stop');
    } catch {
      // The server was never started; nothing to stop.
    }
  }

  /** Sends `didOpen` once per document, then tracks changes against it. */
  ensureOpen(uri: string, languageId: string, version: number, text: string): void {
    if (!this.active || this.opened.has(uri)) return;
    this.opened.add(uri);
    this.notify('textDocument/didOpen', {
      textDocument: { uri, languageId, version, text },
    });
  }

  /**
   * ryoko-lsp advertises incremental sync but its document store accepts a
   * range-less full replacement, which is what Monaco emits here.
   */
  change(uri: string, version: number, text: string): void {
    if (!this.active || !this.opened.has(uri)) return;
    this.notify('textDocument/didChange', {
      textDocument: { uri, version },
      contentChanges: [{ text }],
    });
  }

  close(uri: string): void {
    if (!this.opened.delete(uri)) return;
    this.notify('textDocument/didClose', { textDocument: { uri } });
  }

  private request(method: string, params: unknown): Promise<JsonRpcMessage> {
    const id = this.nextId++;
    return new Promise((resolve) => {
      this.pending.set(id, resolve);
      this.post({ jsonrpc: '2.0', id, method, params });
    });
  }

  private notify(method: string, params: unknown): void {
    this.post({ jsonrpc: '2.0', method, params });
  }

  private post(message: unknown): void {
    invoke('lsp_send', { message: JSON.stringify(message) }).catch((error: unknown) => {
      this.handlers.onState('error', `cannot reach ryoko-lsp: ${String(error)}`);
    });
  }

  private receive(text: string): void {
    const message = parseJsonRpc(text);
    if (!message) return;
    if (typeof message.id === 'number' && this.pending.has(message.id)) {
      const resolve = this.pending.get(message.id);
      this.pending.delete(message.id);
      resolve?.(message);
      return;
    }
    if (isPublishDiagnostics(message)) {
      this.handlers.onDiagnostics(message.params);
    }
  }
}
