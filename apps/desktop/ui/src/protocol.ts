export interface LspPosition {
  line: number;
  character: number;
}

export interface LspRange {
  start: LspPosition;
  end: LspPosition;
}

export interface LspDiagnostic {
  range: LspRange;
  severity?: number;
  code?: string | number;
  source?: string;
  message: string;
}

export interface PublishDiagnosticsParams {
  uri: string;
  version?: number;
  diagnostics: LspDiagnostic[];
}

export interface JsonRpcMessage {
  jsonrpc?: string;
  id?: number | string | null;
  method?: string;
  params?: unknown;
  result?: unknown;
  error?: { code?: number; message?: string };
}

/** LSP severities (1-4) mapped to Monaco marker severities. */
const MARKER_SEVERITY: Record<number, number> = { 1: 8, 2: 4, 3: 2, 4: 1 };

const LANGUAGE_BY_EXTENSION: Record<string, string> = {
  c: 'c',
  cc: 'cpp',
  cpp: 'cpp',
  cs: 'csharp',
  css: 'css',
  go: 'go',
  h: 'cpp',
  hpp: 'cpp',
  html: 'html',
  java: 'java',
  js: 'javascript',
  json: 'json',
  jsx: 'javascript',
  md: 'markdown',
  mjs: 'javascript',
  py: 'python',
  rs: 'rust',
  sh: 'shell',
  sql: 'sql',
  toml: 'ini',
  ts: 'typescript',
  tsx: 'typescript',
  txt: 'plaintext',
  yaml: 'yaml',
  yml: 'yaml',
};

export function languageForPath(path: string): string {
  const name = path.split(/[\\/]/).pop() ?? '';
  const extension = name.includes('.') ? name.split('.').pop()?.toLowerCase() ?? '' : '';
  return LANGUAGE_BY_EXTENSION[extension] ?? 'plaintext';
}

export function markerSeverityFor(severity: number | undefined): number {
  return severity !== undefined ? MARKER_SEVERITY[severity] ?? 2 : 2;
}

export function joinWorkspacePath(root: string, relative: string): string {
  const normalizedRoot = root.replace(/\\/g, '/').replace(/\/+$/, '');
  const normalizedRelative = relative.replace(/\\/g, '/').replace(/^\/+/, '');
  return normalizedRoot + '/' + normalizedRelative;
}

/** Builds an LSP-compatible `file://` URI from an absolute filesystem path. */
export function fileUri(path: string): string {
  const normalized = path.replace(/\\/g, '/');
  const encoded = normalized
    .split('/')
    .map((segment) => (/^[A-Za-z]:$/.test(segment) ? segment : encodeURIComponent(segment)))
    .join('/');
  return 'file://' + (encoded.startsWith('/') ? encoded : '/' + encoded);
}

/** Inverts `fileUri`; returns null for non-file or undecodable URIs. */
export function pathFromFileUri(uri: string): string | null {
  if (!uri.startsWith('file://')) return null;
  try {
    const decoded = decodeURIComponent(uri.slice('file://'.length));
    return /^\/[A-Za-z]:\//.test(decoded) ? decoded.slice(1) : decoded;
  } catch {
    return null;
  }
}

export function parseJsonRpc(text: string): JsonRpcMessage | null {
  try {
    const value: unknown = JSON.parse(text);
    return typeof value === 'object' && value !== null ? (value as JsonRpcMessage) : null;
  } catch {
    return null;
  }
}

export function isPublishDiagnostics(
  message: JsonRpcMessage,
): message is JsonRpcMessage & {
  method: 'textDocument/publishDiagnostics';
  params: PublishDiagnosticsParams;
} {
  if (message.method !== 'textDocument/publishDiagnostics') return false;
  const params = message.params as Partial<PublishDiagnosticsParams> | undefined;
  return Boolean(params) && typeof params?.uri === 'string' && Array.isArray(params.diagnostics);
}
