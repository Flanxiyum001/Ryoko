# Ryoko GitHub App and Workbench (experimental)

This integration adds a **trusted GitHub App broker** and a **browser-hosted, Tauri-ready IDE workbench** with Agent Mission Control. It does **not** register the GitHub App automatically, package a native binary, merge code or deploy applications.

Use [the least-privilege App manifest template](../configs/github-app-manifest.example.json) as a registration reference. Replace YOUR_HOST before registering. For this prototype, leave request_oauth_on_install disabled and use the explicit Connect GitHub button, which creates a per-session OAuth state first.

## 1. Register the GitHub App

Visit GitHub Settings → Developer settings → GitHub Apps → New GitHub App. Create an App with:

- Name: Ryoko Engineering (or an available equivalent).
- Homepage URL: your HTTPS service URL.
- OAuth callback URL: https://YOUR_HOST/auth/github/callback
- Webhook URL: https://YOUR_HOST/webhooks/github, with a strong random secret.
- Installation: select only the repositories Ryoko needs.
- Repository permissions: Contents read/write, Pull requests read/write, Issues read/write, Actions read, Checks read, **Commit statuses read**, Metadata read.
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

The host-side adapter can queue an agent proposal from a JSON file without putting the bearer token in command arguments:

    export RYOKO_GITHUB_BROKER_URL="http://127.0.0.1:4173"
    node services/github-app/src/agent-client.mjs /secure/path/to/proposal.json

Only a trusted tool host should run this adapter. The model must never be given the broker token, its process environment or arbitrary terminal execution privileges. This adapter is **not automatically wired** to the Rust ModelAgent runtime.

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

## 5. Issue-to-PR agent pipeline

A signed GitHub `issues.labeled` webhook for the **newly applied** `ryoko-fix` label
(or the older `ryoko:agent`) can start the full team pipeline. An unrelated label
change or an issue-open event does **not** authorize an agent run. The repository must be listed in
`RYOKO_AGENT_ALLOWED_REPO_IDS`, and the server also needs the CLI and clone root:

    export RYOKO_CLI_BIN="/absolute/path/to/ryoko"
    export RYOKO_PROJECTS_ROOT="/srv/ryoko/projects"
    export RYOKO_AGENT_ALLOWED_REPO_IDS="123456789"

The broker then:

1. clones the repository with a short-lived, read-only installation token;
2. runs `ryoko run --team --repo <clone> --report <clone>/.ryoko/issue-report.json`
   with the issue title and body as the goal;
3. reads the report's published branch, token ledger and validation events;
4. pushes that branch with a separate contents-write token; and
5. opens a **draft** pull request whose body embeds the token ledger, the
   pre-merge security audit and the validation evidence.

### Ryoko GitHub bot commands (experimental)

Once you **register and install the GitHub App** and configure the trusted
broker, an authorized maintainer can comment on an **issue** with one command
on its own line:

| Command | Behavior |
| --- | --- |
| `/ryoko run` | Start one issue-to-draft-PR job, unless this issue already has a recorded job |
| `/ryoko status` | Ask the bot to comment with its in-memory running/completed/failed status |
| `/ryoko retry` | Explicitly rerun a failed job after investigating any partial GitHub writes |

For safety, the command parser accepts only exact commands on `issue_comment.created`
events, **not** pull request comments or edited comments. The webhook must have a
valid HMAC, the GitHub event `sender.login` must match the comment author, and
the account must be in `RYOKO_ALLOWED_USERS`. This is a hard operator allowlist,
not permission for anyone who can comment in the repo. Only repository IDs in
`RYOKO_AGENT_ALLOWED_REPO_IDS` can run or get status responses.

The bot posts a start comment, then either a draft PR link or a generic failure
message to the issue. It obtains a **separate installation token scoped to that
repository with only `issues:write`** for comments; the pipeline continues to
use separately scoped clone, contents and PR tokens. The bot never merges,
approves, deploys, writes directly to the default branch or edits GitHub workflows.

**Operational limitation:** jobs and delivery deduplication are held in process
memory. Restarting the broker loses job history, and multiple replicas can
double-run requests. Do **not** enable this on valuable production repositories
until a durable single-consumer queue and reconciliation/idempotency for partially
pushed branches and PRs are implemented. A failed job may already have pushed a
branch before it failed; inspect GitHub before using `/ryoko retry`. Run staging
against a disposable repository first.

### MCP tools in the pipeline

The pipeline child process never has an approval prompt, so MCP is opt-in and
allowlist-bounded. Without `RYOKO_MCP_SERVER` the run gets no MCP server at all:

    export RYOKO_MCP_SERVER="/srv/ryoko-mcp"
    export RYOKO_MCP_SERVER_ARGS='["--stdio"]'
    export RYOKO_MCP_ALLOW_TOOLS="list_issues,get_issue"
    export RYOKO_MCP_HEADLESS_APPROVE="list_issues"
    export RYOKO_MCP_MAX_CALLS="20"

`RYOKO_MCP_HEADLESS_APPROVE` is the only approval a headless run can obtain.
Every tool it does not name is denied, and each approved tool must also be on
`RYOKO_MCP_ALLOW_TOOLS` or the run fails before connecting. See
[MCP_CLIENT.md](architecture/MCP_CLIENT.md#headless-policy) for the full rules.

The job is started asynchronously after the webhook is accepted, emits
`pipeline` timeline events, and is deduplicated in memory by repository/issue.
Jobs are not durable across restarts. Model keys stay in the service
environment; they are never included in the webhook, clone, report or PR body.
Cloned code and model output remain untrusted, and the PR is never merged
automatically. Docker/container audit and worktree behavior follow the CLI's
normal isolation configuration.

## 6. Desktop workbench

The web shell provides repository installation selection, read-only file browser, branches/issues/PRs/CI, dark/light appearance, reduced motion, a persistent sidebar, a Mission Control board with eight agents, a verified GitHub event timeline, and a user approval queue. It is **not yet packaged as a Tauri/native desktop binary**, and the file panel is a read-only inspector rather than a Monaco editor.

**Still required for the full product:** native desktop packaging, LSP/Monaco editor, terminal, live per-agent inference events, durable audit/job queue, native OAuth token refresh, a first-party Rust agent bridge submitting proposals, bulk patch staging, and staged deployment with proper production controls.

## 7. Native desktop Mission Control (stacked prototype)

The Tauri IDE's **GitHub Mission Control** panel can now do more than
read-only browsing. First launch the Node broker on the **same trusted
machine**, sign in with an allowlisted GitHub user in the browser workbench,
and use **Pair native desktop** to generate a single-use pairing code.
Paste this code in Ryoko's native GitHub panel. The OAuth, App key and
installation credentials remain entirely in the Node broker; the 30-minute
paired bearer token is held only inside Rust, never the webview.

In the native panel you can:

- Browse an authorized repository's issues and PRs and inspect fresh PR CI.
- Explicitly start an issue engineering job with **Run agent** (only for
  repository IDs in `RYOKO_AGENT_ALLOWED_REPO_IDS`).
- Queue an explicitly reviewed local clone into `RYOKO_PROJECTS_ROOT`,
  safe `ryoko/` branch creation, UTF-8 file commits, **draft** PR
  creation and review comments, or promote a draft PR to Ready for review.
- Inspect the **full parameters** of pending proposals (including trusted
  agent proposals) and deliberately **Approve write** or **Reject**. Each native **Approve**
  requires a second, OS-owned confirmation (not only a webview dialog). Merely
  queuing never executes a GitHub mutation; approvals are one-shot and
  re-authorize the repository at execution time.

The `mark_ready_for_review` operation fails closed unless the draft PR
belongs to the selected repository, its head branch starts with `ryoko/`,
all GitHub check runs and commit statuses for the current HEAD are complete
and passing, at least one check has succeeded, and the HEAD is unchanged
on immediate reinspection. Status data is advisory and potentially
time-sensitive; **GitHub branch rulesets must still enforce checks and
reviews on the default branch**. Ryoko never merges PRs or bypasses rulesets.

### Durable bot job ledger

For any real bot installation configure a private persistent file on a
single trusted host:

    mkdir -p /srv/ryoko/private
    chmod 700 /srv/ryoko/private
    export RYOKO_BOT_STATE_FILE="/srv/ryoko/private/github-bot.json"

The broker writes owner-only JSON snapshots atomically (0600 mode).
It records verified webhook delivery IDs and per-issue job states.
When restarted, previously running jobs become `interrupted`. Ryoko
**never automatically retries ambiguous remote writes**, even after
failure; inspect any pushed branch or draft PR before the explicit
`/ryoko retry` command. An operator can also inspect jobs from the IDE.

This ledger is **single-process only**. Multiple broker replicas, network
filesystems, or automated crash recovery are not supported. A full
production service needs transactional storage, worker leasing, outcome
reconciliation and durable approval queues. Desktop approval proposals,
OAuth sessions and bearer pairings are still in process memory. Never put
the ledger or GitHub credentials in the repository.

### Registration and staging checklist

1. Register the GitHub App using the manifest template; install it on **only
   a disposable test repository** first. Enable Commit statuses: read.
2. Supply the backend private key, OAuth, webhook secret, owner allowlist,
   repository ID allowlist, model provider config, local Ryoko CLI and
   sandbox image. Keep these variables outside the UI and repository.
3. Configure the private ledger path, start a **single** broker, sign in
   and pair the native desktop.
4. Try a read, a rejected proposal, a branch proposal and an explicitly
   approved draft PR. Verify the CI gate blocks a pending or failing commit.
5. Start a small issue job, inspect its progress and verify the draft PR
   never merges. Restart the broker mid-job and ensure it reports
   `interrupted`, with no automatic second run.

## 8. Tests and release gates

CI runs Rust workspace formatting/Clippy/tests and the Node workbench syntax/security tests. Locally:

    cd services/github-app
    npm run check
    npm test

Automated coverage includes webhook HMAC and replay defense, RS256 App JWT, scoped tokens, OAuth state and account allowlists, CSRF, exact maintainer-only issue bot commands, webhook job deduplication, operation path restrictions, one-time approval, and secret-free Git clone arguments.

After registering the real App, perform manual staging integration against a disposable repository: OAuth login, selected repo read, clone, branch, commit, PR, CI read and webhook delivery. Do not approve production access before this is complete.
