# Ryoko

**Ryoko** is an experimental Rust foundation for a provider-neutral, multi-agent AI IDE. The target experience is a full engineering team—project manager, architect, senior UI/UX, backend, full-stack, security, QA and DevOps—collaborating on a user-defined application, with isolated worktrees and human-reviewed changes.

> **Project status:** pre-alpha. The current CLI supports provider setup, chat and **review-only team previews**. It does not yet provide an editor UI, autonomous source-code application, secure project-specific build runner or deployment. Do not use its previews as proof of successful implementation.

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

The preview uses the same model for all eight roles; per-role model selection is a planned follow-up. It prints proposed architecture, designs, engineering guidance, security review, QA and operations plans. It never writes generated files or runs code. No hosted GitHub pull requests are produced by this command.

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
| `ryoko-cli` | Terminal-facing provider and team preview |
| `ryoko-native`, `ryoko-scripting`, `ryoko-vfs` | Future native, plugin and virtual filesystem layers |

See [the architecture and security audit](docs/ARCHITECTURE_SECURITY_AUDIT.md) and [IACP specification](docs/IACP.md).

## Development

```sh
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo test --workspace --locked
```

Security-sensitive project changes should be evaluated in isolated, restricted execution environments and reviewed by a human before integration. **No automatic merges or production deployments are performed by the CLI.**
