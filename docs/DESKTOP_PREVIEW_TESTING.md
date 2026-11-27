# Ryoko Desktop — experimental installer testing

**Status:** pre-alpha, unsigned. These builds are for hands-on testing of the
desktop editor, not production deployment. Use a throwaway Git repository with
non-sensitive files and back up anything important.

## Get the latest installers

For public testers, open [Ryoko Releases](https://github.com/Flanxiyum001/Ryoko/releases),
select a release explicitly labeled **Pre-release**, and download the installer
for your operating system. You do not need GitHub Actions access for a public
release. Check the release's `SHA256SUMS.txt` file to verify downloads. Phase 5
also adds GitHub build-provenance attestations for preview assets. Once the
release workflow succeeds, a downloaded installer can be checked with:

```sh
gh attestation verify PATH_TO_DOWNLOADED_INSTALLER --repo Flanxiyum001/Ryoko
```

This verifies the artifact's recorded GitHub workflow identity and digest,
not Windows Authenticode or Apple notarization. Previews remain **unsigned**
at the operating-system level.

For a test build that has not yet been published as a Release:

1. Open [Ryoko preview installer runs](https://github.com/Flanxiyum001/Ryoko/actions/workflows/desktop-preview.yml).
2. Select a **successful** run from `ryoko` or a `desktop-preview-v*` tag.
3. Under **Artifacts**, download the ZIP matching your platform.
4. Extract the ZIP, then install or launch the installer inside:
   - **Windows 10/11 x64:** `.exe` (NSIS) or `.msi` (Windows Installer).
   - **macOS Apple Silicon:** `.dmg`. No Intel macOS build is included yet.
   - **Linux x64:** `.AppImage` (portable) or `.deb` (Ubuntu/Debian).
5. The installer contains the desktop UI, `ryoko-lsp` and the `ryoko` CLI.
   You should not have to compile Rust merely to open and edit files.

GitHub Actions artifacts are retained for **14 days** and may require signing
into GitHub to download. **Windows and macOS previews are unsigned**, so the
operating system may warn or block them. Only install an artifact you trust,
check the successful build run and source commit, and use a test machine or VM
if your OS security policy requires it. Do not disable system-wide protection.

The existing `Desktop installers` workflow remains the separate
production-signing pipeline; this preview workflow never substitutes unsigned
packages for signed production builds.

## Slow downloads from GitHub (Windows)

GitHub chooses its download CDN; Ryoko cannot directly change that route.
If your browser downloads at only a few KB/s, first choose the **smaller
Windows x64 NSIS `.exe`** rather than the MSI: for Alpha 0.7, the EXE is
about 15.2 MB instead of 22.4 MB.

For a resumable download that can use several HTTP connections, open
**PowerShell** and install the open-source aria2 downloader using Windows
Package Manager (one-time setup):

```powershell
winget install --id aria2.aria2 --exact
```

Open a **new PowerShell window** after installation and run:

```powershell
$tag = "desktop-preview-v0.1.0-alpha.7"
$asset = "Ryoko_0.1.0_x64-setup.exe"
$url = "https://github.com/Flanxiyum001/Ryoko/releases/download/$tag/$asset"
aria2c --continue=true --max-connection-per-server=8 --split=8 --min-split-size=1M --file-allocation=none --out=$asset $url
Get-FileHash ".\\$asset" -Algorithm SHA256
```

For newer previews, replace `$tag` with the release tag shown on the
[Releases page](https://github.com/Flanxiyum001/Ryoko/releases). Compare
the printed hash with that release's **SHA256SUMS.txt** before running
the unsigned installer. If the hash differs, delete the file.

Aria2 attempts parallel HTTP range downloads and can resume interrupted
transfers. It **may** improve speed on a throttled connection; it cannot
override a bottleneck imposed by GitHub, your ISP, or the route to GitHub's
download CDN. If speed remains low, try another network (for example, a
phone hotspot), or use a different connection. Do not use unofficial
third-party executable mirrors or bypass security warnings globally.

There is **no verified alternate Ryoko download mirror yet**. An
independently hosted mirror should serve the **exact release bytes** with
matching SHA-256 hashes; an unverified redirect or relay is not a substitute.

## First test session

- [ ] Start Ryoko and open a disposable workspace; close and reopen it.
- [ ] Create or open a text/Rust/TypeScript file, edit, save and verify its
      contents after restarting.
- [ ] Inspect the file explorer and the Git changes drawer.
- [ ] Search a workspace, preview a replacement, then apply only after reviewing
      the affected files. Try changing a file between preview and apply:
      replacement must refuse to overwrite the newer version.
- [ ] Confirm the built-in language server runs. If the workspace requests
      third-party LSP processes, review their command lines and decline unless
      you explicitly installed and trust those binaries.
- [ ] If you configure a model, try a small inline edit on a disposable file;
      review the proposed text and verify indentation is preserved.
- [ ] Open Mission Control and inspect the UI. Executing multi-agent tasks
      requires separately configured models and a reviewed offline sandbox
      image in Docker or Podman; installing Ryoko alone does not satisfy these
      prerequisites.
- [ ] Check that cancelled runs and failed model requests leave no unexpected
      modified files.
- [ ] Windows only: verify the isolated WASM guardrail worker. The build must
      deliver `wasm-guardrail-worker.exe` next to the app executable (or set
      `RYOKO_WASM_WORKER_BIN`). A guardrail run must never kill the app: a
      trapped or endless test artifact must surface as a failed validation
      with the editor session intact, and installing without the worker must
      fail closed instead of running WASM in-process. Record the OS version
      and Ryoko source SHA with the result of this manual test.

For a bug, report the operating system, Ryoko source SHA from the build run,
the exact actions, expected versus actual result, and a screenshot or
redacted error log. **Never upload API keys, private code, or personal files**
to issue reports.

## Releases versus Packages

- **Releases** contains the GUI installers (`.exe`, `.msi`, `.dmg`,
  `.AppImage`, and `.deb`). This is the normal download option for testers.
- **GitHub Packages** contains an optional **Linux x64, CLI-only** OCI
  container image (`ghcr.io/flanxiyum001/ryoko-cli`). It does not include
  the graphical IDE, and Docker/Podman sandboxing of agent runs is not
  configured inside the image. Use it for basic CLI tests and CI, not as
  a replacement for the desktop installer.

For Docker users, after the container package has actually been published
and made public:

```bash
docker pull ghcr.io/flanxiyum001/ryoko-cli:preview
docker run --rm --network none ghcr.io/flanxiyum001/ryoko-cli:preview --help
```

GitHub Container Registry packages may be **private when first created**.
If external testers cannot pull, the package owner must use GitHub's
**Packages → ryoko-cli → Package settings → Change visibility → Public**
control. Publishing a package and making it public are two different checks.

## Publishing the next GitHub prerelease

On the default `ryoko` branch, the **Ryoko preview installers (unsigned)**
GitHub Actions workflow can be started manually using **Actions → Run workflow**.
It also runs on every push to `ryoko` (including merged pull requests).
Automatic releases require both post-merge CI workflows to pass on the
exact commit before native builds start. Superseded branch builds do not
publish; the newest successful commit gets its own release.

It builds native desktop installers on three operating systems, then publishes
a real **GitHub pre-release** with the five installers and SHA-256 checksums
**only after every OS build succeeds**. The CLI container package is built
and published separately in the same workflow after the desktop release
succeeds. If any OS build fails, the release and package jobs do not start.

A run from `ryoko` creates a unique versioned tag, e.g.
`desktop-preview-v0.1.0-alpha.7`. Existing releases are never overwritten.
A tag matching `desktop-preview-v*` can also be pushed explicitly to build
and publish that tag.

The repository currently publishes alpha testing builds as **Latest**;
they are not production-certified. The public share link is
[github.com/Flanxiyum001/Ryoko/releases](https://github.com/Flanxiyum001/Ryoko/releases).

The signed-production `Desktop installers` workflow is separate and requires
Apple and Windows signing credentials. In-app updates require the separate
updater signing configuration below.

## In-app updates

Once a build with the updater is installed, Ryoko checks for new releases
shortly after launch and then every few hours. When one is found, it shows
**A new update is available**, with release notes, **Download and install**,
**Skip this version** and **Later**. Ryoko downloads nothing until you choose
**Download and install**. It verifies the download's signature, installs it and
restarts. You can also use **Help › Check for Updates**, or turn off automatic
checks under **Settings › Updates**.

Builds installed before in-app updates existed (version `0.1.0`) must be
replaced once by hand. The design is recorded in
[ADR-017](decisions/017-in-app-updates.md).

### Maintainer setup (once)

Until these three values exist, release builds ship without the updater and the
workflow publishes no update manifest.

1. On your own machine, generate a signing key pair. Never paste the private key
   into an issue, chat or commit:

   ```sh
   cd apps/desktop
   npx tauri signer generate -w ~/.tauri/ryoko-updater.key
   ```

   Choose a password when asked. Keep the key file and the password backed up
   offline. Losing them means installed apps can no longer update in-app.
2. In GitHub, open **Settings › Secrets and variables › Actions**:
   - **Secrets:** `TAURI_SIGNING_PRIVATE_KEY` (the contents of
     `~/.tauri/ryoko-updater.key`) and `TAURI_SIGNING_PRIVATE_KEY_PASSWORD`.
   - **Variables:** `RYOKO_UPDATER_PUBKEY` (the contents of
     `~/.tauri/ryoko-updater.key.pub`).
3. Publish a release as usual, by pushing a `desktop-preview-v<version>` tag or
   letting the workflow run on `ryoko`. The workflow signs the updater artifacts,
   attaches them to the versioned release and uploads `latest.json` to the
   `desktop-updates` prerelease.

Keep versions increasing. An installed app is offered only versions newer than
its own.
