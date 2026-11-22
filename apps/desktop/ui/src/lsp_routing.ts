//! Pure routing decisions for the language-server multiplexer, testable
//! without Tauri or Monaco.

export const RYOKO_LSP_ID = 'ryoko-lsp';

export interface LspServerDescriptor {
  id: string;
  languages: string[];
}

/**
 * Which server receives a client->server JSON-RPC request. Document-scoped
 * requests go to the language owner; workspace-wide execution and symbol
 * queries stay with `ryoko-lsp`, which owns cross-cutting agent features.
 */
export function ownerForRequest(
  registry: readonly LspServerDescriptor[],
  language: string,
  method: string,
): string | null {
  if (method === 'workspace/executeCommand' || method === 'workspace/symbol') {
    return RYOKO_LSP_ID;
  }
  const owner = registry.find((server) => server.languages.includes(language));
  return owner?.id ?? RYOKO_LSP_ID;
}

/** Every running server answers document-sync notifications. */
export function serversForNotification(
  running: readonly string[],
  method: string,
): string[] {
  if (
    method === 'textDocument/didOpen'
    || method === 'textDocument/didChange'
    || method === 'textDocument/didClose'
    || method === 'textDocument/didSave'
    || method === 'workspace/didChangeConfiguration'
    || method === 'workspace/didChangeWatchedFiles'
  ) {
    return [...running];
  }
  if (method.startsWith('$/')) return [];
  return running.includes(RYOKO_LSP_ID) ? [RYOKO_LSP_ID] : [];
}

/**
 * Standard server->client requests the desktop answers itself; everything
 * else is refused with MethodNotFound so servers can degrade gracefully.
 */
export function builtinServerRequestResult(
  method: string,
  params: unknown,
  workspaceFolders: ReadonlyArray<{ uri: string; name: string }>,
): { supported: boolean; result: unknown } {
  switch (method) {
    case 'client/registerCapability':
    case 'client/unregisterCapability':
      return { supported: true, result: {} };
    case 'workspace/configuration': {
      const items = (params as { items?: unknown[] } | null)?.items ?? [];
      return { supported: true, result: items.map(() => ({})) };
    }
    case 'workspace/workspaceFolders':
      return { supported: true, result: workspaceFolders };
    case 'window/workDoneProgress/create':
      return { supported: true, result: null };
    case 'window/showMessageRequest': {
      const actions = (params as { actions?: unknown[] } | null)?.actions ?? [];
      return { supported: true, result: actions[0] ?? null };
    }
    default:
      return { supported: false, result: null };
  }
}
