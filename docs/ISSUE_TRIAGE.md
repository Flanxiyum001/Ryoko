# Issue triage and contribution workflow

Ryoko is a pre-alpha open-source AI IDE. Issues describe **proposed or verified work**, not delivered capabilities. [Browse open issues](https://github.com/Flanxiyum001/Ryoko/issues?q=is%3Aissue%20is%3Aopen) before filing a new one. Check the [contribution guide](../CONTRIBUTING.md), [engineering rules](../AGENTS.md) and [security policy](../SECURITY.md).

## What makes an issue ready?

A scoped issue states the observable problem, current behavior or architectural gap, affected component, implementation boundaries, testable acceptance criteria, verification plan, relevant documentation and dependencies. File a feature request for new functionality and a bug report for a reproducible regression. Do not request an entire subsystem rewrite in a single issue.

Suggested issue titles are `fix(component): symptom`, `feat(component): outcome`, `test(component): coverage goal`, `perf(component): measured target`, `docs: inaccurate or missing guidance`, and `chore(repo): maintenance goal`. Use a separate pull request per focused change; link it to the issue and explain any split work.

## Labels

GitHub's **existing standard labels** are currently used where appropriate:

| Label | When to use |
| --- | --- |
| `bug` | Confirmed unexpected behavior with reproduction |
| `enhancement` | Proposed feature, engineering improvement or hardening |
| `documentation` | Docs-only correction or improvement |
| `good first issue` | Small, well-bounded change approachable by newcomers |
| `help wanted` | Maintainer is open to a contributor taking this on |
| `question` | A question needing clarification rather than implementation |

Additional labels below are **proposed taxonomy, not automatically provisioned** by this document. A maintainer must create them under **Issues → Labels** before applying them:

- Priority: `priority:P0` (release/security/correctness blocker), `priority:P1` (core product/quality), `priority:P2` (polish or cleanup).
- Area: `area:desktop`, `area:agents`, `area:sandbox`, `area:github`, `area:release`, `area:docs`.
- Workflow: `status:needs-design`, `status:blocked`, `status:ready`.

Until those custom labels exist, the `**Triage:**` footer in each maintainer-authored issue records priority, area, approximate effort and dependencies. Do not silently treat a planned label as an existing GitHub label. Avoid multiple priority labels on one issue, and remove stale status labels during triage.

## Priority and readiness

- **P0:** Review first; security-sensitive details belong in private [security reports](../SECURITY.md), not public tickets.
- **P1:** Important, bounded work with a clear outcome and test evidence.
- **P2:** Documentation, ergonomic polish, benchmarks or optional improvements.
- **Blocked:** Missing dependency/design decision or validation environment; state what unlocks it.
- **Ready:** Scope and acceptance criteria are stable enough for a small PR.

Effort estimates (S/M/L) are planning hints, not schedules or contributor promises.

## Contributor and maintainer lifecycle

1. **Triage:** Search for duplicates; verify existing implementation and prior PRs. Request a reproduction or split the scope if needed. Avoid assigning maintainers or deadlines without agreement.
2. **Claim:** Comment with the intended approach before a substantial change; the maintainer confirms scope when needed.
3. **Implement:** Work on a dedicated branch or fork. Follow existing threat models and use an isolated test project for agent-generated changes.
4. **Open PR:** Reference the issue, fill the [PR template](../.github/PULL_REQUEST_TEMPLATE.md), show checks run/failed/skipped, and include screenshots for UI changes. Draft is appropriate when review is not yet requested.
5. **Review:** Maintainer inspects code, security boundaries, documentation and CI. A green check is evidence, not permission or a security certification.
6. **Merge/close:** Only the human maintainer authorizes integration. Close via the merged PR when completed; use `not planned` with explanation for rejected proposals. The automation never grants itself merge authority.

No agent or contributor should merge, release, deploy or add the `ryoko-fix` trigger without explicit maintainer authorization. New feature issues should not be advertised as shipped until implementation and relevant documentation have been verified.
