# Native installer smoke evidence (#46)

Status: first automation tranche, not certification. Dispatch the **Desktop installer smoke evidence** workflow with the run ID of a successful unsigned desktop-preview run. It verifies the run identity and downloads the exact three target artifacts using read-only permissions; no signing secrets are passed to the smoke jobs. Use disposable GitHub-hosted runners only.

| Platform | Automated | Remaining manual gates |
| --- | --- | --- |
| Linux x64 | DEB hash, install, launch process liveness for 15 seconds, purge | AppImage, real window, workspace edit/save/relaunch |
| Windows x64 | MSI hash, signature status, install, process liveness for 15 seconds, uninstall | NSIS, real window, workspace edit/save/relaunch, OS trust prompts |
| macOS Apple Silicon | DMG hash, read-only mount, copy, Gatekeeper assessment, removal | Launch and UI workflow on a clean machine; never bypass Gatekeeper |

The report records source SHA, source run ID, digest and limited outcomes. Process liveness does not prove that a window rendered correctly. A macOS security block is a recorded manual gate, not a successful launch. No automated secret scan is claimed; inspect artifact contents for unexpected credentials or private data in a controlled environment before release. Raw environment dumps and unrestricted logs are not uploaded.

## Manual acceptance

Follow [desktop preview testing](../DESKTOP_PREVIEW_TESTING.md): verify the exact published digest and provenance, install through the normal OS UI with protections enabled, open a disposable project, edit and save, close and reopen, confirm bytes persist, uninstall, inspect leftovers and record the observed results. Include screenshots only of the disposable workspace. For each failure record artifact digest, platform, step and bounded error; investigate before approving release. Production-signed builds require separate notarization/Authenticode and update verification.

Do not close #46 until all three platforms have successful digest-linked reports, the manual gates are recorded and the artifact secret check is complete.
