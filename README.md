# Ryoko

**Ryoko** is an experimental Rust foundation for a provider-neutral, multi-agent AI IDE. The target experience is a full engineering team—project manager, architect, senior UI/UX, backend, full-stack, security, QA and DevOps—collaborating on a user-defined application, with isolated worktrees and human-reviewed changes.

> **Project status:** pre-alpha. The current CLI supports provider setup, chat, **review-only team previews**, and an isolated, human-reviewed file-generation workflow. It does not yet provide an editor UI, hardened project-specific build isolation or deployment. Do not use its previews as proof of successful implementation.

## Prerequisites

- Rust 1.85+ with Cargo
- An OpenAI **chat-completions compatible** server, e.g. an Ollama /v1 endpoint, an OpenAI API endpoint, or another compatible model server
- An API key in an environment variable for authenticated providers

A custom endpoint is not a universal protocol translator. Providers such as Anthropic or Gemini using their native wire APIs will need separate adapters; tools and streaming support differ by model.

## Getting started

Copy `configs/providers.example.json` to your own file, then edit the provider IDs and URLs. The example config supports a local Ollama endpoint on port 11434, OpenAI, and a custom HTTPS endpoint. **Do not put API keys directly in JSON or in git.** Use `api_key_env` to name an environment variable, for example:

```sh
export RYOKO_OPENAI_API_KEY="your-key-from-provider"
cargo run -p ryoko-cli -- providers --config configs/providers.example.json
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

`run` auto-detects common Cargo, npm, Python and Go test commands; use `--check-command` to override it. `review` merges only after explicit human acceptance, removes the temporary worktree on accept or reject, and never pushes or deploys anything. The pre-flight command runs locally and is not isolated by the WASM sandbox, so only run Ryoko in a project you trust.

## Provider configuration

```json
{
  "default_provider": "local",
  "providers": [
    { "id": "local", "base_url": "http://127.0.0.1:11434/v1" },
    { "id": "hosted", "base_url": "https://provider.example.com/v1", "api_key_env": "MY_PROVIDER_API_KEY" }
  ]
}
```

Models use the `provider/model-name` syntax. Model names can contain slashes after the first one. The default provider handles unprefixed names.

- HTTPS is required for hosted providers using API keys. Keyless, non-loopback HTTP needs explicit `allow_insecure_http: true` and should only be used on trusted networks.
- Loopback HTTP is allowed for local development. Requests do not follow redirects. Explicitly validate that any URL you add is one you trust; model content must not be allowed to set it.
- Provider profiles are strict JSON and support an optional `chat_path` (default `/chat/completions`). Keys are loaded at runtime from the named environment variables.
- The low-level `OpenAiCompatConfig` remains available to Rust callers. Those callers are responsible for trusting and validating their own endpoints.

## Architecture

The root workspace is organized as follows:

| Crate | Purpose |
| --- | --- |
| `ryoko-models` | Provider routing, normalized messages and streaming, usage accounting |
| `ryoko-core` | Task DAG, agent lifecycle, IACP messages, memory and permissions abstractions |
| `ryoko-worktree` | Isolated task Git worktrees and reviewable internal change proposals |
| `ryoko-sandbox` | Capability-free WASM test artifact execution, not a general app runner |
| `ryoko-cli` | Terminal-facing provider setup, team preview, isolated generation and human review |
| `ryoko-native`, `ryoko-scripting`, `ryoko-vfs` | Future native, plugin and virtual filesystem layers |

See [the architecture and security audit](docs/ARCHITECTURE_SECURITY_AUDIT.md) and [IACP specification](docs/IACP.md).

## Development

```sh
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo test --workspace --locked
```

Security-sensitive project changes should be evaluated in isolated, restricted execution environments and reviewed by a human before integration. **Changes are merged only after explicit human acceptance; the CLI never pushes or deploys.**
