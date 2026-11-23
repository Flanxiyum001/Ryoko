# Maintainer review and branch-protection checklist

Ryoko is a single-maintainer, public, pre-alpha repository. The goal is a traceable path from **issue → branch → CI → human review → authorized merge**, with no accidental default-branch updates or automatic production release. This page is **setup guidance, not evidence that GitHub repository protections are enabled**. Track initial configuration in [issue #36](https://github.com/Flanxiyum001/Ryoko/issues/36).

## Ruleset to configure in GitHub

A repository administrator must perform the following in the GitHub website:

1. Open **Settings → Rules → Rulesets → New branch ruleset** for this repository. Name it `Protect main — pull requests and CI`, set **Enforcement status: Active**, and target the default branch `main`.
2. Enable **Require a pull request before merging**. Block **force pushes** and **branch deletion**. Keep any bypass actors absent or tightly scoped and justified; don't let an automated App or ordinary contributor bypass the review boundary.
3. Enable **Require status checks to pass**. Select the **exact current check names** from a successful PR run, rather than guessing from workflow titles. At minimum cover Rust lint/format/tests, desktop frontend/Tauri, GitHub App checks and the Windows/macOS/Linux native test matrix where supported. Verify checks run on `pull_request`, not only `push`.
4. Consider **require branch up to date before merging** only after confirming the workflows and merge queue strategy; otherwise it can needlessly block every contributor. Do not declare a job required if it cannot run for external PRs.
5. Leave **auto-merge disabled**. Do not introduce automation that writes directly to `main`, merges PRs, publishes tags or accesses production signing secrets without human approval.
6. Test the policy using a disposable PR: confirm incomplete checks block merging; confirm a direct update or force push to `main` is rejected for an actor without bypass rights; capture results in issue #36.

### Critical single-maintainer approval caveat

GitHub does **not** let a PR author approve their own PR. With a single maintainer (`Flanxiyum001`), a hard **1 approving review** rule will block that maintainer's self-authored PRs unless a separate trusted reviewer is added. Until such a reviewer exists, the practical solo-maintainer setup is: **require PR + passing checks, set required approvals to 0, and have the maintainer manually inspect and merge each PR**. This preserves owner-controlled merge decisions but cannot technically prove that the owner read every diff.

If a second trusted reviewer joins, consider raising the required approvals to 1, requiring code-owner review, and dismissing stale approvals after new commits. Do not claim these guarantees before the ruleset is actually enabled and tested.

The [CODEOWNERS file](../.github/CODEOWNERS) routes reviews of outside contributions to `@Flanxiyum001`; it **does not** itself enforce approval, and cannot supply approval for that owner's own PRs. The repo's PR template requests an explicit verification report, and [AGENTS.md](../AGENTS.md) forbids unauthorized integration.

## Required-check and release hygiene

The existing workflows include [CI](../.github/workflows/ci.yml) and [Phase 5 cross-platform integration](../.github/workflows/platform-integration.yml). Check their exact job names and up-to-date status in the GitHub UI when creating a ruleset. The unsigned preview publisher and signed-production release workflows have separate controls; avoid treating an unsigned preview or a `push`-only check as a successful release sign-off.

For every PR, record:
- Linked issue, risk and affected subsystem.
- New or updated tests for changed behavior (particularly permission/sandbox boundaries).
- Passing, failing and skipped CI explicitly; platform-specific checks as applicable.
- Any UI screenshot/smoke-test evidence and the exact source commit.
- Maintainer review decision, including any accepted limitation.

Review source changes before enabling any workflow with write permissions, especially automations triggered by issue labels. The experimental `ryoko-fix` label can start an agent pipeline; use it only by explicit opt-in, never as a general-purpose triage label.

## If policy blocks a legitimate change

Do not quietly disable protections or force-push as a workaround. Identify the exact failing check, missing reviewer or ruleset mismatch; document the exception on the PR and use a reversible configuration change only with explicit maintainer authorization. Restore the protection and test it afterward.

For label conventions and contributor issue flow, see [issue triage](ISSUE_TRIAGE.md). For private vulnerability disclosure, see [SECURITY.md](../SECURITY.md).
