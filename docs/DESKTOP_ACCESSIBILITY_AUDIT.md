# Desktop accessibility audit — first tranche (#47)

Status: source review and proposed renderer regression coverage, not accessibility certification. Base: `5ca2814d65bfa49d1979f501b658c2c0a462634e`.

## Source review

- `ui/index.html`: skip-to-editor link, focusable main landmark, named activity rail and explorer, live editor/run status regions.
- `ui/src/styles.css`: focus-visible styles and reduced-motion CSS; `main.ts` sets primary and split Monaco smooth scrolling from the initial OS preference.
- Existing Playwright workbench cases cover keyboard tree and tab navigation, settings focus restoration, named action controls and command execution.
- Verified gap: quick-open's input stopped Tab propagation before the dialog's trap; command palette did not trap Tab at all. Both exposed every listbox option as a separate tab stop despite using aria-activedescendant. The proposed fix retains focus on the combobox, supports arrow selection and Escape restoration, and preserves pointer selection.

## Manual verification checklist (not yet executed)

Use a disposable workspace and test in installed Windows, macOS and Linux builds with default and high-contrast themes. Record version/SHA, OS, screen reader, keyboard sequence, observed outcome and evidence; do not record provider credentials or private files.

| View | Keyboard-only procedure and expected result |
| --- | --- |
| Shell / explorer | Tab to skip link, Enter, traverse activity rail; open workspace, navigate tree with arrows, open file and verify visible focus. |
| Editor tabs and split | Open two files, move tabs with arrows, close dirty tab, cancel and confirm; ensure focus and unsaved text remain correct. Check keyboard escape from Monaco. |
| Quick open / commands | Open from focused control, type query, use arrows, Tab/Shift+Tab, Enter or Escape. Verify active option announced and focus restored on cancellation, including no results. |
| Review / changes | Reach changed-file and hunk actions without pointer; inspect validation outcome, cancel destructive actions, ensure non-color status labels and correct focus. |
| Agent / Mission Control | Reach prompt, scope, run, cancel, activity and result controls; confirm progress/error announcements do not overwhelm the reader. |
| Settings / dialogs | Open and close with keyboard, cycle focus both directions, Escape and restore the trigger; check marketplace, confirmation and recovery dialogs separately. |
| Reduced motion | Enable OS preference before launch, verify transitions, activity visuals and editor scrolling. Change preference while running and record any stale settings. |

## Evidence still required

Run renderer unit tests and Playwright; add an automated accessibility scan of representative states; manually use NVDA (Windows), VoiceOver (macOS), and Orca (Linux) where supported. Full contrast, status-cue, focus-order and native-dialog audits are open. File separately scoped follow-ups for any observed failures. Do not close #47 until the required evidence is recorded.
