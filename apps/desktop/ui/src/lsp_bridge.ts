//! Bridges third-party language servers behind the multiplexer to Monaco:
//! completion, hover, and definition providers per language, plus the pure
//! LSP->Monaco mapping helpers (unit-tested without Monaco).

import type { LspClient } from './lsp';

export interface LspRange {
  start: { line: number; character: number };
  end: { line: number; character: number };
}

export interface LspCompletionItem {
  label?: unknown;
  kind?: number;
  detail?: unknown;
  documentation?: unknown;
  insertText?: unknown;
  textEdit?: { newText?: unknown };
  sortText?: unknown;
  filterText?: unknown;
}

/** LSP CompletionItemKind -> Monaco CompletionItemKind numeric values. */
const LSP_TO_MONACO_KIND: Record<number, number> = {
  1: 18,
  2: 0,
  3: 1,
  4: 2,
  5: 3,
  6: 4,
  7: 5,
  8: 7,
  9: 8,
  10: 9,
  11: 12,
  12: 13,
  13: 15,
  14: 17,
  15: 24,
  16: 19,
  17: 20,
  18: 21,
  19: 22,
  20: 16,
  21: 14,
  22: 6,
  23: 10,
  24: 11,
  25: 13,
};

export function monacoKindFor(kind: number | undefined): number {
  if (kind === undefined) return 18;
  return LSP_TO_MONACO_KIND[kind] ?? 18;
}

export interface MappedCompletion {
  label: string;
  kind: number;
  detail: string;
  insert: string;
  sortKey: string;
}

/** LSP completion items -> plain suggestions; `sortText`/`filterText` kept. */
export function mapCompletions(items: unknown): MappedCompletion[] {
  if (!Array.isArray(items)) return [];
  const mapped: MappedCompletion[] = [];
  for (const raw of items) {
    if (typeof raw !== 'object' || raw === null) continue;
    const item = raw as LspCompletionItem;
    const label = typeof item.label === 'string' ? item.label : null;
    if (!label || label.length === 0) continue;
    const edit = typeof item.textEdit?.newText === 'string' ? item.textEdit.newText : null;
    const insert = edit
      ?? (typeof item.insertText === 'string' && item.insertText.length > 0 ? item.insertText : label);
    mapped.push({
      label,
      kind: monacoKindFor(item.kind),
      detail: typeof item.detail === 'string' ? item.detail : '',
      insert,
      sortKey: typeof item.sortText === 'string' ? item.sortText : label,
    });
  }
  return mapped.slice(0, 300);
}

export interface MappedHover {
  contents: string;
  range: LspRange | null;
}

/** Flattens a MarkupContent or string hover into readable markdown. */
export function mapHover(params: unknown): MappedHover | null {
  if (typeof params !== 'object' || params === null) return null;
  const hover = params as { contents?: unknown; range?: unknown };
  const contents = hoverContents(hover.contents);
  if (!contents) return null;
  const range = isRange(hover.range) ? hover.range : null;
  return { contents, range };
}

function hoverContents(contents: unknown): string | null {
  if (typeof contents === 'string') return contents.length > 0 ? contents : null;
  if (typeof contents !== 'object' || contents === null) return null;
  const markup = contents as { kind?: unknown; value?: unknown };
  if (typeof markup.value === 'string' && markup.value.length > 0) return markup.value;
  return null;
}

function isRange(value: unknown): value is LspRange {
  if (typeof value !== 'object' || value === null) return false;
  const range = value as { start?: unknown; end?: unknown };
  return isPosition(range.start) && isPosition(range.end);
}

function isPosition(value: unknown): value is { line: number; character: number } {
  if (typeof value !== 'object' || value === null) return false;
  const position = value as { line?: unknown; character?: unknown };
  return typeof position.line === 'number' && typeof position.character === 'number';
}

export interface MappedDefinition {
  uri: string;
  range: LspRange;
}

/** Normalizes Location | Location[] | LocationLink[] into locations. */
export function mapDefinitions(params: unknown): MappedDefinition[] {
  const candidates = Array.isArray(params) ? params : params === undefined || params === null ? [] : [params];
  const mapped: MappedDefinition[] = [];
  for (const raw of candidates) {
    if (typeof raw !== 'object' || raw === null) continue;
    const location = raw as Record<string, unknown>;
    if (typeof location.targetUri === 'string') {
      const target = isRange(location.targetSelectionRange)
        ? location.targetSelectionRange
        : isRange(location.targetRange)
          ? location.targetRange
          : null;
      if (target) {
        mapped.push({ uri: location.targetUri, range: target });
        continue;
      }
    }
    if (typeof location.uri === 'string' && isRange(location.range)) {
      mapped.push({ uri: location.uri, range: location.range });
    }
  }
  return mapped.slice(0, 20);
}

export const BRIDGE_TRIGGER_CHARACTERS = ['.', ':', '<', '"', '/', '@', '('];

/**
 * Registers Monaco providers for `languages` that route through the
 * multiplexer. Errors degrade to "no suggestions" so a crashed language
 * server never blocks typing.
 */
export function registerMonacoCapabilities(
  monaco: typeof import('monaco-editor'),
  lsp: LspClient,
  languages: readonly string[],
): void {
  for (const language of languages) {
    monaco.languages.registerCompletionItemProvider(language, {
      provideCompletionItems: async (model, position) => {
        const word = model.getWordUntilPosition(position);
        const range = {
          startLineNumber: position.lineNumber,
          endLineNumber: position.lineNumber,
          startColumn: word.startColumn,
          endColumn: word.endColumn,
        };
        const response = await lsp.requestForLanguage(language, 'textDocument/completion', {
          textDocument: { uri: model.uri.toString() },
          position: { line: position.lineNumber - 1, character: position.column - 1 },
        });
        const result: unknown = response?.result;
        const items = Array.isArray(result)
          ? result
          : Array.isArray((result as { items?: unknown } | null)?.items)
            ? (result as { items: unknown[] }).items
            : [];
        return {
          suggestions: mapCompletions(items).map((item) => ({
            label: item.label,
            kind: item.kind,
            detail: item.detail,
            insertText: item.insert,
            range,
            sortText: item.sortKey,
          })),
        };
      },
      triggerCharacters: BRIDGE_TRIGGER_CHARACTERS,
    });

    monaco.languages.registerHoverProvider(language, {
      provideHover: async (model, position) => {
        const response = await lsp.requestForLanguage(language, 'textDocument/hover', {
          textDocument: { uri: model.uri.toString() },
          position: { line: position.lineNumber - 1, character: position.column - 1 },
        });
        const hover = mapHover(response?.result);
        if (!hover) return null;
        const range = hover.range
          ? {
              startLineNumber: hover.range.start.line + 1,
              startColumn: hover.range.start.character + 1,
              endLineNumber: hover.range.end.line + 1,
              endColumn: hover.range.end.character + 1,
            }
          : undefined;
        return { contents: [{ value: hover.contents }], range };
      },
    });

    monaco.languages.registerDefinitionProvider(language, {
      provideDefinition: async (model, position) => {
        const response = await lsp.requestForLanguage(language, 'textDocument/definition', {
          textDocument: { uri: model.uri.toString() },
          position: { line: position.lineNumber - 1, character: position.column - 1 },
        });
        return mapDefinitions(response?.result).map((definition) => ({
          uri: monaco.Uri.parse(definition.uri),
          range: {
            startLineNumber: definition.range.start.line + 1,
            startColumn: definition.range.start.character + 1,
            endLineNumber: definition.range.end.line + 1,
            endColumn: definition.range.end.character + 1,
          },
        }));
      },
    });
  }
}
