import { test } from 'node:test';
import assert from 'node:assert/strict';
import { msiVersion, releaseConfig, releaseVersionFromRef } from '../release_config.mjs';
import { buildManifest, githubAssetName } from '../update_manifest.mjs';

const KEY = 'dW50cnVzdGVkIGNvbW1lbnQ6IG1pbmlzaWduIHB1YmxpYyBrZXkgQUJDREVGCg==';
const ENDPOINT = 'https://github.com/Flanxiyum001/Ryoko/releases/download/desktop-updates/latest.json';

test('release versions increase with the run number and follow tags', () => {
  assert.equal(releaseVersionFromRef('refs/heads/main', 42), '0.1.0-alpha.42');
  assert.equal(releaseVersionFromRef('refs/tags/desktop-preview-v0.2.0-beta.1', 1), '0.2.0-beta.1');
  assert.throws(() => releaseVersionFromRef('refs/tags/desktop-preview-vbanana', 1));
});

test('without a key only the version changes', () => {
  const base = { bundle: { externalBin: ['binaries/ryoko'] } };
  const config = releaseConfig(base, '0.1.0-alpha.7', { pubkey: '' });
  assert.deepEqual(config, {
    bundle: { externalBin: ['binaries/ryoko'], windows: { wix: { version: '0.1.0.7' } } },
    version: '0.1.0-alpha.7',
  });
  assert.equal(base.version, undefined, 'base config is not mutated');
});

test('with a key the updater is enabled with downgrade protection', () => {
  const config = releaseConfig({ bundle: { externalBin: ['x'] } }, '0.1.0-alpha.8', { pubkey: KEY, endpoint: ENDPOINT });
  assert.equal(config.bundle.createUpdaterArtifacts, true);
  assert.deepEqual(config.bundle.externalBin, ['x']);
  assert.equal(config.plugins.updater.pubkey, KEY);
  assert.deepEqual(config.plugins.updater.endpoints, [ENDPOINT]);
  assert.equal(config.plugins.updater.requireSignedVersion, true);
  assert.equal(config.plugins.updater.dangerousInsecureTransportProtocol, undefined);
});

test('rejects insecure endpoints and malformed keys', () => {
  assert.throws(() => releaseConfig({}, '1.0.0', { pubkey: KEY, endpoint: 'http://example.com/latest.json' }), /HTTPS/);
  assert.throws(() => releaseConfig({}, '1.0.0', { pubkey: 'not a key; rm -rf /', endpoint: ENDPOINT }), /public key/);
  assert.throws(() => releaseConfig({}, '1.0', { pubkey: '' }), /semantic/);
});

const files = [
  'dist/Ryoko_0.1.0-alpha.8_x64-setup.exe', 'dist/Ryoko_0.1.0-alpha.8_x64-setup.exe.sig',
  'dist/Ryoko_0.1.0-alpha.8_x64_en-US.msi', 'dist/Ryoko_0.1.0-alpha.8_x64_en-US.msi.sig',
  'dist/Ryoko.app.tar.gz', 'dist/Ryoko.app.tar.gz.sig', 'dist/Ryoko_0.1.0-alpha.8_aarch64.dmg',
  'dist/Ryoko_0.1.0-alpha.8_amd64.AppImage', 'dist/Ryoko_0.1.0-alpha.8_amd64.AppImage.sig',
  'dist/Ryoko_0.1.0-alpha.8_amd64.deb',
];
const readText = (path) => `sig-of:${path}\n`;

test('manifest points each platform at its signed artifact on the versioned release', () => {
  const manifest = buildManifest({
    files, readText, tag: 'desktop-preview-v0.1.0-alpha.8', version: '0.1.0-alpha.8',
    repo: 'Flanxiyum001/Ryoko', notes: 'New stuff', pubDate: '2026-11-23T00:00:00Z',
  });
  assert.equal(manifest.version, '0.1.0-alpha.8');
  assert.deepEqual(Object.keys(manifest.platforms).sort(), ['darwin-aarch64', 'linux-x86_64', 'windows-x86_64']);
  assert.equal(manifest.platforms['windows-x86_64'].signature, 'sig-of:dist/Ryoko_0.1.0-alpha.8_x64-setup.exe.sig');
  assert.equal(
    manifest.platforms['darwin-aarch64'].url,
    'https://github.com/Flanxiyum001/Ryoko/releases/download/desktop-preview-v0.1.0-alpha.8/Ryoko.app.tar.gz',
  );
});

test('manifest refuses unsigned or missing artifacts', () => {
  const unsigned = files.filter((file) => file !== 'dist/Ryoko.app.tar.gz.sig');
  assert.throws(() => buildManifest({ files: unsigned, readText, tag: 't', version: '1.0.0', repo: 'a/b', notes: '' }), /darwin-aarch64/);
  assert.throws(() => buildManifest({ files, readText, tag: 'bad tag', version: '1.0.0', repo: 'a/b', notes: '' }), /tag/);
});

test('asset names match what GitHub stores', () => {
  assert.equal(githubAssetName('dist/Ryoko Setup.exe'), 'Ryoko.Setup.exe');
});

test('release notes are bounded', () => {
  const manifest = buildManifest({ files, readText, tag: 't', version: '1.0.0', repo: 'a/b', notes: 'x'.repeat(20000) });
  assert.equal(manifest.notes.length, 8000);
});

test('MSI versions are numeric', () => {
  assert.equal(msiVersion('0.1.0-alpha.42'), '0.1.0.42');
  assert.equal(msiVersion('1.2.3'), '1.2.3.0');
  assert.equal(msiVersion('0.2.0-beta'), '0.2.0.0');
  assert.throws(() => msiVersion('256.0.0'), /MSI/);
});
