# Ryoko Desktop — experimental installer testing

**Status:** pre-alpha, unsigned. These builds are for hands-on testing of the
desktop editor, not production deployment. Use a throwaway Git repository with
non-sensitive files and back up anything important.

## Get the latest installers

1. Open [Ryoko preview installer runs](https://github.com/Flanxiyum001/Ryoko/actions/workflows/desktop-preview.yml).
2. Select the most recent **successful** run from the `preview/desktop-installers` branch.
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

## Making an explicit GitHub prerelease

The preview workflow also supports Git tags beginning with
`desktop-preview-v`. Once a platform build run is green and you want a
**public experimental prerelease**, create and push a uniquely named tag at the
verified commit, for example:

```bash
git fetch origin preview/desktop-installers
git checkout --detach origin/preview/desktop-installers
git tag desktop-preview-v0.1.0-alpha1
git push origin desktop-preview-v0.1.0-alpha1
```

The tag triggers all three installer builds. A release is published **only if
they all succeed**, and is marked `prerelease` with SHA-256 checksums. It is
not marked as the latest stable release. No tag or GitHub Release is created by
ordinary preview branch pushes. Do not reuse an existing release tag.

After this workflow is merged into the default branch, GitHub's
`workflow_dispatch` option can also be used for artifact-only builds.
No auto-updater is provided yet.
