/** Editor-only language metadata; never grants permission to execute files. */
const extensions: Record<string, string> = {
  ada:'ada', adb:'ada', ads:'ada', asm:'asm',
  astro:'html', bat:'bat', bash:'shell', c:'c', cc:'cpp',
  cjs:'javascript', clj:'clojure', cljs:'clojure', cmake:'cmake',
  cob:'cobol', coffee:'coffeescript', cpp:'cpp', cxx:'cpp',
  cs:'csharp', csx:'csharp', css:'css', dart:'dart', diff:'diff',
  ex:'elixir', exs:'elixir', elm:'elm', erb:'html',
  fs:'fsharp', fsx:'fsharp', fish:'shell', gleam:'gleam',
  glsl:'glsl', go:'go', gql:'graphql', graphql:'graphql',
  gradle:'groovy', groovy:'groovy', h:'cpp', hh:'cpp', hpp:'cpp',
  hs:'haskell', html:'html', htm:'html', ini:'ini', ipynb:'json',
  java:'java', jl:'julia', js:'javascript', jsx:'javascript',
  json:'json', json5:'json', jsonc:'json', kt:'kotlin', kts:'kotlin',
  less:'less', lhs:'haskell', lua:'lua', m:'objective-c', mm:'objective-c',
  markdown:'markdown', md:'markdown', mdx:'markdown', mjs:'javascript',
  ml:'ocaml', mli:'ocaml', nim:'nim', nix:'nix', php:'php',
  pl:'perl', pm:'perl', prisma:'prisma', proto:'protobuf',
  ps1:'powershell', psm1:'powershell', py:'python', pyi:'python', pyw:'python',
  r:'r', rb:'ruby', rkt:'racket', rmd:'markdown', rs:'rust',
  sass:'scss', sbt:'scala', sc:'scala', scala:'scala', scm:'scheme',
  scss:'scss', sh:'shell', sol:'solidity', sql:'sql', swift:'swift',
  tex:'latex', tf:'hcl', tfvars:'hcl', toml:'ini', ts:'typescript',
  tsx:'typescript', twig:'twig', txt:'plaintext',
  v:'verilog', vhd:'vhdl', vhdl:'vhdl', vim:'vim', vue:'html',
  xml:'xml', xsl:'xml', yaml:'yaml', yml:'yaml', zig:'zig', zsh:'shell',
};
const filenames: Record<string, string> = {
  '.bash_profile':'shell', '.bashrc':'shell', '.editorconfig':'ini',
  '.gitattributes':'ini', '.gitignore':'ini', '.npmrc':'ini', '.zshrc':'shell',
  'cmakelists.txt':'cmake', 'containerfile':'dockerfile',
  'dockerfile':'dockerfile', 'gemfile':'ruby', 'jenkinsfile':'groovy',
  'justfile':'makefile', 'makefile':'makefile', 'rakefile':'ruby',
  'readme':'markdown', 'procfile':'shell',
};
const shebangs: ReadonlyArray<[RegExp, string]> = [
  [/\bpython(?:\d+(?:\.\d+)?)?\b/, 'python'],
  [/\b(?:node|bun|deno)\b/, 'javascript'],
  [/\b(?:bash|zsh|fish|sh)\b/, 'shell'],
  [/\bruby\b/, 'ruby'], [/\bperl\b/, 'perl'], [/\bphp\b/, 'php'],
  [/\blua\b/, 'lua'], [/\b(?:pwsh|powershell)\b/, 'powershell'],
  [/\bRscript\b/, 'r'], [/\bjulia\b/, 'julia'],
];

export function detectLanguage(path: string, contents = ''): string {
  const filename = path.split(/[\\/]/).pop()?.toLowerCase() ?? '';
  if (Object.hasOwn(filenames, filename)) return filenames[filename];
  if (filename.startsWith('dockerfile.')) return 'dockerfile';
  const ext = filename.includes('.') ? filename.split('.').pop() ?? '' : '';
  if (Object.hasOwn(extensions, ext)) return extensions[ext];
  const firstLine = contents.slice(0, 256).split(/\r?\n/, 1)[0];
  if (firstLine.startsWith('#!')) {
    for (const [pattern, language] of shebangs) {
      if (pattern.test(firstLine)) return language;
    }
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
