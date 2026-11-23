# Contributing to Ryoko

Thank you for helping improve Ryoko. Ryoko is a **pre-alpha, experimental multi-agent AI IDE**, not yet a production-ready or security-certified application. We welcome focused bug reports, documentation improvements, tests, and well-scoped features that strengthen the existing implementation.

Please read [the Code of Conduct](CODE_OF_CONDUCT.md), [the security policy](SECURITY.md), and [the engineering rules](AGENTS.md) before contributing.

## Find or discuss a task

1. Search [existing issues](https://github.com/Flanxiyum001/Ryoko/issues) and [pull requests](https://github.com/Flanxiyum001/Ryoko/pulls) before opening a duplicate.
2. For a significant feature, architecture change, new dependency, or protocol change, open an issue explaining the use case, constraints, and acceptance criteria before implementing it.
3. Prefer a small, reviewable issue or PR over a sweeping rewrite. Do not add placeholder features, unsupported claims, unrelated refactors, or dependency upgrades.
4. **Do not publish vulnerabilities or credentials in public issues.** Follow [SECURITY.md](SECURITY.md) to report security problems privately.

## Local development

Use Rust **1.85 or later** with Cargo. Clone the repository and work on a feature branch (on your fork if you do not have write access):

```sh
git clone https://github.com/Flanxiyum001/Ryoko.git
cd Ryoko
git switch -c fix/short-description

cargo fmt --all -- --check
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo test --workspace --locked
```

The Tauri desktop crate is excluded from the root Cargo workspace. When touching the desktop UI, also run the checks relevant to the changed code from `apps/desktop`:

```sh
cd apps/desktop
npm ci
npm test
npm run build
```

For changes in `services/github-app`, run:

```sh
node --check services/github-app/src/index.mjs
node --test services/github-app/test/
```

Do not assume every contributor can run every platform-specific packaging or integration test. Document skipped checks and the reason rather than claiming they passed. For sandbox- or permission-sensitive work, follow the threat-model and test requirements in [AGENTS.md](AGENTS.md) and `docs/security/`.

## Change requirements

- Read the implementation and the relevant architecture or security documents before changing behavior.
- State the problem and acceptance criteria; add regression tests for bug fixes, and especially for security-sensitive fixes.
- Keep changes narrowly scoped, with clear names and reviewable commits. Avoid unrelated formatting, dependency changes, generated artifacts, and lockfile churn.
- Update user-facing documentation when changing behavior or commands. Keep implementation claims accurate for the current pre-alpha state.
- Treat AI/model output and external tools as untrusted input; verify changes, tests, permissions, and sandbox boundaries yourself.
- Do not put secrets, real API tokens, personal information, or private provider prompts into code, fixtures, logs, screenshots, issues, or PRs.
- Only add code, assets, or dependencies you have the right to contribute, and preserve required third-party licenses and attribution.
- Preserve **your actual Git authorship**. Never impersonate the maintainer or another contributor.

## Submit a pull request

1. Commit to your branch and push to your fork or permitted repository branch.
2. Open a PR targeting `main`; complete the [PR template](.github/PULL_REQUEST_TEMPLATE.md), including the verification results and any security considerations.
3. Reference the relevant issue (e.g. `Fixes #123` when appropriate). Include screenshots or a short recording for visible UI changes.
4. Respond to review feedback. CI success is necessary where checks apply, but it does not replace human review or prove security.
5. Maintainers decide whether and when to accept changes. **Do not merge, release, or deploy without explicit authorization.**

## Licensing and contributions

Ryoko's currently published source code is licensed under the **[Apache License, Version 2.0](LICENSE)**. By submitting a contribution for inclusion, you agree it may be distributed under that license and confirm you have the right to offer it on those terms. This does **not** transfer copyright ownership of your work to the maintainer.

The Apache-2.0 license permits commercial use, including commercial products built using Ryoko, subject to its conditions. Previously released Apache-2.0 code cannot later be made exclusive merely by changing the license on a new release. Future separately developed commercial features or services may use different terms, but changing the licensing of another contributor's copyrighted work may require additional permission. No contributor license agreement or copyright assignment is required by this repository at present.

This contribution policy does not authorize use of the Ryoko name, logos, or other trademarks beyond rights granted by applicable law.
