# Ryoko GitHub App and Workbench (experimental)

This integration adds a **trusted GitHub App broker** and a **browser-hosted, Tauri-ready IDE workbench** with Agent Mission Control. It does **not** register the GitHub App automatically, package a native binary, merge code or deploy applications.

## 1. Register the GitHub App

Visit GitHub Settings → Developer settings → GitHub Apps → New GitHub App. Create an App with:

- Name: Ryoko Engineering (or an available equivalent).
- Homepage URL: your HTTPS service URL.
- OAuth callback URL: https://YOUR_HOST/auth/github/callback
- Webhook URL: https://YOUR_HOST/webhooks/github, with a strong random secret.
- Installation: select only the repositories Ryoko needs.
- Repository permissions: Contents read/write, Pull requests read/write, Issues read/write, Actions read, Checks read, Metadata read.
- No Workflows or Administration permissions.
- Webhook subscriptions: installation, installation_repositories, pull_request, pull_request_review, issues, issue_comment, check_run, check_suite, workflow_run, push.

For local tests, set http://127.0.0.1:4173 as your public origin and OAuth callback. GitHub webhooks cannot reach loopback, so production webhooks need a public HTTPS endpoint and a trusted TLS proxy.

GitHub App installation tokens are restricted to exactly one authorized repository ID and either read or write permissions per request. They are short-lived and kept in backend process memory. The user must also authenticate via OAuth and be included in the explicit allowed-login list.

## 2. Configure the trusted backend

Node.js 22+ and Git are required; the backend has no third-party npm dependencies.

Generate/download your GitHub App private key into a location outside the repository:

    chmod 600 /secure/ryoko-app.private-key.pem

Set environment variables (never commit credentials):

    export GITHUB_APP_ID="123456"
    export GITHUB_APP_SLUG="ryoko-engineering"
    export GITHUB_CLIENT_ID="Iv1.your-client-id"
    export GITHUB_CLIENT_SECRET="<server-only-secret>"
    export GITHUB_PRIVATE_KEY_FILE="/secure/ryoko-app.private-key.pem"
    export GITHUB_WEBHOOK_SECRET="<server-only-secret>"
    export RYOKO_ALLOWED_USERS="your-github-login"
    export RYOKO_PUBLIC_ORIGIN="http://127.0.0.1:4173"
    export HOST="127.0.0.1"
    export PORT="4173"
    node services/github-app/src/index.mjs

Open http://127.0.0.1:4173, choose Connect GitHub, install the App on selected repositories, and choose an installation/repository. The frontend never receives the App private key or installation tokens. OAuth user tokens are stored only in backend memory, not browser storage. Sessions use HttpOnly, SameSite=Lax cookies (also Secure under HTTPS).

The broker disables OAuth login unless RYOKO_ALLOWED_USERS is populated. For public hosting use HTTPS and restrict access at the reverse proxy. This service does not terminate TLS itself. The in-memory sessions, proposals and webhook events are not durable across restarts; production requires a persistent encrypted session store, background delivery processing, token rotation and rate limiting.

## 3. Connect the Rust CLI for real Mission Control worktree status

When the broker runs on the same trusted machine:

    export RYOKO_CLI_BIN="/absolute/path/to/ryoko"
    export RYOKO_PROJECT_ROOT="/absolute/path/to/your/trusted/project"

Mission Control invokes the fixed, read-only Rust command with argument-array execution: ryoko sessions --repo PROJECT_ROOT --json, with timeout and bounded output. This reports active/published/merged/abandoned worktrees, not per-agent inference events. Other roles are explicitly labeled not connected; no agent progress is fabricated. GitHub webhook timeline events are authenticated by HMAC and filtered to user-accessible installations.

## 4. Controlled repository operations

From the Workbench approval form, a signed-in user can queue a proposal and then explicitly approve or reject it. A trusted host-side agent bridge may also queue requests without executing them. Configure:

    export RYOKO_AGENT_BROKER_TOKEN="<32-plus-random-characters>"
    export RYOKO_AGENT_ALLOWED_REPO_IDS="123456789"

Send POST /api/agent/proposals with Authorization: Bearer YOUR_BROKER_TOKEN and JSON:

    {
      "installationId": 1111,
      "repoId": 123456789,
      "operation": "create_branch",
      "params": { "branch": "ryoko/fix-123" }
    }

**Supported operations, after explicit approval:**

| Operation | Parameters | Outcome |
| --- | --- | --- |
| clone_repository | projectName | Clone to a new server-side project directory using read-only GitHub permission |
| create_branch | branch | Create only a ryoko/ feature branch from the default branch |
| commit_file | branch, path, content, message, optional expectedSha | Commit UTF-8 content on an isolated branch only |
| open_pull_request | head, title, body | Open PR from ryoko/ into default branch |
| review_comment | number, comment | Leave a non-approval comment on a PR |

Cloning requires a trusted absolute directory on the **server** (not the browser):

    export RYOKO_PROJECTS_ROOT="/srv/ryoko/projects"

The clone uses a short-lived installation token via a temporary owner-only Git askpass executable, with no token in Git command-line arguments, checkout remotes or frontend. Temporary helper is deleted. Cloned code remains untrusted and must not be run outside a restricted worker. Do not enable server-side cloning on a public shared machine.

The broker blocks workflow edits, Git metadata changes, credential files, default-branch direct commits, administration, and automatic merges. GitHub branch protection and rulesets also continue to apply. A proposal expires after 10 minutes; approval requires a valid logged-in session, same-origin CSRF header, and a fresh authorization check. Failed writes are never silently retried.

## 5. Desktop workbench

The web shell provides repository installation selection, read-only file browser, branches/issues/PRs/CI, dark/light appearance, reduced motion, a persistent sidebar, a Mission Control board with eight agents, a verified GitHub event timeline, and a user approval queue. It is **not yet packaged as a Tauri/native desktop binary**, and the file panel is a read-only inspector rather than a Monaco editor.

**Still required for the full product:** native desktop packaging, LSP/Monaco editor, terminal, live per-agent inference events, durable audit/job queue, native OAuth token refresh, a first-party Rust agent bridge submitting proposals, bulk patch staging, and staged deployment with proper production controls.

## 6. Tests and release gates

CI runs Rust workspace formatting/Clippy/tests and the Node workbench syntax/security tests. Locally:

    cd services/github-app
    npm run check
    npm test

Automated coverage includes webhook HMAC and replay defense, RS256 App JWT, scoped tokens, OAuth state and account allowlists, CSRF, operation path restrictions, one-time approval, and secret-free Git clone arguments.

After registering the real App, perform manual staging integration against a disposable repository: OAuth login, selected repo read, clone, branch, commit, PR, CI read and webhook delivery. Do not approve production access before this is complete.
