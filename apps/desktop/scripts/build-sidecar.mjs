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

const exe = process.platform === 'win32' ? '.exe' : '';
const sidecars = [
  { pkg: 'ryoko-native', bin: 'ryoko-lsp' },
  { pkg: 'ryoko-sandbox', bin: 'wasm-guardrail-worker' },
];
for (const { pkg, bin } of sidecars) {
  execFileSync('cargo', ['build', '--locked', '-p', pkg, '--bin', bin], {
    cwd: workspace,
    stdio: 'inherit',
  });
  const destination = join(desktop, 'src-tauri', 'binaries', `${bin}-${host}${exe}`);
  mkdirSync(dirname(destination), { recursive: true });
  copyFileSync(join(workspace, 'target', 'debug', `${bin}${exe}`), destination);
  console.log(`${bin} sidecar -> ${destination}`);
}
