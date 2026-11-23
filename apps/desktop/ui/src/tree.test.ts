import { describe, expect, it } from 'vitest';
import {
  buildTree,
  changeKind,
  changesByPath,
  filterTreePaths,
} from './tree';

describe('changeKind', () => {
  it('classifies porcelain statuses into badge kinds', () => {
    expect(changeKind('??')).toBe('added');
    expect(changeKind('A ')).toBe('added');
    expect(changeKind('M ')).toBe('modified');
    expect(changeKind(' M')).toBe('modified');
    expect(changeKind('D ')).toBe('deleted');
    expect(changeKind('R ')).toBe('renamed');
    expect(changeKind('C ')).toBe('other');
  });
});

describe('changesByPath', () => {
  it('keeps the last kind per path', () => {
    const map = changesByPath([
      { path: 'a.rs', kind: 'modified' },
      { path: 'b.rs', kind: 'added' },
      { path: 'a.rs', kind: 'deleted' },
    ]);
    expect(map.get('a.rs')).toBe('deleted');
    expect(map.get('b.rs')).toBe('added');
  });
});

describe('buildTree', () => {
  const paths = ['src/main.ts', 'src/lsp/client.ts', 'README.md'];

  it('emits collapsible directories first, then files', () => {
    const nodes = buildTree(paths, { expanded: new Set(['src', 'src/lsp']) });
    expect(nodes.map((node) => `${node.type}:${node.path}`)).toEqual([
      'dir:src',
      'dir:src/lsp',
      'file:src/lsp/client.ts',
      'file:src/main.ts',
      'file:README.md',
    ]);
    expect(nodes[0].depth).toBe(0);
    expect(nodes[1].depth).toBe(1);
  });

  it('collapses unexpanded directories to a single row', () => {
    const nodes = buildTree(paths, { expanded: new Set() });
    expect(nodes.filter((node) => node.type === 'dir')).toHaveLength(1);
    expect(nodes).toHaveLength(2);
  });

  it('decorates files with change kinds', () => {
    const nodes = buildTree(paths, {
      expanded: new Set(['src', 'src/lsp']),
      changes: changesByPath([{ path: 'README.md', kind: 'modified' }]),
    });
    const readme = nodes.find((node) => node.path === 'README.md');
    expect(readme?.change).toBe('modified');
    const main = nodes.find((node) => node.path === 'src/main.ts');
    expect(main?.change).toBeUndefined();
  });
});

describe('filterTreePaths', () => {
  it('keeps matching files and drops non-matching ones', () => {
    const visible = filterTreePaths(['src/main.ts', 'src/lsp/client.ts', 'README.md'], 'client');
    expect(visible).toEqual(['src/lsp/client.ts']);
  });

  it('keeps directory-like entries that match directly', () => {
    const visible = filterTreePaths(['src', 'src/main.ts'], 'src');
    expect(visible).toEqual(['src', 'src/main.ts']);
  });

  it('returns everything for an empty query', () => {
    const paths = ['a', 'b/c'];
    expect(filterTreePaths(paths, '')).toEqual(paths);
  });
});
