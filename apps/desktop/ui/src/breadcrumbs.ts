//! Breadcrumbs above the editor: the active file's folders, the file itself,
//! then the chain of symbols enclosing the cursor (for example
//! `src › ui › main.ts › activateView`). Symbol data comes from the file's
//! language server (`textDocument/documentSymbol`) and is untrusted input:
//! it is shape-checked and bounded before rendering, and only ever shown as text.

export interface SymbolRange {
  startLine: number; // 1-based, inclusive
  startColumn: number;
  endLine: number;
  endColumn: number;
}

export interface OutlineSymbol {
  name: string;
  kind: string;
  range: SymbolRange;
  children: OutlineSymbol[];
}

const MAX_SYMBOLS = 5000;
const MAX_DEPTH = 32;
const MAX_NAME = 200;

/** LSP SymbolKind numbers to short labels. */
const LSP_KINDS: Record<number, string> = {
  2: 'module', 3: 'namespace', 4: 'package', 5: 'class', 6: 'method', 7: 'property',
  8: 'field', 9: 'constructor', 10: 'enum', 11: 'interface', 12: 'function',
  13: 'variable', 14: 'constant', 22: 'enum-member', 23: 'struct', 26: 'type-parameter',
};

export function pathCrumbs(path: string): string[] {
  return path.split('/').filter((segment) => segment.length > 0);
}

function contains(range: SymbolRange, line: number, column: number): boolean {
  if (line < range.startLine || line > range.endLine) return false;
  if (line === range.startLine && column < range.startColumn) return false;
  if (line === range.endLine && column > range.endColumn) return false;
  return true;
}

/** Innermost-last chain of symbols that enclose the position. */
export function symbolChain(symbols: readonly OutlineSymbol[], line: number, column: number): OutlineSymbol[] {
  const chain: OutlineSymbol[] = [];
  let level: readonly OutlineSymbol[] = symbols;
  for (let depth = 0; depth < MAX_DEPTH; depth += 1) {
    const hit = level.find((symbol) => contains(symbol.range, line, column));
    if (!hit) break;
    chain.push(hit);
    level = hit.children;
  }
  return chain;
}

function isLspRange(value: unknown): value is { start: { line: number; character: number }; end: { line: number; character: number } } {
  const range = value as { start?: { line?: unknown; character?: unknown }; end?: { line?: unknown; character?: unknown } } | null;
  return !!range && typeof range === 'object'
    && Number.isInteger(range.start?.line) && Number.isInteger(range.start?.character)
    && Number.isInteger(range.end?.line) && Number.isInteger(range.end?.character);
}

function toRange(range: { start: { line: number; character: number }; end: { line: number; character: number } }): SymbolRange {
  return {
    startLine: range.start.line + 1,
    startColumn: range.start.character + 1,
    endLine: range.end.line + 1,
    endColumn: range.end.character + 1,
  };
}

function cleanName(value: unknown): string | null {
  if (typeof value !== 'string') return null;
  const name = value.replace(/[\u0000-\u001f\u007f]/g, ' ').trim();
  return name ? name.slice(0, MAX_NAME) : null;
}

/**
 * Converts a `textDocument/documentSymbol` result. Accepts hierarchical
 * `DocumentSymbol[]` and flat `SymbolInformation[]` (flat results are nested
 * by range containment). Anything malformed is dropped.
 */
export function fromLspDocumentSymbols(result: unknown): OutlineSymbol[] {
  if (!Array.isArray(result)) return [];
  let budget = MAX_SYMBOLS;
  const convert = (items: unknown[], depth: number): OutlineSymbol[] => {
    const out: OutlineSymbol[] = [];
    for (const item of items) {
      if (budget <= 0 || depth > MAX_DEPTH) break;
      const symbol = item as { name?: unknown; kind?: unknown; range?: unknown; location?: { range?: unknown }; children?: unknown };
      const name = cleanName(symbol?.name);
      const range = isLspRange(symbol?.range) ? symbol.range : isLspRange(symbol?.location?.range) ? symbol.location!.range as never : null;
      if (!name || !range) continue;
      budget -= 1;
      out.push({
        name,
        kind: LSP_KINDS[Number(symbol.kind)] ?? 'symbol',
        range: toRange(range),
        children: Array.isArray(symbol.children) ? convert(symbol.children, depth + 1) : [],
      });
    }
    return out;
  };
  const flat = convert(result, 0);
  const hierarchical = (result as Array<{ children?: unknown }>).some((item) => Array.isArray(item?.children));
  return hierarchical ? flat : nestByRange(flat);
}

function rangeContains(outer: SymbolRange, inner: SymbolRange): boolean {
  const startsAfter = inner.startLine > outer.startLine
    || (inner.startLine === outer.startLine && inner.startColumn >= outer.startColumn);
  const endsBefore = inner.endLine < outer.endLine
    || (inner.endLine === outer.endLine && inner.endColumn <= outer.endColumn);
  return startsAfter && endsBefore;
}

/** Nests flat symbols by range containment (largest first). */
export function nestByRange(symbols: readonly OutlineSymbol[]): OutlineSymbol[] {
  const sorted = [...symbols].sort((a, b) =>
    a.range.startLine - b.range.startLine || a.range.startColumn - b.range.startColumn
    || b.range.endLine - a.range.endLine || b.range.endColumn - a.range.endColumn);
  const roots: OutlineSymbol[] = [];
  const stack: OutlineSymbol[] = [];
  for (const original of sorted) {
    const symbol = { ...original, children: [] as OutlineSymbol[] };
    while (stack.length && !rangeContains(stack[stack.length - 1].range, symbol.range)) stack.pop();
    if (stack.length < MAX_DEPTH) {
      (stack.length ? stack[stack.length - 1].children : roots).push(symbol);
      stack.push(symbol);
    }
  }
  return roots;
}

export interface BreadcrumbItem {
  label: string;
  kind: 'folder' | 'file' | 'symbol';
  /** For symbols: where to move the cursor when clicked. */
  line?: number;
  column?: number;
}

export function breadcrumbItems(path: string, chain: readonly OutlineSymbol[]): BreadcrumbItem[] {
  const segments = pathCrumbs(path);
  const items: BreadcrumbItem[] = segments.map((label, index) => ({
    label,
    kind: index === segments.length - 1 ? 'file' : 'folder',
  }));
  for (const symbol of chain) {
    items.push({ label: symbol.name, kind: 'symbol', line: symbol.range.startLine, column: symbol.range.startColumn });
  }
  return items;
}
