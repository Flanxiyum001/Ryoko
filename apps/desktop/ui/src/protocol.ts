import { detectLanguage } from './languages';

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

export function languageForPath(path: string, contents = ''): string {
  return detectLanguage(path, contents);
}

export function markerSeverityFor(severity: number | undefined): number {
  return severity !== undefined ? MARKER_SEVERITY[severity] ?? 2 : 2;
}

export function joinWorkspacePath(root: string, relative: string): string {
  const normalizedRoot = root.replace(/\\/g, '/').replace(/\/+$/, '');
  const normalizedRelative = relative.replace(/\\/g, '/').replace(/^\/+/, '');
  return normalizedRoot + '/' + normalizedRelative;
}

/** Normalizes filesystem paths for editor/LSP identity, not native filesystem I/O. */
export function normalizeFilePath(path: string): string {
  const normalized = path.replace(/\\/g, '/');
  // Rust canonicalize() returns extended-length paths on Windows.
  if (/^\/\/\?\/UNC\//i.test(normalized)) return '//' + normalized.slice(8);
  if (/^\/\/\?\/[A-Za-z]:\//.test(normalized)) return normalized.slice(4);
  return normalized;
}

/** Resolves a user-picked path to a workspace-relative path; native reads recheck containment. */
export function workspaceRelativeFile(root: string, selected: string): string | null {
  const base = normalizeFilePath(root).replace(/\/+$/, '');
  const candidate = normalizeFilePath(selected);
  const absolute = /^(?:\/|[A-Za-z]:)/.test(candidate);
  const relative = absolute ? null : candidate;
  const windows = /^[A-Za-z]:\/|^\/\//.test(base + '/');
  const prefix = base + '/';
  const matches = windows
    ? candidate.toLowerCase().startsWith(prefix.toLowerCase())
    : candidate.startsWith(prefix);
  const path = relative ?? (matches ? candidate.slice(prefix.length) : '');
  const segments = path.split('/').filter((segment) => segment && segment !== '.');
  if (!segments.length || segments.includes('..')) return null;
  return segments.join('/');
}

/** Builds an LSP-compatible file URI from an absolute filesystem path. */
export function fileUri(path: string): string {
  const normalized = normalizeFilePath(path);
  const encoded = normalized
    .split('/')
    .map((segment) => (/^[A-Za-z]:$/.test(segment) ? segment : encodeURIComponent(segment)))
    .join('/');
  // A UNC server belongs in the authority, not in a double-slash URI path.
  if (/^\/\/[^/]+\//.test(normalized)) return 'file:' + encoded;
  return 'file:///' + encoded.replace(/^\/+/, '');
}

/** Inverts fileUri; returns null for non-file or undecodable URIs. */
export function pathFromFileUri(uri: string): string | null {
  if (!uri.startsWith('file://')) return null;
  try {
    const decoded = decodeURIComponent(uri.slice('file://'.length));
    if (!decoded.startsWith('/')) return '//' + decoded;
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
