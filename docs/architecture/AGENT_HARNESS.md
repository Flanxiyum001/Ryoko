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
| Built-in tools | `read_file`, `list_files`, `search_text`, `write_file`, `edit_file`, `update_plan`, `remember`, `load_skill`, `run_command` (opt-in), `delegate`. |
| Staged edits | `write_file` and `edit_file` only stage changes. `HarnessOutcome::changes` holds the original and proposed content of each file for diff review. Reads see the staged version. |
| Planning | `update_plan` keeps a short checklist and streams it to the host. |
| Memory | `remember` appends short notes to `.ryoko/agent-memory.md`, and later sessions load them into the system prompt. Notes are capped, flattened to a single line and screened for credentials. |
| Skills | Folders with a `SKILL.md` under `.ryoko/skills`, `.agents/skills` or `.claude/skills`. The prompt lists only each skill's name and description; `load_skill` fetches the body. Skills whose real path leaves the workspace are ignored. |
| Sub-agents | `delegate` runs a fresh agent loop with its own context and the same staged edits, then returns a summary. Nesting is capped by `max_delegation_depth` (default 1). |
| Long sessions | Once the conversation passes `context_char_budget`, older tool output is replaced by a placeholder. The goal and the latest turns are kept. |
| Extensions | Hosts can add `ExternalTool`s (for example approved MCP tools). A tool cannot take over a built-in name. |
| Control | `cancel_handle()` stops the run before its next model turn or tool call. `on_event` streams progress (`HarnessEvent`). |

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
