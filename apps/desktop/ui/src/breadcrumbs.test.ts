import { describe, expect, it } from 'vitest';
import { breadcrumbItems, fromLspDocumentSymbols, pathCrumbs, symbolChain } from './breadcrumbs';

const range = (sl: number, sc: number, el: number, ec: number) => ({ start: { line: sl, character: sc }, end: { line: el, character: ec } });

describe('breadcrumbs', () => {
  it('splits paths into crumbs', () => {
    expect(pathCrumbs('src/ui/main.ts')).toEqual(['src', 'ui', 'main.ts']);
  });
  it('finds the enclosing symbol chain in hierarchical results', () => {
    const symbols = fromLspDocumentSymbols([
      { name: 'Workbench', kind: 5, range: range(0, 0, 10, 1), children: [
        { name: 'activateView', kind: 6, range: range(2, 2, 5, 3), children: [] },
      ] },
      { name: 'helper', kind: 12, range: range(12, 0, 14, 1) },
    ]);
    expect(symbolChain(symbols, 4, 5).map((symbol) => symbol.name)).toEqual(['Workbench', 'activateView']);
    expect(symbolChain(symbols, 14, 1).map((symbol) => symbol.name)).toEqual(['helper']);
    expect(symbolChain(symbols, 12, 1)).toEqual([]);
  });
  it('nests flat SymbolInformation results by range', () => {
    const symbols = fromLspDocumentSymbols([
      { name: 'Outer', kind: 5, location: { uri: 'file:///a', range: range(0, 0, 20, 0) } },
      { name: 'inner', kind: 6, location: { uri: 'file:///a', range: range(3, 0, 6, 0) } },
    ]);
    expect(symbols).toHaveLength(1);
    expect(symbolChain(symbols, 5, 1).map((symbol) => symbol.name)).toEqual(['Outer', 'inner']);
  });
  it('drops malformed entries and strips control characters', () => {
    const symbols = fromLspDocumentSymbols([
      null, 'x', { name: 42, range: range(0, 0, 1, 0) }, { name: 'ok', kind: 'evil' },
      { name: 'a\u0000b\u001b[31m', kind: 12, range: range(0, 0, 1, 0) },
    ]);
    expect(symbols.map((symbol) => symbol.name)).toEqual(['a b [31m']);
    expect(fromLspDocumentSymbols({ not: 'an array' })).toEqual([]);
  });
  it('bounds very large results', () => {
    const many = Array.from({ length: 6000 }, (_, index) => ({ name: `s${index}`, kind: 13, range: range(index, 0, index, 1) }));
    expect(fromLspDocumentSymbols(many).length).toBe(5000);
  });
  it('builds folder, file and symbol items', () => {
    const [symbol] = fromLspDocumentSymbols([{ name: 'run', kind: 12, range: range(1, 0, 3, 0), children: [] }]);
    expect(breadcrumbItems('src/main.ts', [symbol])).toEqual([
      { label: 'src', kind: 'folder' },
      { label: 'main.ts', kind: 'file' },
      { label: 'run', kind: 'symbol', line: 2, column: 1 },
    ]);
  });
});
