# Ryoko architecture and security audit — 2026-11-07

## Scope and evidence

Source review of all eight Rust workspace crates, the CI workflow, the model adapter, task orchestration, IACP messages, sandbox, Git worktree implementation, and existing tests. No running desktop application or production deployment was available to test. Findings are prioritized engineering risks; this is **not** a penetration test or a statement of compliance.

## Current architecture

- `ryoko-models`: normalized chat contracts, OpenAI-compatible streaming, native Anthropic Messages streaming, provider routing and usage ledger. Local and hosted engines that implement either protocol can share the same router.
- `ryoko-core`: agent runtime trait, project/task model, dependency DAG orchestration, events, memory, IACP validation, change validation, workspace abstraction and tree-sitter Rust context extraction.
- `ryoko-worktree`: Git worktree isolation with reviewable internal pull requests and guarded merge APIs.
- `ryoko-sandbox`: Wasmtime harness with WASI imports backed by an empty context, no host preopens, and fuel/epoch/memory limits. This **does not** prove the generated application itself is safe or working.
- `ryoko-cli`: provider checks, one-turn model requests, deterministic team plans and read-only multi-agent previews.
- `ryoko-native`: `tower-lsp` server over stdio with a diagnostic publisher, code lenses, quick fixes and editor commands. It does not yet connect to a running multi-agent session by default.
- `ryoko-vfs`: thread-safe incremental document store used by the language server; `ryoko-scripting` remains a placeholder. No operational desktop UI or general tool-execution layer exists.

### Intended control flow

`user goal -> project manager -> architect -> UX/backend -> senior developer -> security/QA -> DevOps`

Each worker uses `provider/model` from the model router; role contracts must not imply permissions. Planned mutation pathway is `model proposal -> schema/path checks -> isolated workspace -> real project validation -> human approval -> merge/deploy`. The initial CLI **does not** execute that mutation pathway.

## Security findings

| ID | Severity | Area | Evidence and risk | Recommended fix / status |
| --- | --- | --- | --- | --- |
| RY-SEC-01 | High | Provider URL/credentials | Unrestricted `OpenAiCompatConfig` accepts caller-specified endpoints and headers; remote cleartext HTTP could expose credentials. Redirects can change destination and error bodies might echo secrets. | Added validated user-owned profiles, environment-only API keys, HTTPS required for non-loopback keyed requests, no HTTP redirects and redacted HTTP errors. Direct low-level adapter still requires trusted caller input. |
| RY-SEC-02 | High | Filesystem | Relative-path validation previously allowed `.git` / `.ryoko` metadata targets and platform-specific prefixes. | Hardened path validation and added regression tests. |
| RY-SEC-03 | High | Filesystem TOCTOU | `GitWorktreeLease::apply` canonicalizes the parent then writes by path; another actor could race a symlink switch between checks and write. | Open: move to directory-handle-relative, no-follow atomic writes (and treat model-created content as untrusted). |
| RY-SEC-04 | High | Validation correctness | `SandboxValidator::static_module` can pass on an unrelated WASM module; an agent's submitted source is not demonstrably compiled, tested, scanned or executed. `AllowAllValidator` is intentionally unsafe for production. | Open: bind attestations to exact staged content and run language-specific build/lint/tests in a real restricted runner; fail closed otherwise. |
| RY-SEC-05 | High | Network / tool execution | IACP has a tool allowlist, but a future terminal and provider bridge needs explicit, enforceable policies against exfiltration, prompt injection, arbitrary outbound calls and destructive commands. | Open: per-workspace capability policy, consent UI, network isolation and command audit. |
| RY-SEC-06 | Medium | Streaming / memory | SSE buffering and output aggregation were not tightly bounded. | Added per-line upper bound and overall timeout; full response, tool argument and entire conversation budgets still need enforcement. |
| RY-SEC-07 | Medium | Cost / quota | Usage ledger sees usage after completion; parallel requests can overrun a shared budget and local providers may omit usage. | Open: reservation-based admission, max output and per-role quotas, cancellation and cost controls. |
| RY-SEC-08 | Medium | AI output correctness | Providers that do not return well-formed or complete tool-call fragments can create partially decoded results. | Open: strict per-provider streaming conformance and tool schema validation before dispatch. |
| RY-SEC-09 | Medium | Secrets management | There is no OS keychain/secure desktop settings UI. Environment variables are better than committing keys but remain visible to the owning process and its children. | Open: OS credential store + redaction + access compartmentalization. |
| RY-SEC-10 | Medium | Product readiness | Desktop shell, editor, virtual filesystem, scripting and native host are not implemented in this snapshot. | Open: staged implementation with explicit user journeys and real integration tests. |

## Design decisions for the IDE

1. **Model adapters:** Keep core messages provider-neutral. OpenAI-compatible and native Anthropic Messages adapters now exist; Gemini, Bedrock/Vertex signing and other non-compatible APIs still need dedicated adapters with contract tests. Never claim every API speaks the same protocol.
2. **Agent workers:** Register role-specific prompts, model selection, granted capabilities and maximum token/cost budgets. Use deterministic DAG and IACP envelopes for handoffs, not free-form unsafe tool instructions.
3. **Workbench:** Desktop app with Monaco/editor, file tree, diff/patch preview, logs, approvals and role timeline. Separately isolate the trusted desktop host from the untrusted model output renderer. The LSP server is the first editor bridge, not a replacement for the desktop host.
4. **Execution:** Run project builds and tests in restricted containers or ephemeral VMs with explicit allowlisted network and mounts. WASM is useful for narrowly defined plugins, not a generic guarantee about arbitrary generated applications.
5. **Review:** Every change needs a diff, passed project-specific tests and an explicit human approve/merge action. Never conflate an internal worktree PR object with a hosted GitHub PR.
6. **Provider UX:** Endpoint, protocol type, model ID, key storage, latency/health tests, streaming/tools capability tests and per-role binding. Remote HTTP without TLS is not suitable for secrets.

## Release gates

- **Gate A — foundation:** provider configs, basic CLI, named roles, documented risks, unit tests, `cargo fmt`, Clippy and workspace test suites.
- **Gate B — real coding:** model-backed implementation agent, file-aware context retrieval, bounded JSON patch schema, isolated worktrees, content-bound project test attestations, explicit approval.
- **Gate C — desktop IDE:** editor UI, local files, diff viewer, permission manager, keychain, progress and cancellation.
- **Gate D — production:** adversarial prompt-injection tests, symlink-race tests, SSRF and DNS rebinding policy, audit logs, budget reservations, cross-platform builds and signed releases.

## Validation commands

```sh
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo test --workspace --locked
```

Do not mark Gate A passed until these commands are green in CI. This audit is reviewable and must not be interpreted as authorization for production deployments.
