# Ryoko

**Ryoko** is an experimental Rust foundation for a provider-neutral, multi-agent AI IDE. The target experience is a full engineering team—project manager, architect, senior UI/UX, backend, full-stack, security, QA and DevOps—collaborating on a user-defined application, with isolated worktrees and human-reviewed changes.

> **Project status:** pre-alpha. The current workspace supports provider setup, chat, review-only team previews, an isolated human-reviewed file-generation workflow, editor language-server scaffolding, a Tauri desktop shell with a Monaco editor wired to `ryoko-lsp`, WASI test execution and structural context extraction. It does not yet provide a task-DAG visualizer, hunk-level review, hardened project-specific build isolation, signed installers or deployment. Do not use its previews as proof of successful implementation.

The product and engineering plan for the desktop IDE, agent runtime and release gates is in [`docs/RYOKO_2_MASTER_BLUEPRINT.md`](docs/RYOKO_2_MASTER_BLUEPRINT.md). Those documents describe intent; only the crates and commands documented below are implemented. Contribution rules are in [`AGENTS.md`](AGENTS.md).

## Prerequisites

- Rust 1.85+ with Cargo
- An OpenAI **chat-completions compatible** server, e.g. an Ollama /v1 endpoint, an OpenAI API endpoint, or another compatible model server, or an Anthropic Messages-compatible endpoint
- An API key in an environment variable for authenticated providers

A custom endpoint is not a universal protocol translator. Anthropic Messages is supported natively; other provider-native wire APIs still need separate adapters, and tools, streaming and token accounting differ by model.

## Getting started

Copy `configs/providers.example.json` to your own file, then edit the provider IDs and URLs. The example config supports a local Ollama endpoint on port 11434, OpenAI, and a custom HTTPS endpoint. **Do not put API keys directly in JSON or in git.** Use `api_key_env` to name an environment variable, for example:

```sh
export RYOKO_OPENAI_API_KEY="your-key-from-provider"
cargo run -p ryoko-cli -- providers --config configs/providers.example.json
cargo run -p ryoko-cli -- discover-local  # probes only local Ollama / LM Studio
cargo run -p ryoko-cli -- roles
cargo run -p ryoko-cli -- plan --goal "Build a collaborative task manager"
cargo run -p ryoko-cli -- ask --config configs/providers.example.json --model ollama/qwen2.5-coder --prompt "Design a REST API for tasks"
```

To run an **eight-turn, read-only multi-agent preview** against a chosen model (this contacts your provider and may incur model usage charges):

```sh
cargo run -p ryoko-cli -- team-preview \
  --config configs/providers.example.json \
  --model ollama/qwen2.5-coder \
  --goal "Build an accessible team task manager"
```

Use the optional team profile to assign a different configured provider/model to each specialist. The CLI shows live role progress and reported tokens; save a report without overwriting existing files:

```sh
cargo run -p ryoko-cli -- init --output ryoko.providers.json
cargo run -p ryoko-cli -- doctor --config ryoko.providers.json
cargo run -p ryoko-cli -- team-preview \
  --config ryoko.providers.json \
  --team-config configs/team.example.json \
  --goal "Build an accessible team task manager" \
  --output team-report.md --format markdown
```

You must edit model names to match your installed local models or hosted provider configuration. Team configurations support a default_model and role_models keyed by role names. A --model CLI value overrides only the default, not explicit per-role selections. Use --format json for machine-readable reports. Reports can contain proprietary context and never overwrite existing paths. If a provider fails, completed turns are retained in the exported partial report; a failure still returns a nonzero exit status.

Other useful commands include:

```sh
cargo run -p ryoko-cli -- ask --config ryoko.providers.json \
  --model ollama/qwen2.5-coder --prompt-file prompt.txt
cargo run -p ryoko-cli -- sessions --repo .
cargo run -p ryoko-cli -- sessions --repo . --state published --json
cargo run -p ryoko-cli -- review --repo . --compact
```

The doctor checks configuration and credential environment variables offline: it does not send requests, test a server, or incur charges. The review screen supports D to toggle the full patch. Team preview remains advice-only and does not write generated code or run commands. See the [UX upgrade notes](docs/UX_UPGRADES.md) for details and the unimplemented graphical IDE roadmap.

## Isolated generation and review

`run` creates a temporary branch and worktree, asks the model for a structured JSON file proposal, writes only those paths inside the worktree, and runs the project's build/test command before publishing it for review. `review` shows the Git diff and the pre-flight result, then accepts, rejects, or sends feedback back to the model.

```sh
cargo run -p ryoko-cli -- run \
  --config configs/providers.example.json \
  --model ollama/qwen2.5-coder \
  --repo . \
  "Add a config loader with unit tests"

cargo run -p ryoko-cli -- review --repo .
```

`run` auto-detects common Cargo, npm, Python and Go test commands; use `--check-command` to override it. `review` merges only after explicit human acceptance, removes the temporary worktree on accept or reject, and never pushes or deploys anything. Pre-flight commands now run in a Docker or Podman Linux container by default, **never directly on the host**. The operator must provide an offline-ready OCI image with the project's compiler and dependencies using `--sandbox-image`; otherwise validation refuses to run. Containers get only the worktree bind mount (read/write for build outputs), no network, no inherited secrets, a read-only root filesystem, dropped Linux capabilities and CPU/memory/process/time limits. For example:

```sh
# Prepare and review the image separately. Test dependencies must be baked in:
cargo run -p ryoko-cli -- run --repo . --sandbox-backend docker \
  --sandbox-image my-reviewed-test-image:2026-11 --config configs/providers.example.json \
  --model ollama/qwen2.5-coder "Fix the parser tests"
```

The intentionally dangerous `--unsafe-host-check` escape hatch is restricted to trusted projects and emits a warning; it is never chosen automatically. Use the same flags for `ryoko review` when revising a task. The container image is not pulled during validation; build it before running Ryoko. See `docs/security/EXECUTION_SANDBOX.md` for known limitations and outstanding security gates. Before acceptance, every proposal is also secret-scanned; `cargo audit` and `npm audit` run when their manifests, lockfiles and tools are available. Findings block the merge, while missing audit tools are reported as skipped rather than clean. Pass `--budget-usd`, `--budget-tokens` or `--budget-secs` for an overall run cap, and `--task-budget-usd`, `--task-budget-tokens` or `--task-budget-secs` to cap each DAG task independently. The ledger checks before dispatch; one in-flight request can still cross a cap because its size and price are unknown until the response arrives.

## Executable multi-agent DAG (experimental)

Pass `--team` to `ryoko run` to execute Architect → Backend → (QA and Security in parallel) using the existing task DAG, with `--max-concurrency 1..4`. Each role makes a real model call, proposes code and tests, runs a container-backed pre-flight check, and publishes an independently reviewable Git branch. Backend inherits Architect's *validated published commit*; QA and Security inherit Backend. Failed dependencies are skipped rather than rerun from stale source. The original checkout is never edited automatically.

Run with an explicit reviewed OCI image and configured model, for example:
```sh
ryoko run --team --max-concurrency 2 --repo . \
  --config ryoko.providers.json --model local/qwen2.5-coder \
  --sandbox-image reviewed-offline-toolchain:latest \
  --event-log .ryoko/events/team.jsonl "Implement rate-limited authentication"
```

**Review boundary:** QA and Security are separate sibling branches. Ryoko does not silently combine their changes or automatically merge to `main`; use `ryoko review --task <id>` and resolve any overlapping edits. Multiple independent dependency branches fail closed until an explicit integration step exists. Mission Control displays per-task ledger token totals and validation logs, but raw provider prompts and verified charges are deliberately *not* stored in the event log.

## Selective hunk review (CLI alpha)

In `ryoko review`, choose **[H]unks** to inspect numbered Git text hunks and enter IDs such as `1,3`. Ryoko creates an entirely new `ryoko/hunk-review/*` worktree at the target branch tip, applies only those hunks with `git apply --index` (no shell), commits the selection, and **reruns the full sandboxed validation**. Only an explicit `MERGE` confirmation after a passing validation can integrate the selection. Cancelling, failing validation, or failing merge preserves the separate selected branch for a later review. The original agent branch is never modified or discarded.

Limits: only ordinary unified text hunks are selectable; binary, renames, deletions and mode changes are excluded. Newly created files can be selected only in full. Diff patches are capped at 1 MiB/128 hunks. This is Git staging, **not** an editor privilege to execute arbitrary commands or merge without approval.

## Desktop Mission Control

The desktop sidebar also shows **Agent Sessions**: a read-only list of the
active and published worktree leases recorded by `ryoko run` (agent, branch,
base, changed files, state and last update). It reads only
`.ryoko/state/worktrees.sqlite3`, performs no Git or agent action, and offers no
merge or resume control. Press **Refresh** to reload it.

Mission Control shows each task's live phase (Waiting, Thinking, Coding or
Testing), a timestamped agent activity timeline, per-task and run token totals
with estimated ledger cost, validation logs, and a failure explanation naming
the stage that stopped the task. The DAG is read-only and never executes event
payloads; ledger cost is not verified billing.

The desktop app can inspect local agent orchestration events recorded as JSONL.
Use the optional `--event-log .ryoko/events/run.jsonl` flag with a sandboxed
`ryoko run` invocation (and a reviewed `--sandbox-image`). The Mission
Control panel displays task dependencies, attempts, validation results, and
outputs. Logging is disabled by default; logs can contain task descriptions
and should remain private. The desktop reader rejects log files that escape
the selected workspace and limits each displayed log to 4 MiB.

## Provider configuration

```json
{
  "default_provider": "local",
  "providers": [
    { "id": "local", "base_url": "http://127.0.0.1:11434/v1" },
    { "id": "hosted", "base_url": "https://provider.example.com/v1", "api_key_env": "MY_PROVIDER_API_KEY" },
    { "id": "anthropic", "base_url": "https://api.anthropic.com", "api_key_env": "ANTHROPIC_API_KEY", "wire": "anthropic" }
  ]
}
```

Models use the `provider/model-name` syntax. Model names can contain slashes after the first one. The default provider handles unprefixed names.

- HTTPS is required for hosted providers using API keys. Keyless, non-loopback HTTP needs explicit `allow_insecure_http: true` and should only be used on trusted networks.
- Loopback HTTP is allowed for local development. Requests do not follow redirects. Explicitly validate that any URL you add is one you trust; model content must not be allowed to set it.
- Provider profiles are strict JSON and support an optional `chat_path` (default `/chat/completions`). Keys are loaded at runtime from the named environment variables.
- Set `"wire": "anthropic"` for the native Anthropic Messages adapter. Omitting `wire` keeps OpenAI-compatible chat completions. Direct Anthropic URLs work through the adapter; Bedrock/Vertex request signing and cloud identity are not implemented yet.
- The low-level `OpenAiCompatConfig` and `AnthropicConfig` remain available to Rust callers. Those callers are responsible for trusting and validating their own endpoints.

## Architecture

The root workspace is organized as follows:

| Crate | Purpose |
| --- | --- |
| `ryoko-models` | Provider routing, normalized messages and streaming, usage accounting |
| `ryoko-core` | Task DAG, agent lifecycle, IACP messages, memory, permissions and tree-sitter context extraction |
| `ryoko-worktree` | Isolated task Git worktrees and reviewable internal change proposals |
| `ryoko-sandbox` | WASI guardrail plus restricted Docker/Podman pre-flight execution; not a VM |
| `ryoko-cli` | Terminal-facing provider setup, team preview, isolated generation and human review |
| `ryoko-native` | `tower-lsp` language server and agent diagnostic publisher |
| `ryoko-vfs` | Incremental virtual document store used by the language server |
| `ryoko-scripting` | Future plugin layer |

See [the architecture and security audit](docs/ARCHITECTURE_SECURITY_AUDIT.md) and [IACP specification](docs/IACP.md).

## Desktop app

`apps/desktop` contains the native shell: a Tauri v2 window around a Vite + TypeScript frontend using Monaco. It edits a local folder through root-confined Rust commands and talks to `ryoko-lsp` over a stdio-to-event bridge, so open documents are stored in `ryoko-vfs` by the language server. Writes to sensitive paths (`.env*`, CI workflows, credential files) additionally require an OS-native, one-time confirmation before any filesystem mutation; cancellation denies the write. This does not yet gate arbitrary agent shell/network actions, which are not exposed through the current desktop IPC.

    cargo build -p ryoko-native --bin ryoko-lsp
    cd apps/desktop
    npm ci
    npm test
    npm run build
    RYOKO_LSP_BIN="$PWD/../../target/debug/ryoko-lsp" npm run tauri dev

Without `RYOKO_LSP_BIN`, the shell looks for a `ryoko-lsp` binary next to the app executable (the bundled sidecar); it never resolves the language server from `PATH`. Installers are built by the `Desktop installers` workflow on macOS, Windows and Linux runners; they are unsigned until signing secrets are configured. The browser GitHub workbench at `http://127.0.0.1:4173` is unchanged and still served from the files at the desktop root.

## Editor integration

The `ryoko-native` crate provides `ryoko-lsp`, a standard LSP server over stdio. Point VS Code, Neovim, Zed or another LSP client at:

```sh
cargo run -p ryoko-native --bin ryoko-lsp
```

The server supports incremental document sync, diagnostics, code lenses, quick fixes and `ryoko.fix` / `ryoko.explain` / `ryoko.review` commands. An agent host can publish findings through `ryoko_native::diagnostic_channel()`; the server forwards those diagnostics to the editor. It does not start an editor UI or execute editor commands automatically.

`ryoko_core::ContextExtractor` uses tree-sitter to return imports, a symbol outline, intra-file call edges and targeted bodies for Rust, TypeScript/TSX, JavaScript, Python, Go and C/C++. `ryoko run` scans supported source files under the repository root and injects a bounded structural outline into the developer agent's context; the review revise flow includes the same outline alongside the changed file contents.

## Development

```sh
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo test --workspace --locked
```

Security-sensitive project changes should be evaluated in isolated, restricted execution environments and reviewed by a human before integration. **Changes are merged only after explicit human acceptance; the CLI never pushes or deploys.**

## GitHub App workbench (experimental)

Ryoko includes a browser-hosted, Tauri-ready code-inspection and Mission Control shell with a trusted Node.js GitHub App broker. The GitHub App installation and OAuth credentials must be registered by the operator; no GitHub private key is embedded in the desktop frontend.

- One-repository, short-lived installation tokens with separate read and write scopes
- GitHub OAuth login with owner allowlist; repository browsing, PRs, issues and CI status
- HMAC-verified GitHub webhook timeline and optional real Rust worktree session state
- Explicit human-approved clone, branch, commit-file, PR and PR-comment proposals
- Dark/light responsive UI, read-only code inspection and review queue

Requirements: Node.js 22+, Git for server-side clones. After configuring environment variables in the [GitHub App guide](docs/GITHUB_APP.md):

    node services/github-app/src/index.mjs

Open http://127.0.0.1:4173 and connect your GitHub App installation. The initial UX is a browser shell, **not yet a packaged native desktop app or a Monaco/LSP editor**. Nothing auto-merges or deploys. Git clone and existing Rust pre-flight commands should only run inside trusted, properly sandboxed environments.

