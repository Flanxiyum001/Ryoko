/**
 * Deterministic 10,000-path workspace fixture for the quick-open benchmark.
 *
 * The same seed produces byte-identical paths on every machine and Node
 * version, so a recorded measurement and a fresh one describe the same work.
 * Nothing here touches the filesystem: the fixture is the path list the desktop
 * already loads once per workspace.
 */

/** Mulberry32: small, fast, deterministic. Never used for anything security related. */
function seeded(seed: number): () => number {
  let state = seed >>> 0;
  return () => {
    state = (state + 0x6d2b79f5) >>> 0;
    let t = state;
    t = Math.imul(t ^ (t >>> 15), t | 1);
    t ^= t + Math.imul(t ^ (t >>> 7), t | 61);
    return ((t ^ (t >>> 14)) >>> 0) / 4_294_967_296;
  };
}

const TOPS = [
  'src', 'crates', 'apps', 'docs', 'tests', 'scripts', 'packages', 'assets',
  'services', 'tools', 'examples', 'configs', 'ui', 'api', 'infra', 'fixtures',
];
const MIDDLES = [
  'components', 'features', 'models', 'runtime', 'transport', 'storage', 'editor',
  'palette', 'review', 'mission', 'sessions', 'settings', 'protocol', 'search',
  'bench', 'shared', 'internal', 'native', 'sandbox', 'worktree', 'cli', 'core',
];
const STEMS = [
  'index', 'main', 'client', 'server', 'config', 'parser', 'router', 'session',
  'workspace', 'terminal', 'debug', 'quickOpen', 'commandPalette', 'fileTree',
  'diffView', 'hunkReview', 'agentRun', 'modelRuntime', 'gitStatus', 'settings',
  'shortcut', 'keybinding', 'marketplace', 'connector', 'lspClient', 'telemetry',
  'audit', 'release', 'pipeline', 'credential', 'checkpoint', 'snapshot', 'queue',
];
const SUFFIXES = ['', '', '', '-view', '-panel', '-client', '-model', '-test', '-spec', '.stories'];
const EXTENSIONS = ['.ts', '.tsx', '.ts', '.ts', '.rs', '.md', '.json', '.css'];

/** Exactly 10,000 unique relative paths, nested up to five segments deep. */
export function buildWorkspacePaths(count = 10_000, seed = 0x51a7): string[] {
  const random = seeded(seed);
  const paths = new Set<string>();
  while (paths.size < count) {
    const top = TOPS[Math.floor(random() * TOPS.length)];
    const depth = 1 + Math.floor(random() * 3);
    const segments = [top];
    for (let level = 0; level < depth; level += 1) {
      segments.push(MIDDLES[Math.floor(random() * MIDDLES.length)]);
    }
    const stem = STEMS[Math.floor(random() * STEMS.length)];
    const suffix = SUFFIXES[Math.floor(random() * SUFFIXES.length)];
    const extension = EXTENSIONS[Math.floor(random() * EXTENSIONS.length)];
    const filename = stem + suffix + extension;
    const directory = segments.join('/');
    // Collisions are resolved with a numeric discriminator so the fixture is
    // always exactly `count` unique paths.
    const candidate = directory + '/' + filename;
    paths.add(paths.has(candidate) ? candidate + '.' + String(paths.size) : candidate);
  }
  return [...paths];
}

/** Queries chosen to cover every ranking path the issue names. */
export const BENCH_QUERIES: readonly { query: string; label: string }[] = [
  { query: '', label: 'empty (recent files)' },
  { query: 'e', label: 'single letter (broad match)' },
  { query: 'ma', label: 'two letters' },
  { query: 'main', label: 'exact basename' },
  { query: 'index', label: 'common basename' },
  { query: 'qopen', label: 'fuzzy subsequence' },
  { query: 'rct', label: 'fuzzy initials' },
  { query: 'src/models', label: 'path segments' },
  { query: 'quickOpen', label: 'camelCase basename' },
  { query: 'zzznomatch', label: 'no results' },
];

/** Recents that the empty query ranks first, as the palette does on open. */
export function recentPaths(paths: readonly string[]): string[] {
  return [paths[7], paths[1_234], paths[8_888]].filter(
    (path): path is string => typeof path === 'string' && path.length > 0,
  );
}
