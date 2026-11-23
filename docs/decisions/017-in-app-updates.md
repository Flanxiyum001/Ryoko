# ADR-017: Signed in-app updates from GitHub Releases

Status: proposed
Date: 2026-11-23

## Context
Testers install Ryoko from GitHub Releases. Before this change, getting a new
version meant going back to GitHub and installing again. Every build also
reported the same version (`0.1.0`), so an installed app could not tell that it
was out of date.

Updating installs and runs new code, so it must not be possible for anyone
other than the maintainer to make Ryoko install something.

## Decision
1. **Tauri updater plugin.** `tauri-plugin-updater` checks a JSON manifest,
   downloads the platform artifact and verifies its minisign signature against
   a public key built into the app before installing it.
   `requireSignedVersion` is on, so the version announced by the manifest must
   match the version inside the signature. A tampered manifest therefore cannot
   offer an older, validly signed build as an "update".
2. **Keys.** The maintainer generates the key pair. The public key is the
   repository variable `RYOKO_UPDATER_PUBKEY`. The private key and its password
   are the secrets `TAURI_SIGNING_PRIVATE_KEY` and
   `TAURI_SIGNING_PRIVATE_KEY_PASSWORD`, read only by the Tauri CLI during the
   release build. No key is committed.
3. **Versions.** Release builds are versioned to match their tag:
   `desktop-preview-v<version>` builds `<version>`, and builds from `main` are
   `0.1.0-alpha.<run number>`. MSI packages get a numeric
   `major.minor.patch.build` version, because Windows Installer rejects
   prerelease versions.
4. **Manifest location.** `latest.json` is uploaded to a fixed prerelease,
   `desktop-updates`. Its URL never changes, and it never becomes the
   repository's Latest release. The manifest links to the signed artifacts on
   the versioned release.
5. **User control.** The app checks shortly after launch and then at most
   every 6 hours. It can also check from Help › Check for Updates, and
   automatic checks can be turned off in Settings. It shows "A new update is
   available" and a status bar item. Nothing is downloaded until the user
   chooses **Download and install**. Unsaved files are listed for confirmation
   before the app restarts.
6. **Opt-in per build.** The plugin is registered only when the bundled config
   has a public key and HTTPS endpoints. Development builds, and release builds
   made before the key is configured, start normally and say they cannot update
   themselves.

## Alternatives
- **Link to the release page only.** Simple, but it is what testers already do.
- **Custom downloader that checks SHA256SUMS.** The checksum file sits next to
  the artifact, so anyone who can replace one can replace both. A signature
  from a key outside GitHub is stronger.
- **Use the repository's Latest release for the manifest.** Other tag workflows
  also publish Latest releases, which would break the manifest URL.

## Consequences
- Platforms: Windows updates through the NSIS installer, macOS through the
  `.app` bundle, and Linux through the AppImage. Installs from the `.deb` or
  the `.msi` are told about updates, but the updater replaces them using the
  NSIS or AppImage artifact. Re-install manually if you prefer to stay on
  `.deb`/`.msi`.
- Apps installed before this change report version `0.1.0`. Under semantic
  versioning that is newer than every `0.1.0-alpha.N`, and those builds have
  no updater anyway. Testers install once more by hand; after that, updates
  are in-app.
- A tag whose version is lower than an earlier build from `main` is not offered
  as an update. Keep versions increasing.
- Losing the private key means installed apps can no longer be updated in-app
  and must be re-installed with a new public key. Keep a backup offline.
- Updates are not OS code signed. Windows SmartScreen and macOS Gatekeeper
  behaviour is unchanged (see `docs/DESKTOP_PREVIEW_TESTING.md`).

## Verification
- `apps/desktop/src-tauri/src/updater.rs` tests: the updater is enabled only for
  a non-empty key with HTTPS endpoints, and manifest text is bounded and
  stripped of control characters.
- `apps/desktop/scripts/updater/test/`: versioning, MSI versions, HTTPS-only
  endpoints, key validation, and that the manifest refuses unsigned or missing
  artifacts.
- `apps/desktop/ui/src/updates.test.ts`: check intervals, "Skip this version"
  and progress text.
- Playwright: an available update is announced, and `update_install` runs only
  after the user clicks **Download and install**.
- Not yet verified: a real update end to end on Windows, macOS and Linux. That
  needs the signing key configured and two consecutive releases.
