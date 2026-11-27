# UX specification

Status: active specification for the Ryoko desktop workbench (`apps/desktop`, Tauri + Monaco) alongside the `ryoko-cli` terminal workflow. See [DESKTOP_ARCHITECTURE.md](../architecture/DESKTOP_ARCHITECTURE.md) and [DESKTOP_IDE_WORKBENCH.md](../architecture/DESKTOP_IDE_WORKBENCH.md) for the implemented IPC and shell contracts.

## Implemented (experimental) and planned / unverified

The interaction table below identifies existing code paths, not certified feature completeness. The layout, modes, states, onboarding and accessibility sections define product requirements; untested behavior remains a target. See the [Phase 4 IDE experience](../PHASE_4_IDE_EXPERIENCE.md) and [Phase 5 release trust plan](../PHASE_5_RELEASE_TRUST.md) for acceptance gates and evidence.

## Layout

```text
┌──────────────────────────────────────────────────────────────┐
│ title bar: workspace · branch · model · agent activity       │
├────┬─────────────────────────────────────────┬───────────────┤
│ a  │ explorer / search / source control      │ AI panel      │
│ c  │                                         │ chat          │
│ t  │ editor group (tabs, split pane)         │ context       │
│ i  │                                         │ agents        │
│ v  │                                         │ approvals     │
│ i  ├─────────────────────────────────────────┤               │
│ t  │ terminal · output · search · debug      │               │
│ y  │                                         │               │
├────┴─────────────────────────────────────────┴───────────────┤
│ status bar: diagnostics · position · encoding · usage        │
└──────────────────────────────────────────────────────────────┘
```

The activity rail switches between Files (explorer), Search, Editor, Agent Chat, Mission Control, Changes, Issues (problems), Extensions, and Settings. The inspector pane is resizable and collapsible, never a modal that blocks editing.

## Modes

- **Editor** — the default. AI is available inline (`Mod+K` explain, fix, refactor) and in the Chat panel.
- **Agent** — attach a task, choose scope and permissions, watch progress.
- **Mission Control** — team overview, tool stream, agent sessions, and review inbox.

Mode is a view state. Switching never discards work and never starts work.

## Interaction requirements

| Area | Requirements | Status |
| --- | --- | --- |
| Command palette | Every command reachable and discoverable; shows keybinding | Implemented (experimental) (`command_palette.ts`, `commands.ts`) |
| Keybindings | Platform-aware defaults, configurable overrides, Vim/Emacs presets | Implemented (experimental) (`keybindings.ts`, `keymap_preset.ts`) |
| Tabs & splits | Dirty indicators, close confirmation, split editor right | Implemented (experimental) (`editor_tabs.ts`, `#secondaryPane`) |
| Explorer | Keyboard tree navigation, new file/folder, rename, delete with confirmation | Implemented (experimental) (`tree.ts`, `workspace.rs`) |
| Diff | Side-by-side Monaco diff, hunk staging/rejection, proposal review | Implemented (experimental) (`proposal_review.ts`, `review.rs`) |
| Agents | Status, model, elapsed time, tokens, cost, current tool, cancellable | Implemented (experimental) (`mission.ts`, `agent_chat.ts`, `run.rs`) |
| Review | Findings grouped by file, jump to line, severity filters | Implemented (experimental) (`ryoko_review.ts`) |
| Terminal & Debug | Local PTY terminal and DAP debugger workbench | Implemented (experimental) (`terminal.rs`, `debug_adapter.rs`) |
| Crash recovery | Opt-in atomic buffer snapshots with diff preview and consent on restart | Implemented (experimental) (`recovery.rs`, `recovery.ts`) |
| Errors | Human-readable message, actionable guidance, status bar + toast | Implemented (experimental) (`toasts.ts`) |

## Default keybindings

| Shortcut | Action |
| --- | --- |
| Ctrl/Cmd+P | Quick open file |
| Ctrl/Cmd+Shift+P | Command palette |
| Ctrl/Cmd+Shift+F | Search project |
| Ctrl/Cmd+Shift+R | Replace in files |
| Ctrl/Cmd+\` | Toggle terminal |
| Ctrl/Cmd+B | Toggle sidebar |
| Ctrl/Cmd+J | Toggle bottom panel |
| Ctrl/Cmd+\\ | Split editor right |
| Ctrl/Cmd+K | Inline edit with agent |
| Ctrl/Cmd+Shift+G | Toggle changes drawer |
| Ctrl/Cmd+Shift+D | Toggle debug panel |
| Ctrl/Cmd+, | Open settings |

## States

Panels expose explicit loading, empty, error, and ready states without claiming optimistic success before backend confirmation. Long operations remain cancellable until completion is reported.

## Truthfulness

These are required UI contracts, not a guarantee that every path has been independently validated. Human review and release/security gates must be verified against the corresponding execution evidence.

The UI distinguishes:

- "agent says complete" from "validated complete";
- "tests passed" from "tests not run";
- "planned" from "implemented";
- provider-reported token counts from locally measured ones.

Never show an optimistic success state for an operation the backend has not confirmed.

## Accessibility — requirements and current evidence

The renderer contains a skip link, landmarks, named controls, live regions, focus styles and reduced-motion rules. Existing renderer tests exercise selected keyboard navigation and settings focus restoration. Full keyboard-only traversal of every view, dialog focus containment/restoration, non-color status cues and native screen-reader verification remain unverified ([#47](https://github.com/Flanxiyum001/Ryoko/issues/47)). The following are requirements, not a conformance statement:

- Skip-to-editor link (`Skip to editor`) and semantic landmarks (`nav`, `aside`, `main`, `role="tree"`, `role="tablist"`).
- Keyboard access to every action, arrow-key navigation in menus, tabs, and the explorer tree, plus `Escape` handling and focus restoration in dialogs.
- Visible `:focus-visible` outlines, screen-reader `aria-label` attributes on icon buttons, `aria-live` status regions, and no colour-only meaning.
- `@media (prefers-reduced-motion: reduce)` disables CSS transitions/animations and Monaco smooth scrolling.

## Onboarding

This is the intended flow; native credential storage and onboarding must be validated on each supported platform before claiming production readiness.

Open or create a project → index → configure a provider (test connection) →
optional GitHub connection → first-run tour. Credentials are entered into a
secure field and stored in the OS credential store; they are never echoed back.
