# Phase 3 — Intelligent collaboration (experimental)

Ryoko's executable team mode now separates **implementation**, **independent
review**, **evidence-backed verification**, and **human approval**. This is not a
promise that AI code review is infallible: final acceptance remains a human
decision, and the compilation/test sandbox remains the hard execution gate.

## Dynamic team composition

`dynamic_plan_with_reviews(goal, ReviewConfig { code, security })` retains the
existing goal-specific Architect, Backend, UI/UX, Full-stack, QA, Security,
DevOps and Integrator tasks. It optionally adds two *independent, read-only*
reviewer tasks on the validated Full-stack implementation commit:

- `independent-code-reviewer` — correctness, maintainability, regression risk
- `independent-security-reviewer` — trust boundaries, dangerous inputs and
  adversarial edge cases

They have separate task IDs, agent identities, worktree leases, and dependency
edges. Both must succeed before the Integrator runs. They cannot write files or
approve their own target. Other QA and Security tasks remain responsible for
writing and validating concrete regression/hardening changes.

In CLI team mode, both reviewers are on by default. For smaller, trusted
experiments, omit one with `--no-code-review` or
`--no-security-review`. These flags **do not** disable regression tests or
the existing QA/Security implementation tasks. Without `--team`, the flags
are not accepted.

## Review contract and evidence

Each independent review is a **single JSON object**:

```json
{
  "verdict": "fail",
  "summary": "One confirmed high-severity correctness defect",
  "evidence_refs": ["validator:<reviewed-task-uuid>"],
  "findings": [
    {
      "code": "RYOKO-001",
      "severity": "high",
      "path": "src/lib.rs",
      "line": 42,
      "message": "Error path returns success",
      "recommendation": "Propagate the failed parse and add a regression test",
      "evidence_ref": "validator:<reviewed-task-uuid>"
    }
  ]
}
```

The reviewer receives the **actual producer commit diff** from its isolated
worktree (up to 64 KiB), the target's bounded summary, and one trusted
validation reference. External repository content is treated as untrusted
data, never elevated to the system prompt.

Only the orchestrator can publish the reference after the producer has
successfully run and published a validated change. The reference is created
only for a passing `CommandValidator` report with an executed spec/test
command recorded in its logs. The permissive `AllowAllValidator`, missing
logs, unsuccessful validation, and an invented model citation cannot produce
a passing independent review. The orchestrator **rechecks** the review JSON
and reference even if a custom agent claims success.

Review records reject missing/invalid paths, malformed JSON, arbitrary file
proposals, missing recommendations/evidence, unbounded fields and contradictory
verdicts. Critical/high findings require `fail`; a passing review cannot
contain a blocking finding. Failing reviews route actionable feedback to the
producer through the existing bounded repair loop; exhausted failures
escalate to human review.

The `run --report PATH` JSON now includes `independent_reviews` for passing
structured reviews, alongside existing task outcomes, validation logs and
usage telemetry. Failed findings remain in the task failure/repair feedback.
The source of truth for test execution is still the **sandbox validation log**,
not the review's own prose.

## Semantic conflict resolution

The Integrator continues to merge only validated QA/Security sibling branches
in its own throwaway worktree. On a textual conflict it sends the model:

1. The conflicted text from the working tree.
2. Git's stage 1 (merge base), stage 2 (ours), and stage 3 (theirs).

Missing or non-text merge-stage sources fail closed for human intervention.
The response must resolve exactly the conflicted paths, remain within the size
limits, and contain no Git conflict markers. A staged patch
`git diff --cached --check` runs before committing the merge; a final
`git diff --check` runs on the integration range. The normal container-backed
compiler, regression suite and security audit must still pass before the
integrated change is published for explicit human acceptance.

**Limitation:** a model cannot certify that a semantic conflict has been
resolved correctly. Passing Git checks means the patch is well-formed, not
behaviorally correct. Add regression tests for both sides of every conflict and
inspect the integrated PR. No automatic merge or deployment authorization is
introduced by Phase 3.

## Verification and failure cases

Run:

```sh
cargo fmt --all -- --check
cargo check --workspace --locked --all-targets
cargo clippy --workspace --locked --all-targets -- -D warnings
cargo test --workspace --locked
```

Core regression tests cover reviewer-policy selection, dependency
independence, forged/stale/missing evidence, blocking findings, and malformed
model output. Existing worktree integration tests exercise non-conflicting
sibling merges and model-assisted three-way resolution.

The caller is responsible for configuring real local or hosted models,
a reviewed sandbox image, and a meaningful `--check-command`. A missing
provider, unsupported code diff, absent trusted evidence or ambiguous merge
source causes the task to fail rather than report an unsupported success.
