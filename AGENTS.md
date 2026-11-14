# Engineering rules

Ryoko is a pre-alpha Rust workspace. This file is the contract for every change,
whether it is written by a person or generated with AI assistance.

## Ground rules

1. Never rewrite an existing crate without reading its implementation first.
2. Never create placeholder crates, modules or flags just to satisfy an
   architecture diagram.
3. Every feature needs acceptance criteria and a way to verify them.
4. Security-sensitive changes require regression tests.
5. No model response is trusted executable code. Model output is untrusted input
   that must be parsed, validated and bounded before use.
6. No agent may grant itself permissions. Permissions are granted by the user or
   by a policy file the user controls.
7. Every generated change must be reviewable as a diff before it is integrated.
8. Nothing is merged, pushed, deployed or released without explicit human
   authorization.
9. No credentials in source, logs, prompts, project memory, fixtures or test
   snapshots. Provider keys are read from environment variables or the OS
   credential store.
10. Provider-specific behaviour stays inside its adapter in `ryoko-models`.
11. IPC commands, events and protocol payloads are versioned and typed.
12. Platform-specific filesystem or process behaviour needs a cross-platform
    test or an explicitly documented limitation.

## Required checks

Run these before proposing a change:

```sh
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo test --workspace --locked
```

When `services/github-app` is touched, also run its Node checks:

```sh
node --check services/github-app/src/index.mjs
node --test services/github-app/test/
```

Keep `Cargo.lock` committed and untouched unless a dependency change requires
it. Do not upgrade dependencies as part of an unrelated change.

## Layout

```text
crates/ryoko-cli        terminal entry point and workflows
crates/ryoko-core       domain types, agents, IACP, orchestrator, memory
crates/ryoko-models     provider adapters, routing, streaming, usage
crates/ryoko-native     LSP server and agent diagnostic publisher
crates/ryoko-sandbox    WASM validation guardrail and spec evidence
crates/ryoko-scripting  plugin/scripting layer (not yet wired)
crates/ryoko-vfs        virtual document store for editors
crates/ryoko-worktree   isolated Git worktrees and change proposals
services/github-app     GitHub App broker and workbench (experimental)
configs/                example provider, team and App manifests
docs/                   architecture, protocols, security, product, releases
```

See `docs/architecture/CRATE_RESPONSIBILITIES.md` for ownership boundaries and
the dependency direction rules.

## Documentation

Architecture, protocol, security and release decisions live under `docs/`.
Behaviour changes that affect a documented contract must update that document in
the same change. Use `docs/architecture/` for design, `docs/decisions/` for
architecture decision records, and `docs/releases/` for gates.

## Review

State what you verified and what you did not. "Tests passed" is not a substitute
for saying which tests ran. A passing build does not prove that a UI renders, a
provider responds, or a sandbox isolates.
