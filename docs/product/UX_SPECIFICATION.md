# UX specification

Status: specification for the planned desktop application. The current product is a
terminal CLI.

## Layout

```text
┌──────────────────────────────────────────────────────────────┐
│ title bar: workspace · branch · model · agent activity       │
├────┬─────────────────────────────────────────┬───────────────┤
│ a  │ explorer / search / source control      │ AI panel      │
│ c  │                                         │ chat          │
│ t  │ editor group (tabs, splits)             │ context       │
│ i  │                                         │ agents        │
│ v  │                                         │ approvals     │
│ i  ├─────────────────────────────────────────┤               │
│ t  │ terminal · problems · tests · output    │               │
│ y  │                                         │               │
├────┴─────────────────────────────────────────┴───────────────┤
│ status bar: diagnostics · position · encoding · usage        │
└──────────────────────────────────────────────────────────────┘
```

The activity bar holds explorer, search, source control, agents, extensions,
settings. The AI panel is a first-class pane, resizable and collapsible, never a
modal that blocks editing.

## Modes

- **Editor** — the default. AI is available inline (explain, fix, refactor) and in
  the panel.
- **Agent** — attach a task, choose scope and permissions, watch progress.
- **Mission Control** — team overview, task graph, agent workspace, review centre.

Mode is a view state. Switching never discards work and never starts work.

## Interaction requirements

| Area | Requirements |
| --- | --- |
| Command palette | Every command reachable and discoverable; shows keybinding |
| Keybindings | Platform-aware defaults, fully configurable, conflicts surfaced |
| Tabs | Dirty indicators, preview tabs, close-others, split groups |
| Explorer | Keyboard navigation, rename, new file/folder, delete with confirmation |
| Diff | Side-by-side and inline, hunk staging, whitespace toggle |
| Agents | Status, model, elapsed time, tokens, cost, current tool, cancellable |
| Review | Findings grouped by file, jump to line, severity filters |
| Terminal | Multiple sessions, scrollback, kill confirmation for running processes |
| Errors | Human-readable message, machine code, copyable details, retry where safe |

## Default keybindings

| Shortcut | Action |
| --- | --- |
| Ctrl/Cmd+P | Quick open file |
| Ctrl/Cmd+Shift+P | Command palette |
| Ctrl/Cmd+Shift+F | Search project |
| Ctrl/Cmd+` | Toggle terminal |
| Ctrl/Cmd+B | Toggle sidebar |
| Ctrl/Cmd+/ | Toggle comment |
| Ctrl/Cmd+Enter | Send selection to AI |
| Ctrl/Cmd+Shift+A | Agent panel |
| Ctrl/Cmd+Shift+D | Debug panel |

## States

Every panel defines four states: loading (skeleton, not a spinner over empty
content), empty (explains what to do), error (message, cause, recovery action), and
ready. Long operations show progress with a cancel action and remain cancellable
until completion is reported.

## Truthfulness

The UI must distinguish:

- "agent says complete" from "validated complete";
- "tests passed" from "tests not run";
- "planned" from "implemented";
- provider-reported token counts from locally measured ones.

Never show an optimistic success state for an operation the backend has not
confirmed.

## Accessibility

Keyboard access to every action, visible focus, screen-reader labels on icons and
status changes, no colour-only meaning, reduced-motion support, configurable font
size and contrast, and contrast ratios that meet WCAG AA for text and controls.

## Onboarding

Open or create a project → index → configure a provider (test connection) →
optional GitHub connection → first-run tour of the three modes. Credentials are
entered into a secure field and stored in the OS credential store; they are never
echoed back.
