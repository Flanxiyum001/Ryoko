# ADR-016: Main branch protection ruleset and single-maintainer review policy

Status: accepted
Date: 2026-11-23

## Context
Prior to this policy, `main` reported `protected: false` and the repository had no active rulesets. Written guidance in `CONTRIBUTING.md` and `AGENTS.md` prohibited unreviewed merges, but technical enforcement was missing, permitting accidental direct pushes or unreviewed merges.

In a single-maintainer repository where Pourush Nair (`Flanxiyum001`) is the sole administrator, GitHub's branch protection introduces a structural constraint: PR authors cannot approve their own pull requests. Requiring an approving review from a second party without a co-maintainer would deadlock self-authored maintenance and feature PRs.

## Decision
1. **GitHub Repository Ruleset (`protect-main`):**
   - **Target:** `refs/heads/main`
   - **Enforcement:** `active`
   - **Branch Deletion:** Blocked (`type: deletion`).
   - **Force Pushes:** Blocked (`type: non_fast_forward`).
   - **Pull Request Requirement:** All updates to `main` must arrive via pull request (`type: pull_request`). Direct pushes to `main` are rejected.
   - **Required Status Checks:** Exactly 7 verified stable CI checks must complete successfully:
     - `Rust` (`.github/workflows/ci.yml`)
     - `Desktop UI / Tauri shell` (`.github/workflows/ci.yml`)
     - `Tauri shell` (`.github/workflows/ci.yml`)
     - `GitHub App / Desktop Workbench` (`.github/workflows/ci.yml`)
     - `Native tests / ubuntu-24.04` (`.github/workflows/platform-integration.yml`)
     - `Native tests / windows-latest` (`.github/workflows/platform-integration.yml`)
     - `Native tests / macos-latest` (`.github/workflows/platform-integration.yml`)
   - **Strict Status Checks Policy:** Enabled (`strict_required_status_checks_policy: true`), requiring branches to be up to date with `main` before merging.
   - **Conversation Resolution:** Required (`required_review_thread_resolution: true`).
   - **Dismiss Stale Reviews on Push:** Enabled (`dismiss_stale_reviews_on_push: true`).
   - **Bypass Actors:** Empty (`bypass_actors: []`). Even repository administrators cannot casually bypass required status checks or push directly.
2. **Single-Maintainer Approval & Merge Policy:**
   - Auto-merge remains strictly disabled (`allow_auto_merge: false`).
   - All PRs require the maintainer's explicit manual review and authorization before merging.
   - For external contributions, the maintainer provides the required code review and explicitly merges the PR.
   - For maintainer-authored PRs, the PR gate, branch update requirements, and all 7 CI checks are strictly enforced; self-approvals are not supported by GitHub, so author merge authorization serves as explicit human review once all required gates pass.
   - When peer approvals become mandatory in future development phases, a second trusted co-maintainer or bot reviewer must be designated.

## Alternatives Considered
- *Classic branch protection without rulesets:* Rulesets provide atomic configuration, auditability via GitHub API, and explicit bypass actor control without accidental admin bypasses.
- *Strict required_approving_review_count: 1 without co-maintainer:* Deadlocks self-authored PRs on GitHub since authors cannot approve their own PRs.
- *Permissive administrator bypass:* Rejected; breaks the core principle that no change reaches `main` without validated CI evidence.

## Consequences
- No direct pushes or force pushes can reach `main`.
- All changes must pass all 7 CI checks before merge.
- Verified with disposable PR #59: direct push blocked (403), merge blocked while checks pending even with `--admin`.

## Verification
- Ruleset active with ID `27886008`.
- Branch `main` reports `protected: true`.
- Disposable test PR #59 demonstrated hard rejection of unverified merges.
