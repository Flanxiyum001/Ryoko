/** Editor-only language metadata; never grants permission to execute files. */
const extensions: Record<string, string> = {
  abap:'abap', ada:'ada', adb:'ada', adoc:'asciidoc', ads:'ada',
  asciidoc:'asciidoc', asm:'asm', apex:'apex', astro:'html', awk:'awk',
  bat:'bat', bash:'shell', bazel:'python', bib:'latex', bicep:'bicep',
  bzl:'python', c:'c', cabal:'haskell', cbl:'cobol', cc:'cpp', cfg:'ini',
  cjs:'javascript', cl:'lisp', clj:'clojure', cljs:'clojure', cls:'latex',
  cmake:'cmake', cmd:'bat', cnf:'ini', cob:'cobol', coffee:'coffeescript',
  comp:'glsl', conf:'ini', cpp:'cpp', cppm:'cpp', cpy:'cobol', cql:'cypher',
  cs:'csharp', csh:'shell', cshtml:'razor', csproj:'xml', css:'css',
  csx:'csharp', cts:'typescript', cu:'cpp', cue:'cue', cuh:'cpp',
  cxx:'cpp', cypher:'cypher', d:'d', dart:'dart', desktop:'ini',
  dhall:'haskell', diff:'diff', dockerfile:'dockerfile', dpr:'pascal',
  edn:'clojure', el:'lisp', elm:'elm', env:'ini', erb:'html', erl:'erlang',
  ex:'elixir', exs:'elixir', f:'fortran', f03:'fortran', f08:'fortran',
  f77:'fortran', f90:'fortran', f95:'fortran', fish:'shell', for:'fortran',
  fpp:'fortran', frag:'glsl', fs:'fsharp', fsproj:'xml', fsx:'fsharp',
  ftl:'freemarker2', fx:'cpp', geojson:'json', geom:'glsl', gleam:'gleam',
  glsl:'glsl', go:'go', gql:'graphql', gradle:'groovy', graphql:'graphql',
  groovy:'groovy', h:'cpp', hbs:'handlebars', hcl:'hcl', hh:'cpp',
  hlsl:'cpp', hpp:'cpp', hrl:'erlang', hs:'haskell', htm:'html', html:'html',
  hxx:'cpp', inf:'ini', ini:'ini', inl:'cpp', ipynb:'json', ixx:'cpp',
  jade:'pug', java:'java', jl:'julia', js:'javascript', json:'json',
  json5:'json', jsonc:'json', jsonl:'json', jsx:'javascript', kdl:'kdl',
  ksh:'shell', kt:'kotlin', kts:'kotlin', less:'less', lhs:'haskell',
  liquid:'liquid', lisp:'lisp', lsp:'lisp', lua:'lua', luau:'lua',
  m:'objective-c', mak:'makefile', make:'makefile', markdown:'markdown',
  md:'markdown', mdx:'markdown', metal:'cpp', mjs:'javascript',
  mk:'makefile', ml:'ocaml', mli:'ocaml', mm:'objective-c', mmd:'mermaid',
  mojo:'mojo', move:'move', mts:'typescript', nasm:'asm', ndjson:'json',
  nim:'nim', nix:'nix', nu:'shell', nuspec:'xml', odin:'odin', org:'org',
  pas:'pascal', patch:'diff', php:'php', pl:'perl', plist:'xml', pm:'perl',
  pom:'xml', pp:'pascal', prisma:'prisma', properties:'ini', props:'xml',
  proto:'protobuf', ps1:'powershell', psd1:'powershell', psm1:'powershell',
  psql:'sql', pug:'pug', py:'python', pyi:'python', pyw:'python',
  qs:'qsharp', r:'r', razor:'razor', rb:'ruby', reg:'ini', rkt:'racket',
  rmd:'markdown', rq:'sparql', rs:'rust', rss:'xml', rst:'restructuredtext',
  s:'asm', sass:'scss', sbt:'scala', sc:'scala', scala:'scala', scm:'scheme',
  scss:'scss', service:'ini', sh:'shell', sol:'solidity', sparql:'sparql',
  sql:'sql', star:'python', sty:'latex', sv:'systemverilog', svelte:'svelte',
  svh:'systemverilog', svg:'xml', swift:'swift', targets:'xml', tcl:'tcl',
  tcsh:'shell', tesc:'glsl', tese:'glsl', tex:'latex', tf:'hcl',
  tfvars:'hcl', toml:'ini', tpp:'cpp', ts:'typescript', tsp:'typespec',
  tsx:'typescript', twig:'twig', txt:'plaintext', typ:'typst', v:'verilog',
  vb:'vb', vbproj:'xml', vbs:'vb', vcxproj:'xml', vert:'glsl', vhd:'vhdl',
  vhdl:'vhdl', vim:'vim', vue:'html', vy:'vyper', wat:'wasm',
  webmanifest:'json', wgsl:'wgsl', xaml:'xml', xhtml:'xml', xml:'xml',
  xsd:'xml', xsl:'xml', xslt:'xml', yaml:'yaml', yml:'yaml', zig:'zig',
  zsh:'shell',
};
const filenames: Record<string, string> = {
  '.babelrc':'json', '.bash_profile':'shell', '.bashrc':'shell',
  '.dockerignore':'ini', '.editorconfig':'ini', '.env':'ini',
  '.eslintignore':'ini', '.eslintrc':'json', '.gitattributes':'ini',
  '.gitconfig':'ini', '.gitignore':'ini', '.htaccess':'ini', '.npmrc':'ini',
  '.prettierignore':'ini', '.prettierrc':'json', '.profile':'shell',
  '.pypirc':'ini', '.vimrc':'vim', '.yarnrc':'ini', '.zlogin':'shell',
  '.zprofile':'shell', '.zshrc':'shell', 'brewfile':'ruby', 'build':'python',
  'cargo.lock':'ini', 'cmakelists.txt':'cmake', 'containerfile':'dockerfile',
  'dockerfile':'dockerfile', 'fastfile':'ruby', 'flake.lock':'json',
  'gemfile':'ruby', 'gnumakefile':'makefile', 'jenkinsfile':'groovy',
  'justfile':'makefile', 'makefile':'makefile', 'meson.build':'python',
  'pipfile':'ini', 'podfile':'ruby', 'poetry.lock':'ini', 'procfile':'shell',
  'rakefile':'ruby', 'readme':'markdown', 'uv.lock':'ini',
  'vagrantfile':'ruby', 'workspace':'python',
};
const shebangs: ReadonlyArray<[RegExp, string]> = [
  [/\bpython(?:\d+(?:\.\d+)?)?\b/, 'python'],
  [/\b(?:node|bun|deno)\b/, 'javascript'],
  [/\b(?:bash|zsh|fish|ksh|dash|ash|sh)\b/, 'shell'],
  [/\bruby\b/, 'ruby'], [/\bperl\b/, 'perl'], [/\bphp\b/, 'php'],
  [/\blua\b/, 'lua'], [/\b(?:pwsh|powershell)\b/, 'powershell'],
  [/\bRscript\b/, 'r'], [/\bjulia\b/, 'julia'], [/\bawk\b/, 'awk'],
  [/\b(?:tclsh|wish)\b/, 'tcl'], [/\bescript\b/, 'erlang'],
  [/\belixir\b/, 'elixir'], [/\bswift\b/, 'swift'],
];

export function detectLanguage(path: string, contents = ''): string {
  const filename = path.split(/[\\/]/).pop()?.toLowerCase() ?? '';
  if (Object.hasOwn(filenames, filename)) return filenames[filename];
  if (filename.startsWith('dockerfile.') || filename.startsWith('containerfile.')) return 'dockerfile';
  if (filename.startsWith('.env.')) return 'ini';
  const ext = filename.includes('.') ? filename.split('.').pop() ?? '' : '';
  if (Object.hasOwn(extensions, ext)) return extensions[ext];
  const firstLine = contents.slice(0, 256).replace(/^\uFEFF/, '').split(/\r?\n/, 1)[0];
  if (firstLine.startsWith('#!')) {
    for (const [pattern, language] of shebangs) {
      if (pattern.test(firstLine)) return language;
    }
  }
  return 'plaintext';
}

/** Monaco's installed language metadata extends detection without a hardcoded whitelist. */
export interface EditorLanguageMetadata {
  id: string;
  extensions?: readonly string[];
  filenames?: readonly string[];
}

export function detectEditorLanguage(
  path: string, contents: string, registered: readonly EditorLanguageMetadata[],
): string {
  const detected = detectLanguage(path, contents);
  if (detected !== 'plaintext') return detected;
  const name = path.split(/[\\/]/).pop()?.toLowerCase() ?? '';
  if (name.endsWith('.txt')) return 'plaintext';
  const exact = registered.find((mode) =>
    mode.filenames?.some((filename) => filename.toLowerCase() === name));
  if (exact) return exact.id;
  let best: { id: string; length: number } | undefined;
  for (const mode of registered) {
    for (const extension of mode.extensions ?? []) {
      if (name.endsWith(extension.toLowerCase()) && extension.length > (best?.length ?? 0)) {
        best = { id: mode.id, length: extension.length };
      }
    }
  }
  if (best) return best.id;
  // Only strong content signals; never execute code or install a language server.
  const start = contents.trimStart().slice(0, 512);
  if (/^<\?xml\b/i.test(start)) return 'xml';
  if (/^<!doctype\s+html\b/i.test(start)) return 'html';
  if (/^<\?php\b/i.test(start)) return 'php';
  if (/^(?:\{|\[)/.test(start) && contents.length <= 2 * 1024 * 1024) {
    try { JSON.parse(contents.replace(/^\uFEFF/, '')); return 'json'; } catch { /* ambiguous text */ }
  }
  return 'plaintext';
}

/** Supported detection modes; arbitrary Monaco/LSP modes may also be selected. */
export const LANGUAGE_IDS = Object.freeze(
  [...new Set([...Object.values(extensions), ...Object.values(filenames), 'plaintext'])].sort(),
);
export interface RunProfile { label: string; program: string; args: string[]; }

/** Explicit argv; no shell evaluation, implicit installer or background run. */
export function defaultRunProfile(path: string): RunProfile | null {
  const filename = path.split(/[\\/]/).pop()?.toLowerCase() ?? '';
  if (filename.endsWith('.tsx') || filename.endsWith('.jsx')) return null;
  if (filename.endsWith('.ts') || filename.endsWith('.mts') || filename.endsWith('.cts')) {
    return { label: 'Node.js 22+ (type stripping)', program:'node',
      args:['--experimental-strip-types', '{file}'] };
  }
  if (filename.endsWith('.kts')) {
    return { label:'Kotlin script', program:'kotlinc', args:['-script', '{file}'] };
  }
  const recipes: Record<string, RunProfile> = {
    python:{label:'Python',program:'python3',args:['-u','{file}']},
    javascript:{label:'Node.js',program:'node',args:['{file}']},
    go:{label:'Go',program:'go',args:['run','{file}']},
    java:{label:'Java (JDK 11+)',program:'java',args:['{file}']},
    ruby:{label:'Ruby',program:'ruby',args:['{file}']},
    php:{label:'PHP',program:'php',args:['{file}']},
    perl:{label:'Perl',program:'perl',args:['{file}']},
    lua:{label:'Lua',program:'lua',args:['{file}']},
    r:{label:'R',program:'Rscript',args:['{file}']},
    julia:{label:'Julia',program:'julia',args:['{file}']},
    dart:{label:'Dart',program:'dart',args:['run','{file}']},
    swift:{label:'Swift',program:'swift',args:['{file}']},
    elixir:{label:'Elixir',program:'elixir',args:['{file}']},
    powershell:{label:'PowerShell',program:'pwsh',args:['-File','{file}']},
    shell:{label:'POSIX shell',program:'sh',args:['{file}']},
    clojure:{label:'Clojure',program:'clojure',args:['{file}']},
    haskell:{label:'Haskell',program:'runghc',args:['{file}']},
    groovy:{label:'Groovy',program:'groovy',args:['{file}']},
    ocaml:{label:'OCaml',program:'ocaml',args:['{file}']},
    scala:{label:'Scala',program:'scala',args:['{file}']},
    rust:{label:'Cargo project',program:'cargo',args:['run','--locked']},
  };
  return recipes[detectLanguage(path)] ?? null;
}

/** Quote-aware argv parser; keeps tokens literal with no shell expansion. */
export function parseTaskArgs(input: string): string[] {
  const args: string[] = [];
  let token = '';
  let quote: '"' | "'" | null = null;
  let started = false;
  for (let i = 0; i < input.length; i++) {
    const char = input[i];
    const next = input[i+1];
    if (char === '\\' && next && [' ', '"', "'", '\\'].includes(next)) {
      token += next; started = true; i++;
    } else if (quote && char === quote) {
      quote = null;
    } else if (!quote && (char === '"' || char === "'")) {
      quote = char; started = true;
    } else if (!quote && /\s/.test(char)) {
      if (started) args.push(token);
      token = ''; started = false;
    } else {
      token += char; started = true;
    }
  }
  if (quote) throw new Error('Close the quoted argument before running.');
  if (started) args.push(token);
  if (args.length > 32) throw new Error('A task may have at most 32 arguments.');
  return args;
}
