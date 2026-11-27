# Ryoko UX improvements — CLI foundation and desktop workbench

Ryoko provides both a Rust terminal CLI (`crates/ryoko-cli`) and a native Tauri + Monaco desktop workbench (`apps/desktop`). This document records the shipped CLI UX contracts alongside the current desktop workbench status.

## Implemented CLI UX capabilities

| Capability | User benefit | Command |
| --- | --- | --- |
| One-command onboarding | Create a valid local/provider config without embedding secrets | ryoko init |
| Offline setup doctor | Diagnose missing credential environment variables, malformed provider profiles, and unknown role assignments before making paid calls | ryoko doctor |
| Per-role model routing | Assign any configured compatible provider/model to each of the eight specialists | team-preview --team-config |
| Progress and telemetry | See which specialist is running, the chosen model, elapsed time and provider-reported token usage | team-preview |
| Exportable reports | Save Markdown or JSON from the team workflow, including successful partial turns if a provider fails | team-preview --output --format |
| Safer report exports | Do not overwrite existing paths; owner-only permissions where supported | --output, init |
| Saved session discovery | List persisted agent worktrees, review status, and exact review commands; filter/export as JSON | ryoko sessions |
| Cleaner human review | Optional compact diff and an interactive D shortcut to show/hide the full patch | ryoko review --compact |
| Multiline prompting | Use a UTF-8 prompt file rather than giant shell arguments | ryoko ask --prompt-file |
| Local/cloud independence | Only initialize models actually selected, so unused cloud credentials do not block local work | ask, team-preview, run |

**Limits:** The CLI `team-preview` remains advice-only. It does not change files or merge projects. The existing `ryoko run`/`review` workflow edits isolated worktrees and executes user-configured commands locally.

## Quickstart (CLI)

From a trusted checkout:

    cargo run -p ryoko-cli -- init --output ryoko.providers.json
    cargo run -p ryoko-cli -- doctor --config ryoko.providers.json
    cargo run -p ryoko-cli -- sessions --repo .
    cargo run -p ryoko-cli -- team-preview \
      --config ryoko.providers.json \
      --team-config configs/team.example.json \
      --goal "Design an accessible note-taking app" \
      --output team-review.md --format markdown

Edit the team example to match local models you have actually installed. To use a remote endpoint, run init with --id, --base-url (HTTPS) and --api-key-env, then set that environment variable outside your repo. There is no implicit network probe in doctor.

For the desktop IDE workbench, see [`apps/desktop/README.md`](../apps/desktop/README.md) and [`docs/product/UX_SPECIFICATION.md`](product/UX_SPECIFICATION.md).

## Review-safe reports

- Reports are exported **only** when --output is provided, and existing files are never overwritten.
- Markdown reports contain raw model output. Treat this as untrusted content when rendering in a UI; do not execute embedded commands.
- Provider-reported token counts can be missing or incomplete. The CLI intentionally does not claim accurate prices from unknown providers.
- If any specialist fails, earlier completed outputs can still be exported, with status "failed".
- A fully complete multi-agent preview does not mean a functional application was compiled or tested.

## Desktop UX status & remaining roadmap

### Implemented (experimental)

The Tauri/Monaco workbench supports file exploration and editing, tabs and splits, project search, language selection and LSP routing, agent chat and Mission Control, local diff/hunk review, a PTY terminal and DAP bridge, provider setup and opt-in buffer recovery. The CLI remains a separate supported entry point; its advice-only team preview is not a claim of autonomous execution.

Keyboard shortcuts, the skip-to-editor link, semantic landmarks, focus styling and reduced-motion handling exist in the renderer. These are individual implemented mechanisms, not evidence of complete keyboard or screen-reader coverage.

### Planned / unverified

- Complete keyboard navigation, focus restoration, reduced-motion and assistive-technology verification ([#47](https://github.com/Flanxiyum001/Ryoko/issues/47)).
- Production-signed installers, validated signed updates, and independent reproducibility and installer smoke evidence. An unsigned preview is not a production release.
- Verified publisher trust for marketplace installation, full cross-platform sandbox and debugger parity, and end-to-end human-review and security acceptance evidence.

See the [Phase 4 IDE experience](PHASE_4_IDE_EXPERIENCE.md) for workbench scope and acceptance tests and the [Phase 5 release trust plan](PHASE_5_RELEASE_TRUST.md) for outstanding release gates.
