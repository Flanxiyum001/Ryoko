// Builds the ryoko-lsp sidecar for the host and copies it to
// src-tauri/binaries/ryoko-lsp-<target-triple>, the name Tauri's
// `externalBin` expects. CI does the same per release target in desktop.yml.
import { execFileSync } from 'node:child_process';
import { copyFileSync, mkdirSync } from 'node:fs';
import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';

const desktop = join(dirname(fileURLToPath(import.meta.url)), '..');
const workspace = join(desktop, '..', '..');
const host = execFileSync('rustc', ['-vV'], { encoding: 'utf8' })
  .split('\n')
  .find((line) => line.startsWith('host: '))
  ?.slice('host: '.length)
  .trim();
if (!host) throw new Error('could not determine the Rust host target triple');

execFileSync('cargo', ['build', '--locked', '-p', 'ryoko-native', '--bin', 'ryoko-lsp'], {
  cwd: workspace,
  stdio: 'inherit',
});
const exe = process.platform === 'win32' ? '.exe' : '';
const destination = join(desktop, 'src-tauri', 'binaries', `ryoko-lsp-${host}${exe}`);
mkdirSync(dirname(destination), { recursive: true });
copyFileSync(join(workspace, 'target', 'debug', `ryoko-lsp${exe}`), destination);
console.log(`ryoko-lsp sidecar -> ${destination}`);
