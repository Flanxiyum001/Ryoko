import { describe, expect, it } from 'vitest';
import {
  builtinServerRequestResult,
  ownerForRequest,
  serversForNotification,
} from './lsp_routing';

const registry = [
  { id: 'rust-analyzer', languages: ['rust'] },
  { id: 'pyright', languages: ['python'] },
];

describe('ownerForRequest', () => {
  it('routes document requests to the language owner', () => {
    expect(ownerForRequest(registry, 'rust', 'textDocument/completion')).toBe('rust-analyzer');
    expect(ownerForRequest(registry, 'python', 'textDocument/hover')).toBe('pyright');
  });

  it('falls back to ryoko-lsp without a matching server', () => {
    expect(ownerForRequest(registry, 'go', 'textDocument/completion')).toBe('ryoko-lsp');
  });

  it('keeps workspace-wide requests on ryoko-lsp', () => {
    expect(ownerForRequest(registry, 'rust', 'workspace/symbol')).toBe('ryoko-lsp');
    expect(ownerForRequest(registry, 'rust', 'workspace/executeCommand')).toBe('ryoko-lsp');
  });
});

describe('serversForNotification', () => {
  it('fans document sync out to every running server', () => {
    expect(serversForNotification(['rust-analyzer', 'pyright'], 'textDocument/didOpen'))
      .toEqual(['rust-analyzer', 'pyright']);
  });

  it('silences internal $/ notifications', () => {
    expect(serversForNotification(['rust-analyzer'], '$/progress')).toEqual([]);
  });

  it('keeps custom notifications on ryoko-lsp only', () => {
    expect(serversForNotification(['rust-analyzer'], 'ryoko/agentEvent')).toEqual([]);
    expect(serversForNotification(['rust-analyzer', 'ryoko-lsp'], 'window/showMessage'))
      .toEqual(['ryoko-lsp']);
  });
});

describe('builtinServerRequestResult', () => {
  it('answers configuration with one object per item', () => {
    const answered = builtinServerRequestResult(
      'workspace/configuration',
      { items: [{}, {}] },
      [],
    );
    expect(answered.supported).toBe(true);
    expect(answered.result).toEqual([{}, {}]);
  });

  it('acknowledges dynamic capability registration', () => {
    expect(builtinServerRequestResult('client/registerCapability', {}, []).supported).toBe(true);
    expect(builtinServerRequestResult('client/unregisterCapability', {}, []).supported).toBe(true);
  });

  it('refuses unsupported requests with MethodNotFound semantics', () => {
    const answered = builtinServerRequestResult('window/showDocument', {}, []);
    expect(answered.supported).toBe(false);
  });

  it('exposes the workspace folders when asked', () => {
    const folders = [{ uri: 'file:///w', name: 'w' }];
    expect(builtinServerRequestResult('workspace/workspaceFolders', null, folders).result)
      .toEqual(folders);
  });
});
