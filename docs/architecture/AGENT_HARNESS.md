# Agent harness (`ryoko-harness`)

Ryoko's own agent harness, written in Rust so it runs natively on Windows,
macOS and Linux with no Python runtime. Its design borrows the ideas that make
harnesses such as Nous Research's Hermes Agent capable (a long tool loop,
persistent memory, on-demand skills and sub-agents). It does not reuse or
bundle Hermes code.

## What it does

| Capability | How |
| --- | --- |
| Tool loop | Up to `max_turns` (default 40) model turns, up to 16 tool calls per turn. Bad calls come back to the model as errors it can recover from instead of ending the run. |
| Built-in tools | `find_context`, `read_file`, `list_files`, `search_text`, `write_file`, `edit_file`, `update_plan`, `remember`, `load_skill`, `run_command` (opt-in), `delegate`. |
| Staged edits | `write_file` and `edit_file` only stage changes. `HarnessOutcome::changes` holds the original and proposed content of each file for diff review. Reads see the staged version. |
| Planning | `update_plan` keeps a short checklist and streams it to the host. |
| Memory | `remember` appends short notes to `.ryoko/agent-memory.md`, and later sessions load them into the system prompt. Notes are capped, flattened to a single line and screened for credentials. |
| Skills | Folders with a `SKILL.md` under `.ryoko/skills`, `.agents/skills` or `.claude/skills`. The prompt lists only each skill's name and description; `load_skill` fetches the body. Skills whose real path leaves the workspace are ignored. |
| Sub-agents | `delegate` runs a fresh agent loop with its own context and the same staged edits, then returns a summary. Nesting is capped by `max_delegation_depth` (default 1). |
| Long sessions | Once the conversation passes `context_char_budget`, older tool output is replaced by a placeholder. The goal and the latest turns are kept. |
| Extensions | Hosts can add `ExternalTool`s (for example approved MCP tools). A tool cannot take over a built-in name. |
| Control | `cancel_handle()` stops the run before its next model turn or tool call. `on_event` streams progress (`HarnessEvent`). |

## Fast Context

`ryoko_harness::context::FastContext` is an in-memory index of the workspace's
text files. It respects `.gitignore` and skips binaries, `target`,
`node_modules` and `dist`.

* **Terms:** each line becomes search terms. Identifiers are kept whole and
  also split at `camelCase` and `snake_case` boundaries.
* **Ranking:** a query is ranked by the inverse document frequency of the
  terms that fall within a ±4-line window. Definition lines get a ×1.5 boost,
  and files whose path matches get a bonus. Prose files (`.md`, `.txt`) get a
  ×0.7 penalty.
* **Results:** at most two non-overlapping snippets per file.
* **Measured speed:** on this repository (322 files) a release build indexes
  in about 200 ms and answers a query in about 1–2 ms. A unit test asserts
  search over about 800 files takes under 200 ms, even in debug builds.
* **Where it's used:**
  * the agent's `find_context` tool;
  * Ask chat, which automatically attaches the top 4 snippets as `<related_code>`;
  * Ryoko Review, to fetch related code;
  * the desktop `fast_context_search` command;
  * `ryoko find "<query>"` in the CLI.
* **Desktop caching:** the desktop app keeps one index per workspace and
  rebuilds it when it is older than 30 seconds.

## Ryoko Review

`ryoko_harness::review` builds unified diffs (an LCS line diff with 3 lines of
context) and asks a model for a JSON report: a verdict, a summary and
findings, each with a path, line, severity (`blocker`, `warning` or
`suggestion`), title and detail.

The reply is untrusted:

* only the JSON object is parsed;
* findings for files that are not in the diff are dropped;
* counts and text lengths are capped;
* a blocker always turns the verdict into `needs_changes`.

It is available in three places:

* **Agent changes card:** "Review with Ryoko". Clicking a finding opens the
  diff at that line.
* **Changes panel:** "Review with Ryoko" reviews `git diff HEAD`. Clicking a
  finding opens the file at that line.
* **CLI:** `ryoko review-diff [--staged | --base main] [--fail-on-blocker]`.

## Agent swarm

`ryoko_harness::swarm::Swarm` coordinates a swarm of agents.

1. A coordinator model splits the goal into 1–6 independent tasks, each one a
   pull-request-sized unit. If its reply is unusable, the whole goal becomes a
   single task.
2. Each task runs in its own `ryoko-worktree` lease and branch
   (`ryoko/swarm-N/<id>`), at most 3 at a time by default.
3. In each task, a harness agent stages edits, the edits are written into the
   worktree (never the user's checkout), and Ryoko Review reviews the diff.
4. The branch is committed with the task title as the message. A
   pull-request body is drafted with the summary, the files and the review
   findings.
5. Tasks with no changes or errors are abandoned.

Branches stay local until the user publishes them:

* **Desktop:** "Push & open PR" pushes the branch and shows a link that
  opens a prefilled pull request on GitHub.
* **CLI:** `ryoko swarm --goal … --push` does the same.

Swarm branches are normal worktree sessions, so they also appear under
Running agents and in `ryoko sessions`.

## Safety rules

* Nothing is written to project files. The host applies only the changes the user approves.
* `run_command` is offered only when the host sets `allow_commands`, and the
  host's `ApprovalGate` must approve **each** call. The default gate denies
  everything. Commands:
  * run without a shell (a bare program name only);
  * get a minimal environment, so no API keys are forwarded;
  * get no stdin;
  * stop after a 120-second timeout;
  * have their output capped.
* Paths must be relative, use `/`, and must not contain `..` or hidden folders.
  Reads also go through `ryoko-tools`' canonical-path checks.
* File contents, tool output and memory are presented to the model as data.
  The system prompt tells it to ignore instructions found inside them.

## Using it

```bash
ryoko agent --config providers.json --model openai/gpt-4.1 --goal "Add input validation to the signup form"
```

The flags are listed below.

* `--allow-commands` lets the agent ask to run tests or builds. Each command is confirmed with `y/N`.
* `--output run.json` saves the full proposal.
* `--no-delegate` turns off sub-agents.
* `--max-turns` changes the turn limit.

## Not done yet

* Desktop wiring: show staged changes in the diff review panel, ask for
  command approval in the UI, and stream `HarnessEvent`s to the chat and
  Tasks panels.
* Applying approved changes from the CLI. Today the CLI prints the summary
  and saves the JSON.
* Using the harness as the engine for the orchestrator's team roles.
