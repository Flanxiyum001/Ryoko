# Ryoko UX improvements — implementation and next milestones

Ryoko is currently a Rust terminal application, not yet a graphical IDE. This milestone makes the terminal workflow easier to adopt and provides data contracts that can back a future desktop experience.

## Implemented on this branch

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

**Limits:** The team preview remains advice-only. It does not change files or merge projects. The existing ryoko run/review workflow edits isolated worktrees and executes user-configured commands locally; it is NOT a hardened code sandbox. Per-role model routing is supported for team-preview. The run/review single coding agent still uses one selected model.

## Quickstart

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

## Review-safe reports

- Reports are exported **only** when --output is provided, and existing files are never overwritten.
- Markdown reports contain raw model output. Treat this as untrusted content when rendering in a future UI; do not execute embedded commands.
- Provider-reported token counts can be missing or incomplete. The CLI intentionally does not claim accurate prices from unknown providers.
- If any specialist fails, earlier completed outputs can still be exported, with status "failed".
- A fully complete multi-agent preview does not mean a functional application was compiled or tested.

## Product UX roadmap — not implemented here

1. **Graphical code editor:** tabs, Monaco-like editing, tabs and integrated diff viewer.
2. **Agent activity board:** live DAG graph with parallel workers, handoff traces, retries and human review checkpoints.
3. **Guided project wizard:** app templates and architecture preferences, project import, Git hosting connection and consent.
4. **Secure tool permission system:** granular read/write/exec/network approvals, scope expirations, per-agent policies and audit log.
5. **Visual app preview:** safely hosted web preview, hot reload, screenshot/DOM feedback and browser tests.
6. **Native provider adapters:** dedicated protocols for APIs that aren't OpenAI chat-completions compatible; feature detection for tools and streaming.
7. **Usage budgets and cancellation:** accurate cost telemetry, estimated ceilings, reservations, per-role caps and cancel controls.
8. **Context manager:** search/index/retrieve source files, token budgeting, project knowledge and durable reviewed memory.
9. **Accessibility and theming:** keyboard-first operation, dark/light themes, screen-reader labels and reduced-motion support.
10. **Release workflow:** test matrix, sandbox isolation, CI status, GitHub PR creation, staging deploys and production approval gates.

These features require additional architecture and validation and should not be advertised as functional until each is shipped and tested.
