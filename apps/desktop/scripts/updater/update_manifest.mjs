// Writes the `latest.json` manifest the in-app updater reads.
//
// It points each platform at the signed updater artifact on the versioned
// GitHub release and embeds that artifact's signature. The app verifies the
// signature against its built-in public key before installing anything, so the
// manifest alone cannot make the app run unsigned code.
//
// Usage: node update_manifest.mjs <dist-dir> <tag> <version> <repo> <notes-file> <out.json>

import { readFileSync, readdirSync, statSync, writeFileSync } from 'node:fs';
import { basename, join } from 'node:path';
import { pathToFileURL } from 'node:url';

/** Updater artifact suffix for each Tauri updater platform key. */
export const PLATFORM_ARTIFACTS = {
  'windows-x86_64': '-setup.exe',
  'darwin-aarch64': '.app.tar.gz',
  'linux-x86_64': '.AppImage',
};

const MAX_NOTES = 8000;

function walk(dir) {
  return readdirSync(dir).flatMap((name) => {
    const path = join(dir, name);
    return statSync(path).isDirectory() ? walk(path) : [path];
  });
}

/** GitHub stores uploaded asset names with spaces replaced by dots. */
export function githubAssetName(file) {
  return basename(file).replace(/ /g, '.');
}

export function buildManifest({ files, readText, tag, version, repo, notes, pubDate }) {
  if (!/^[\w.-]+\/[\w.-]+$/.test(repo)) throw new Error(`Bad repository: ${repo}`);
  if (!/^[\w.-]+$/.test(tag)) throw new Error(`Bad tag: ${tag}`);
  const platforms = {};
  for (const [platform, suffix] of Object.entries(PLATFORM_ARTIFACTS)) {
    const matches = files.filter((file) => file.endsWith(suffix) && files.includes(`${file}.sig`));
    if (matches.length !== 1) {
      throw new Error(`Expected exactly one signed ${suffix} artifact for ${platform}, found ${matches.length}`);
    }
    const [artifact] = matches;
    const signature = readText(`${artifact}.sig`).trim();
    if (!signature) throw new Error(`Empty signature for ${artifact}`);
    platforms[platform] = {
      signature,
      url: `https://github.com/${repo}/releases/download/${tag}/${encodeURIComponent(githubAssetName(artifact))}`,
    };
  }
  return {
    version,
    notes: String(notes ?? '').slice(0, MAX_NOTES),
    pub_date: pubDate,
    platforms,
  };
}

if (import.meta.url === pathToFileURL(process.argv[1] ?? '').href) {
  const [dist, tag, version, repo, notesFile, out] = process.argv.slice(2);
  if (!out) {
    console.error('usage: update_manifest.mjs <dist-dir> <tag> <version> <repo> <notes-file> <out.json>');
    process.exit(2);
  }
  const manifest = buildManifest({
    files: walk(dist),
    readText: (path) => readFileSync(path, 'utf8'),
    tag,
    version,
    repo,
    notes: readFileSync(notesFile, 'utf8'),
    pubDate: new Date().toISOString(),
  });
  writeFileSync(out, `${JSON.stringify(manifest, null, 2)}\n`);
  console.log(`latest.json for ${version}: ${Object.keys(manifest.platforms).join(', ')}`);
}
