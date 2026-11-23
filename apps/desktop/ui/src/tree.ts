//! Pure logic behind the file explorer: a collapsible tree built from the
//! flat, directory-anchored path list the Rust shell returns, decorated with
//! Git working-tree badges.

export type ChangeKind = 'added' | 'modified' | 'deleted' | 'renamed' | 'other';

export interface TreeNode {
  name: string;
  /** Workspace-relative path (directories keep their trailing-less form). */
  path: string;
  type: 'dir' | 'file';
  depth: number;
  change?: ChangeKind;
}

export interface ChangeInfo {
  path: string;
  kind: ChangeKind;
}

/** Maps a raw `git status --porcelain` XY code (already parsed upstream). */
export function changeKind(status: string): ChangeKind {
  if (status.includes('A') || status === '??') return 'added';
  if (status.includes('D')) return 'deleted';
  if (status.includes('R')) return 'renamed';
  if (status.includes('M') || status.includes('T')) return 'modified';
  return 'other';
}

export function changeLabel(kind: ChangeKind): string {
  return {
    added: 'A',
    modified: 'M',
    deleted: 'D',
    renamed: 'R',
    other: '~',
  }[kind];
}

export function changesByPath(entries: ChangeInfo[]): Map<string, ChangeKind> {
  const map = new Map<string, ChangeKind>();
  for (const entry of entries) map.set(entry.path, entry.kind);
  return map;
}

/**
 * Builds an ordered, depth-first tree. Directories sort first and then files,
 * each alphabetically. Expanding is decided by the caller; collapsed
 * directories emit exactly one placeholder node.
 */
export function buildTree(
  paths: readonly string[],
  options: { expanded: ReadonlySet<string>; changes?: ReadonlyMap<string, ChangeKind> },
): TreeNode[] {
  interface Dir {
    dirs: Map<string, Dir>;
    files: string[];
  }
  const rootDir: Dir = { dirs: new Map(), files: [] };
  for (const path of paths) {
    const segments = path.split('/').filter((segment) => segment.length > 0);
    let cursor = rootDir;
    for (let index = 0; index < segments.length; index += 1) {
      const last = index === segments.length - 1;
      const segment = segments[index];
      if (last) {
        cursor.files.push(segment);
      } else {
        let next = cursor.dirs.get(segment);
        if (!next) {
          next = { dirs: new Map(), files: [] };
          cursor.dirs.set(segment, next);
        }
        cursor = next;
      }
    }
  }

  const nodes: TreeNode[] = [];
  const walk = (dir: Dir, prefix: string, depth: number): void => {
    const dirEntries = [...dir.dirs.keys()].sort((a, b) => a.localeCompare(b));
    for (const name of dirEntries) {
      const path = prefix ? `${prefix}/${name}` : name;
      const subtree = dir.dirs.get(name)!;
      nodes.push({ name, path, type: 'dir', depth });
      if (options.expanded.has(path)) {
        walk(subtree, path, depth + 1);
      }
    }
    for (const name of dir.files.sort((a, b) => a.localeCompare(b))) {
      const path = prefix ? `${prefix}/${name}` : name;
      nodes.push({ name, path, type: 'file', depth, change: options.changes?.get(path) });
    }
  };
  walk(rootDir, '', 0);
  return nodes;
}

/** Rows a filter query leaves visible: files by substring, dirs as ancestors. */
export function filterTreePaths(paths: readonly string[], query: string): string[] {
  const needle = query.trim().toLowerCase();
  if (!needle) return [...paths];
  const keep = new Set<string>();
  for (const path of paths) {
    if (path.toLowerCase().includes(needle)) {
      keep.add(path);
      const segments = path.split('/');
      for (let index = 1; index < segments.length; index += 1) {
        keep.add(segments.slice(0, index).join('/'));
      }
    }
  }
  return paths.filter((path) => keep.has(path));
}
