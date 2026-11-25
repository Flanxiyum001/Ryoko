import { describe, expect, it } from 'vitest';
import {
  fileUri,
  isPublishDiagnostics,
  languageForPath,
  markerSeverityFor,
  normalizeFilePath,
  parseJsonRpc,
  joinWorkspacePath,
  pathFromFileUri,
  workspaceRelativeFile,
} from './protocol';

describe('language detection', () => {
  it('maps known extensions and falls back to plaintext', () => {
    expect(languageForPath('src/main.rs')).toBe('rust');
    expect(languageForPath('web/app.tsx')).toBe('typescript');
    expect(languageForPath('C:\\repo\\README.md')).toBe('markdown');
    expect(languageForPath('LICENSE')).toBe('plaintext');
    expect(languageForPath('weird.unknown')).toBe('plaintext');
  });
});

describe('file URIs', () => {
  it('encodes spaces and keeps the leading slash', () => {
    expect(fileUri('/home/me/My Project/src/main.rs')).toBe(
      'file:///home/me/My%20Project/src/main.rs',
    );
  });

  it('normalises Windows paths to file:///C:/...', () => {
    expect(fileUri('C:\\work\\ryoko\\main.rs')).toBe('file:///C:/work/ryoko/main.rs');
  });

  it('opens files under Rust-canonicalized Windows drive roots', () => {
    const root = '\\\\?\\C:\\work\\My Project';
    const uri = fileUri(joinWorkspacePath(root, 'src/main #1%.rs'));
    expect(uri).toBe('file:///C:/work/My%20Project/src/main%20%231%25.rs');
    expect(pathFromFileUri(uri)).toBe('C:/work/My Project/src/main #1%.rs');
    expect(normalizeFilePath(root)).toBe('C:/work/My Project');
  });

  it('uses the server as authority for ordinary and extended UNC roots', () => {
    const ordinary = '\\\\server\\share\\project';
    const extended = '\\\\?\\UNC\\server\\share\\project';
    const expected = 'file://server/share/project/src/main.rs';
    for (const root of [ordinary, extended]) {
      const uri = fileUri(joinWorkspacePath(root, 'src/main.rs'));
      expect(uri).toBe(expected);
      expect(pathFromFileUri(uri)).toBe('//server/share/project/src/main.rs');
      expect(normalizeFilePath(root)).toBe('//server/share/project');
    }
  });

  it('does not produce an empty authority with a double-slash path', () => {
    expect(fileUri('///home/me/main.rs')).toBe('file:///home/me/main.rs');
  });

  it('round-trips absolute paths and rejects non-file URIs', () => {
    expect(pathFromFileUri('file:///home/me/My%20Project/src/main.rs')).toBe(
      '/home/me/My Project/src/main.rs',
    );
    expect(pathFromFileUri('file:///C:/work/main.rs')).toBe('C:/work/main.rs');
    expect(pathFromFileUri('untitled:Untitled-1')).toBeNull();
  });

  it('resolves direct file picks without depending on the file index', () => {
    expect(workspaceRelativeFile('/repo', '/repo/ignored/file.xyz')).toBe('ignored/file.xyz');
    expect(workspaceRelativeFile('/repo', 'ignored/file.xyz')).toBe('ignored/file.xyz');
    expect(workspaceRelativeFile('/repo', '/repository/file.xyz')).toBeNull();
    expect(workspaceRelativeFile('/repo', '/other/file.xyz')).toBeNull();
    expect(workspaceRelativeFile('\\\\?\\C:\\Repo', 'c:\\repo\\file.xyz')).toBe('file.xyz');
    expect(workspaceRelativeFile('\\\\?\\UNC\\server\\share', '\\\\server\\share\\file.xyz')).toBe('file.xyz');
  });

  it('joins workspace roots without doubling or dropping separators', () => {
    expect(joinWorkspacePath('/repo/', '/src/lib.rs')).toBe('/repo/src/lib.rs');
    expect(joinWorkspacePath('C:\\repo', 'src\\lib.rs')).toBe('C:/repo/src/lib.rs');
  });
});

describe('marker severities', () => {
  it('maps LSP error/warning/info/hint to Monaco values', () => {
    expect(markerSeverityFor(1)).toBe(8);
    expect(markerSeverityFor(2)).toBe(4);
    expect(markerSeverityFor(3)).toBe(2);
    expect(markerSeverityFor(4)).toBe(1);
    expect(markerSeverityFor(undefined)).toBe(2);
  });
});

describe('JSON-RPC handling', () => {
  it('parses messages and rejects malformed input', () => {
    expect(parseJsonRpc('{"jsonrpc":"2.0","id":1}')).toMatchObject({ id: 1 });
    expect(parseJsonRpc('not json')).toBeNull();
    expect(parseJsonRpc('42')).toBeNull();
  });

  it('recognises publishDiagnostics with a diagnostics array', () => {
    const message = parseJsonRpc(
      JSON.stringify({
        jsonrpc: '2.0',
        method: 'textDocument/publishDiagnostics',
        params: { uri: 'file:///a.rs', diagnostics: [{ range: { start: { line: 0, character: 0 }, end: { line: 0, character: 1 } }, message: 'x' }] },
      }),
    );
    expect(message).not.toBeNull();
    expect(isPublishDiagnostics(message!)).toBe(true);

    const withoutDiagnostics = parseJsonRpc(
      JSON.stringify({ method: 'textDocument/publishDiagnostics', params: { uri: 'file:///a.rs' } }),
    );
    expect(isPublishDiagnostics(withoutDiagnostics!)).toBe(false);
  });
});
