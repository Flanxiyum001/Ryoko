# UI verification: harness, payloads and critique

Agents do not write browser scripts. They call one bundled harness, so every run
captures the same artifacts in the same shape, and a text-only model can verify
semantics without seeing a pixel.

## 1. The harness

`ryoko sandbox init` bakes `verify-ui.mjs` into the image when the project
declares frontend tooling, at a fixed path:

```sh
node /opt/ryoko/verify-ui.mjs \
  --dev-command "npm run preview -- --port 4173" \
  --routes /,/pricing \
  --out /ryoko-artifacts
```

`VERIFY_UI_CONTAINER_PATH` (`/opt/ryoko/verify-ui.mjs`) is a constant in
`ryoko-sandbox`, and the Dockerfile also sets `RYOKO_VERIFY_UI` to it, so an agent
finds the harness without guessing.

Lifecycle: the harness spawns the dev command in its own process group, polls
the URL until it answers with anything below HTTP 500 (or the ready timeout
expires), captures, and then sends `SIGTERM` to the whole group and `SIGKILL`
after five seconds. Killing the group matters: `npm run preview` leaves a `vite`
grandchild behind that a plain `child.kill()` orphans.

Only loopback URLs are accepted (`127.0.0.1`, `localhost`, `[::1]`, `0.0.0.0`), so
the harness cannot be pointed at the open internet from inside the container.

Per route, under `<out>/<route>/`:

| Artifact | Content |
| --- | --- |
| `desktop.png` | full-page screenshot at 1440×900 |
| `mobile.png` | full-page screenshot at 390×844 |
| `aria-tree.json` | `{ source, tree }` — ARIA snapshot, falling back to `page.accessibility.snapshot()`, then to a pruned DOM outline |
| `console-errors.json` | console errors, uncaught exceptions, failed local requests, and `document.documentElement.scrollWidth > window.innerWidth` for both viewports |

A route slug is the route with non-alphanumerics collapsed to `-`; `/` becomes
`root`. `summary.json` at the top level aggregates issue counts.

Exit codes: `0` clean, `1` issues captured, `2` harness or launch failure, `3` the
server never became ready. `--allow-issues` reports issues without failing.
`--check-config` prints the plan and exits without importing Playwright, which is
what the tests use.

## 2. Payloads

`ryoko_core::vision::read_captures(dir)` reads that directory into one
`RouteCapture` per route, with every string bounded (24k characters of ARIA tree,
50 issues at 500 characters each).

`vision_message(captures, model)` then builds the prompt:

- **vision model** — a text part plus one `ContentBlock::Image` per PNG. The
  block carries base64 PNG data and is validated in `ryoko-models` (media type in
  `image/png`, `image/jpeg`, `image/webp`; base64 characters only; at most 6 MiB).
- **text-only model** — no image blocks at all. The prompt says so explicitly and
  carries the accessibility tree plus the measured overflow report instead.

`supports_vision()` is deliberately conservative: a model name is matched against
a short list of known vision markers (`gpt-4o`, `claude-3`/`claude-sonnet`,
`gemini`, `qwen2-vl`, `llava`, …) and **anything unrecognised is treated as
text-only**. Sending PNG bytes to a model that cannot read them wastes tokens and
returns nonsense.

Adapters render the image block in their native shape: OpenAI `image_url` data
URL, Anthropic `image`/`base64` source, Gemini `inlineData`.

## 3. Critique schema

The UI/UX agent must answer with JSON, which `VisualCritique::parse` extracts
from surrounding prose or a fenced block:

```json
{
  "passed": false,
  "layout_issues": ["CTA overlaps the footer at 390px", "mobile horizontal overflow on /pricing"],
  "revision_instructions": "Stack the footer below the CTA and constrain the table width"
}
```

Validation is enforced, not trusted: at most 50 issues, 500 characters each, 4 000
characters of instructions, and a failed critique must carry at least one issue or
an instruction — a bare `{"passed": false}` is rejected rather than routed as an
empty repair.

`VisualCritique::feedback()` produces the text for the implementing agent, and
`VISUAL_FEEDBACK_METADATA` (`ryoko.visual-feedback`) is the metadata key the
orchestrator uses to carry it. `VisualCritique::report()` converts the critique
into the same `ValidationReport` shape every other check uses.

## The review loop

A `UiUxDesigner` task is a visual review when the harness has captured routes for
it. The agent reads `<project root>/.ryoko/artifacts/<task id>/`, which the
validator's writable mount owns, and attaches `vision_message(captures, model)`:
screenshots and the overflow report for a model that `supports_vision`, the ARIA
tree and the overflow report alone for a text-only model. With no captures the
task keeps its ordinary text prompt and proposal contract, so a designer task that
runs before any verification still works.

A review reply is parsed with `VisualCritique::parse` instead of as a file
proposal. Passing answers succeed with no proposed changes. A failing critique
returns `TaskResult::failure` carrying `visual_feedback`, which
`execute_task` copies to `TaskOutcome::Failed { visual_feedback }` and then to
`TaskFailure::visual_feedback`. `is_actionable_review_failure` accepts that flag
alongside the QA and Security roles, so `repair_review_failures` routes the design
feedback to the producer the review depends on, re-runs the producer, and
re-runs the review. Whatever still fails after the repair budget escalates to
human review. A reply that is not a valid critique is a plain failure: prose
cannot be routed as design feedback.

## What is not implemented

- **Nothing schedules the capture for a review task.** The loop works from
  whatever captures exist under the review task's artifact directory; no
  component runs `verify-ui.mjs` for that task id yet. Captures are produced today
  by an operator or the validator for a producer task.
- **No browser was executed.** The harness is syntax-checked, its argument
  handling and its process lifecycle are tested against a real local server, but
  the Playwright capture path has never run: this environment has no browser
  binaries and no container image with them baked in.
- **One capture per viewport pair.** Device emulation beyond width/height (touch,
  DPR, user agent) is not applied.
