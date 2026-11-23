// Builds the Tauri config override used by the release workflow.
//
// Every release build gets a real, increasing version so an installed app can
// tell it is out of date. When the maintainer has configured an update signing
// key, the override also turns on signed updater artifacts and embeds the
// updater's public key and manifest URL. The private key never appears here:
// the Tauri CLI reads it from TAURI_SIGNING_PRIVATE_KEY at build time.
//
// Usage: node release_config.mjs <base-config.json> <version> <out.json>
//   env RYOKO_UPDATER_PUBKEY   minisign public key (base64), optional
//   env RYOKO_UPDATER_ENDPOINT manifest URL, required with the public key

import { readFileSync, writeFileSync } from 'node:fs';
import { pathToFileURL } from 'node:url';

const SEMVER = /^(0|[1-9]\d*)\.(0|[1-9]\d*)\.(0|[1-9]\d*)(-[0-9A-Za-z-]+(\.[0-9A-Za-z-]+)*)?$/;

export function releaseVersionFromRef(ref, runNumber) {
  const tag = /^refs\/tags\/desktop-preview-v(.+)$/.exec(ref ?? '');
  const version = tag ? tag[1] : `0.1.0-alpha.${Number(runNumber)}`;
  if (!SEMVER.test(version)) throw new Error(`Not a semantic version: ${version}`);
  return version;
}

/**
 * Windows Installer only accepts numeric versions (`major.minor.patch.build`),
 * so `0.1.0-alpha.9` is packaged as MSI version `0.1.0.9`. The in-app updater
 * uses the NSIS installer, which keeps the full version.
 */
export function msiVersion(version) {
  const match = SEMVER.exec(version);
  if (!match) throw new Error(`Not a semantic version: ${version}`);
  const [major, minor, patch] = match.slice(1, 4).map(Number);
  if (major > 255 || minor > 255 || patch > 65535) throw new Error(`Version too large for MSI: ${version}`);
  const build = /\.(\d+)$/.exec(match[4] ?? '');
  return `${major}.${minor}.${patch}.${build ? Math.min(Number(build[1]), 65535) : 0}`;
}

export function releaseConfig(base, version, { pubkey, endpoint } = {}) {
  if (!SEMVER.test(version)) throw new Error(`Not a semantic version: ${version}`);
  const config = structuredClone(base);
  config.version = version;
  config.bundle = config.bundle ?? {};
  config.bundle.windows = {
    ...(config.bundle.windows ?? {}),
    wix: { ...(config.bundle.windows?.wix ?? {}), version: msiVersion(version) },
  };
  const key = (pubkey ?? '').trim();
  if (!key) return config;
  if (!/^[A-Za-z0-9+/=]{40,1024}$/.test(key)) throw new Error('RYOKO_UPDATER_PUBKEY is not a base64 minisign public key');
  const url = new URL(endpoint ?? '');
  if (url.protocol !== 'https:') throw new Error('The update manifest must be served over HTTPS');
  config.bundle = { ...(config.bundle ?? {}), createUpdaterArtifacts: true };
  config.plugins = {
    ...(config.plugins ?? {}),
    updater: {
      pubkey: key,
      endpoints: [url.toString()],
      requireSignedVersion: true,
      windows: { installMode: 'passive' },
    },
  };
  return config;
}

if (import.meta.url === pathToFileURL(process.argv[1] ?? '').href) {
  const [basePath, version, outPath] = process.argv.slice(2);
  if (!basePath || !version || !outPath) {
    console.error('usage: release_config.mjs <base-config.json> <version> <out.json>');
    process.exit(2);
  }
  const base = JSON.parse(readFileSync(basePath, 'utf8'));
  const config = releaseConfig(base, version, {
    pubkey: process.env.RYOKO_UPDATER_PUBKEY,
    endpoint: process.env.RYOKO_UPDATER_ENDPOINT,
  });
  writeFileSync(outPath, `${JSON.stringify(config, null, 2)}\n`);
  console.log(`version ${version}; updater ${config.plugins?.updater ? 'enabled' : 'not configured'}`);
}
