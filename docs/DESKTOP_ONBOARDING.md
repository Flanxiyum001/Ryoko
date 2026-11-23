# Desktop onboarding and welcome hub

Ryoko's first launch uses the native Tauri desktop backend, not browser persistence.

## Guided flow

1. Choose **OpenAI**, **Anthropic**, **Gemini**, or **Ollama**.
2. Pick a model ID. Hosted providers require an API key; Ollama uses the local server at `127.0.0.1:11434` and does not require a key.
3. Select a folder or clone a public HTTPS Git repository using a local Git installation.
4. Review the suggested starter task in Mission Control. The run does **not** start automatically: use **Generate image** to prepare a supported sandbox if needed, then select **Run** after checking its goal, budget and sandbox settings. Review agent diffs before applying them.

The **Configure your AI provider** welcome action can be used to change the provider later. Dismissing onboarding still allows local editing without a provider.

## Secret handling

The credential is sent from the password field to the native `onboarding_configure` command and stored with the system credential service, keyed by provider. It is not saved in `.ryoko/desktop/providers.json`, the machine-wide `desktop-setup.json`, localStorage, a Git remote URL, or the console. The native runtime retrieves it on demand.

- **Windows:** Credential Manager (persistent local-machine generic credential).
- **macOS:** system Keychain generic password.
- **Linux:** a running freedesktop Secret Service implementation and the `secret-tool` binary (typically `libsecret-tools`) must be available and unlocked. Setup fails closed if the service is unavailable.
- **Other platforms:** no plaintext fallback. Provider configuration is rejected if native credential storage is unsupported.

Mission Control starts the `ryoko` CLI as a child process. The desktop shell creates a private, per-run provider catalog containing **only the API key environment variable name** (never its value) and passes the key from the OS vault in the child's process environment. Its `--config` argument points at that catalog. Child process environments can be inspected by the same OS user or elevated processes, so consider the local user account trusted. The CLI never receives the key as a command-line argument. Existing explicit `--config` choices override onboarding, as does a workspace provider catalog.

The global model choice takes precedence over environment-only fallback. An explicitly configured workspace `.ryoko/desktop/providers.json` still takes precedence over global onboarding preferences, and workspace catalogs continue to resolve credentials from environment-variable names.

The machine-wide configuration is limited to the provider ID, model ID, setup-complete flag and up to eight recently opened paths. No API key is written there. If a user removes the credential from their OS vault, model calls fail with instructions to configure the provider again. To rotate a key, reopen provider setup and save the replacement.

## Clone behavior

The clone dialog accepts HTTPS URLs only, rejects embedded username/password, query and fragment components, and derives a safe folder name. Ryoko launches `git clone --depth 1` without a shell. The user chooses the destination directory. Interactive Git credentials are disabled; private repositories requiring authentication need an authenticated integration and are not part of this initial flow.

## Brand assets

The source-of-truth vector is `apps/desktop/ui/public/ryoko-mark.svg`. Renderer welcome, header and splash all use it. Run `npm run icons` from `apps/desktop` to regenerate platform PNG/ICO/ICNS assets. Signed and unsigned installer workflows generate the native icon set before packaging.

## Validation / manual acceptance

- Run `npm ci && npm test && npm run build && npm run test:ui` inside `apps/desktop`.
- Run `cargo fmt --manifest-path apps/desktop/src-tauri/Cargo.toml -- --check`, `cargo check --manifest-path apps/desktop/src-tauri/Cargo.toml --locked`, and the desktop shell tests.
- On each supported OS, confirm that the key exists in the system vault, can be rotated, is not echoed in logs, and that a real provider request works.
- Confirm missing/unlocked keychain errors, workspace-switch confirmation with unsaved files, cloning a public repo, offline editing without setup, reduced-motion animations, and the draft/approval process for the starter task.

Browser acceptance tests mock IPC and do **not** prove native keychain, clone or provider network behavior. Cross-platform manual verification is required before declaring the feature production-ready.
