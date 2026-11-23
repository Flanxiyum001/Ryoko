# Native GitHub integration

Status: implemented in the desktop shell. `crates/ryoko-github` owns sign-in,
repository access and checkout sync; `ryoko-models` owns the Copilot request
shapes; `apps/desktop` is a thin command layer plus one sidebar panel.

The earlier [GitHub App broker](../GITHUB_APP.md) still exists and is unchanged.
It is a server-side, multi-user installation broker. This page describes the
**single-user, local** path a desktop IDE needs: no server, no client secret, no
repository installation step.

## What one sign-in gives you

| Capability | Command | Notes |
| --- | --- | --- |
| Sign in | `github_sign_in_start`, `github_sign_in_poll` | OAuth device flow: the user approves in their own browser. |
| Use an existing credential | `github_connect_existing` | `RYOKO_GITHUB_TOKEN`, or the token Git's credential helper already holds for `github.com`. |
| Sign out | `github_sign_out` | Clears the session and erases the helper entry. |
| Status | `github_status` | Login, token source and whether it survives a restart. No token. |
| Repositories | `github_repositories` | Owner, collaborator and organisation repositories, most recently updated first, capped at 500. |
| Clone | `github_clone_repository` | `parent/<name>`, never overwrites, then opens the clone as the workspace. |
| Sync state | `git_sync_status` | Branch, upstream, ahead/behind, dirty, remote URL. |
| Sync | `git_sync` | `fetch`, fast-forward-only `pull`, `push`. |
| Copilot | `github_copilot_models`, `github_use_copilot` | Lists the models the account may use, then writes a validated provider catalog and default model on explicit action. |

## Authentication

Device flow is the right grant for a binary that cannot keep a client secret.
`RYOKO_GITHUB_CLIENT_ID` names the **operator's own OAuth App client ID**; Ryoko
never ships or borrows one.

    # Register an OAuth App: Settings → Developer settings → OAuth Apps
    # Callback URL is not used by the device flow; any HTTPS URL is accepted.
    export RYOKO_GITHUB_CLIENT_ID="Iv1.your-own-client-id"

Without that variable the panel still offers **Use existing credential**, which
covers the common cases:

* `RYOKO_GITHUB_TOKEN` set in the environment (headless, CI, a wrapper script);
* a token Git already stores for `github.com` — the same credential an existing
  `git push` over HTTPS uses. This is why a developer who already pushes with
  Git is signed in on first launch.

Requested scopes are `read:user repo`. There is deliberately no `workflow`
scope: the IDE must not be able to rewrite a repository's Actions pipelines.

## Where the token lives

* In memory for the session (`GitHubAccountState`).
* For persistence, in the host's Git credential helper
  (`git credential approve|fill|reject`), which is the OS-backed store Git and
  the GitHub CLI already use: macOS Keychain, Windows Credential Manager,
  libsecret on Linux.

The token is written to the helper on **stdin**, never in an argument, a remote
URL or a file Ryoko owns. Nothing is written into the workspace, an event log or
an IPC payload; the only status that leaves the shell is login, source and
persistence.

Consequences, stated rather than hidden:

* A host with **no credential helper** stores nothing. Sign-in still works and
  the panel says the session is not persisted.
* If the helper is Git's plaintext `store` helper, the credential is kept where
  that helper keeps it. That is the user's existing Git configuration.
* Every helper call pins `GIT_TERMINAL_PROMPT=0` and `GCM_INTERACTIVE=never`, so
  a lookup can never open a prompt or block a window.

## Cloning and syncing

Git is always invoked with an explicit argv — no shell — and bounded output.

* **Clone** looks the repository up in the account's own list instead of
  trusting a URL from the frontend, refuses an existing destination, and only
  accepts an `https://github.com/...` remote.
* **Pull** always fetches first, then `git merge --ff-only`. A diverged branch
  is refused with an explanation; Ryoko never invents a merge commit and never
  rebases on the operator's behalf.
* **Push** never forces and never rewrites history. A branch without an upstream
  is published as `origin/<branch>` with `--set-upstream`.
* The token is handed to Git through an ephemeral `credential.helper` definition
  plus the `RYOKO_GIT_TOKEN` environment variable, and only when the remote is
  `https://github.com/...`. SSH, local and third-party remotes keep their own
  credential configuration.
* Git's own output is truncated and the token is replaced with `<redacted>`
  before it reaches a status line.

## GitHub Copilot

Copilot speaks the OpenAI chat-completions wire protocol with a different
credential layer. The adapter lives in `ryoko-models::copilot`:

1. a **fine-grained personal access token** with the account permission
   `Copilot requests` (`github_pat_...`) is sent to Copilot directly;
2. an **OAuth user token** (`gho_...` / `ghu_...`) is exchanged at
   `api.github.com/copilot_internal/v2/token` for a ~30 minute bearer, which also
   names the API host for the account's plan. The bearer is cached in memory and
   refreshed 60 seconds before expiry, and a 401 invalidates the cache.

`github_copilot_models` is the honest availability check: an empty list means the
account has no Copilot seat, and the panel then hides the controls.

Selecting **Use for this workspace** writes `.ryoko/desktop/providers.json` with
one validated profile:

    {
      "default_provider": "github-copilot",
      "providers": [{
        "id": "github-copilot",
        "base_url": "https://api.githubcopilot.com",
        "api_key_env": "RYOKO_GITHUB_TOKEN",
        "wire": "copilot"
      }]
    }

and records `github-copilot/<model>` as the workspace default model. The signed-in
session supplies the credential in-process through
`ProviderCatalog::build_router_for_models_with_secrets`, so the token is **not**
placed in the process environment. The CLI child started by Mission Control
receives `RYOKO_GITHUB_TOKEN` only in its own environment so that the same
catalog works there.

**Limitation, stated plainly:** the token-exchange endpoint and the required
editor-identity headers are not part of a published GitHub API contract; they
mirror GitHub's own client and can change without notice. The documented,
stable option is the fine-grained token with `Copilot requests`. When GitHub
refuses the exchange, the error names that option instead of guessing.

## Verification

`crates/ryoko-github` (26 tests, no network, no credential):

* device-flow start and poll parsing, hostile verification URLs, token-type and
  lifetime bounds, redacted `Debug` for every secret-bearing type;
* repository metadata validation (off-site URL, path traversal, owner/name
  disagreement) and paging;
* the Git credential helper round trip through a scripted helper, including
  erase and "the token never appears on the command line";
* real Git repositories for status, push, fast-forward pull, a dirty checkout,
  a diverged branch, and a clone destination that must not be overwritten.

`crates/ryoko-models`:

* Copilot exchange parsing (expiry, per-plan host, hostile values), refresh
  margin, fine-grained-token detection, model-id filtering, redacted diagnostics,
  and catalog validation for the `copilot` wire.

`apps/desktop`:

* `settings` writes and re-reads the default model and rejects traversal;
* `model_runtime` resolves a saved default model;
* the UI tests cover the sync summary and account status text.

Not covered by automated tests: a real GitHub OAuth approval, a real Copilot
subscription, and the Keychain/Credential-Manager/libsecret helpers on each
platform. Those need a disposable account and a manual pass; a passing build
does not prove them.
