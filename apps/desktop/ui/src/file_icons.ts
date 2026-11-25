//! File-type icons for the explorer, tabs and breadcrumbs. Each icon is a
//! short text badge with a colour, or a local SVG for unknown file types.

import { workbenchIcon } from './workbench_icons';

export interface FileIcon {
  /** One to three characters drawn inside the badge. */
  glyph: string;
  /** CSS colour for the glyph. */
  color: string;
  /** Human-readable type, used as the badge's tooltip. */
  label: string;
}

const BY_NAME: Record<string, FileIcon> = {
  'cargo.toml': { glyph: 'rs', color: '#dea584', label: 'Cargo manifest' },
  'cargo.lock': { glyph: 'rs', color: '#9a7b62', label: 'Cargo lockfile' },
  'package.json': { glyph: '{}', color: '#8bc34a', label: 'npm package' },
  'package-lock.json': { glyph: '{}', color: '#6b8f3a', label: 'npm lockfile' },
  'dockerfile': { glyph: 'dk', color: '#2496ed', label: 'Dockerfile' },
  'makefile': { glyph: 'mk', color: '#e37933', label: 'Makefile' },
  '.gitignore': { glyph: 'git', color: '#f14e32', label: 'Git ignore' },
  'license': { glyph: '§', color: '#d4b106', label: 'License' },
  'readme.md': { glyph: 'i', color: '#42a5f5', label: 'Readme' },
};

const BY_EXTENSION: Record<string, FileIcon> = {
  ts: { glyph: 'TS', color: '#3178c6', label: 'TypeScript' },
  tsx: { glyph: 'TSX', color: '#3178c6', label: 'TypeScript React' },
  js: { glyph: 'JS', color: '#f1dd35', label: 'JavaScript' },
  mjs: { glyph: 'JS', color: '#f1dd35', label: 'JavaScript module' },
  cjs: { glyph: 'JS', color: '#f1dd35', label: 'CommonJS' },
  jsx: { glyph: 'JSX', color: '#61dafb', label: 'JavaScript React' },
  rs: { glyph: 'rs', color: '#dea584', label: 'Rust' },
  py: { glyph: 'py', color: '#4b8bbe', label: 'Python' },
  go: { glyph: 'go', color: '#00add8', label: 'Go' },
  java: { glyph: 'J', color: '#e76f00', label: 'Java' },
  kt: { glyph: 'kt', color: '#a97bff', label: 'Kotlin' },
  c: { glyph: 'C', color: '#649ad2', label: 'C' },
  h: { glyph: 'h', color: '#a074c4', label: 'C header' },
  cpp: { glyph: 'C++', color: '#f34b7d', label: 'C++' },
  hpp: { glyph: 'h', color: '#f34b7d', label: 'C++ header' },
  cs: { glyph: 'C#', color: '#9b4f96', label: 'C#' },
  rb: { glyph: 'rb', color: '#cc342d', label: 'Ruby' },
  php: { glyph: 'php', color: '#8892bf', label: 'PHP' },
  swift: { glyph: 'sw', color: '#f05138', label: 'Swift' },
  html: { glyph: '<>', color: '#e44d26', label: 'HTML' },
  css: { glyph: '#', color: '#42a5f5', label: 'CSS' },
  scss: { glyph: '#', color: '#cd6799', label: 'SCSS' },
  json: { glyph: '{}', color: '#cbcb41', label: 'JSON' },
  jsonc: { glyph: '{}', color: '#cbcb41', label: 'JSON with comments' },
  toml: { glyph: 'tm', color: '#9c4221', label: 'TOML' },
  yaml: { glyph: 'yml', color: '#cb171e', label: 'YAML' },
  yml: { glyph: 'yml', color: '#cb171e', label: 'YAML' },
  md: { glyph: 'M↓', color: '#519aba', label: 'Markdown' },
  txt: { glyph: '≡', color: '#a0a0a0', label: 'Text' },
  sh: { glyph: '$', color: '#89e051', label: 'Shell script' },
  ps1: { glyph: '>_', color: '#5391fe', label: 'PowerShell' },
  sql: { glyph: 'db', color: '#e38c00', label: 'SQL' },
  svg: { glyph: 'svg', color: '#ffb13b', label: 'SVG image' },
  png: { glyph: 'img', color: '#a074c4', label: 'Image' },
  jpg: { glyph: 'img', color: '#a074c4', label: 'Image' },
  jpeg: { glyph: 'img', color: '#a074c4', label: 'Image' },
  gif: { glyph: 'img', color: '#a074c4', label: 'Image' },
  lock: { glyph: '🔒', color: '#9a9a9a', label: 'Lockfile' },
  wasm: { glyph: 'wa', color: '#654ff0', label: 'WebAssembly' },
};

export const DEFAULT_FILE_ICON: FileIcon = { glyph: '·', color: '#9a9a9a', label: 'File' };

export function iconForPath(path: string): FileIcon {
  const name = (path.split('/').pop() ?? path).toLowerCase();
  if (BY_NAME[name]) return BY_NAME[name];
  const dot = name.lastIndexOf('.');
  if (dot > 0 && dot < name.length - 1) {
    const icon = BY_EXTENSION[name.slice(dot + 1)];
    if (icon) return icon;
  }
  return DEFAULT_FILE_ICON;
}

/** Builds the badge element from static icons; file names never become markup. */
export function fileIconElement(path: string): HTMLSpanElement {
  const icon = iconForPath(path);
  const badge = document.createElement('span');
  badge.className = 'file-icon';
  if (icon === DEFAULT_FILE_ICON) badge.append(workbenchIcon('file', 'control-icon'));
  else badge.textContent = icon.glyph;
  badge.style.color = icon.color;
  badge.dataset.length = String([...icon.glyph].length);
  badge.setAttribute('aria-hidden', 'true');
  return badge;
}
