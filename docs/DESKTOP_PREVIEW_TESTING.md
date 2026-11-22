# Ryoko Desktop — experimental installer testing

**Status:** pre-alpha, unsigned. These builds are for hands-on testing of the
desktop editor, not production deployment. Use a throwaway Git repository with
non-sensitive files and back up anything important.

## Get the latest installers

For public testers, open [Ryoko Releases](https://github.com/Flanxiyum001/Ryoko/releases),
select a release explicitly labeled **Pre-release**, and download the installer
for your operating system. You do not need GitHub Actions access for a public
release. Check the release's `SHA256SUMS.txt` file to verify downloads.

For a test build that has not yet been published as a Release:

1. Open [Ryoko preview installer runs](https://github.com/Flanxiyum001/Ryoko/actions/workflows/desktop-preview.yml).
2. Select a **successful** run from `main` or a `desktop-preview-v*` tag.
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

On the default `main` branch, the **Ryoko preview installers (unsigned)**
GitHub Actions workflow can be started manually using **Actions → Run workflow**.
It also runs on a push to `main` that changes the release workflow, the
CLI container packaging files, or this tester guide.

It builds native desktop installers on three operating systems, then publishes
a real **GitHub pre-release** with the five installers and SHA-256 checksums
**only after every OS build succeeds**. The CLI container package is built
and published separately in the same workflow after the desktop release
succeeds. If any OS build fails, the release and package jobs do not start.

A run from `main` creates a unique versioned tag, e.g.
`desktop-preview-v0.1.0-alpha.7`. Existing releases are never overwritten.
A tag matching `desktop-preview-v*` can also be pushed explicitly to build
and publish that tag.

GitHub's **Latest** badge intentionally doesn't apply to pre-alpha versions;
the correct public share link is
[github.com/Flanxiyum001/Ryoko/releases](https://github.com/Flanxiyum001/Ryoko/releases).

The signed-production `Desktop installers` workflow is separate and requires
Apple and Windows signing credentials. No auto-updater is provided yet.
