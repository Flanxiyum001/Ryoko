# Agent runtime

Status: `ryoko-core` implements the `AgentRuntime` contract, a `ModelAgent`, a
dependency-aware orchestrator, isolated worktrees and IACP v1. The single-agent
coding workflow exists in the CLI. Multi-agent execution, durable state and review
findings do not.

## Roles

| Role | Responsibility |
| --- | --- |
| Project Manager | Requirements, plan, delegation |
| Architect | Interfaces and technical decisions |
| UI/UX Designer | Experience, design, accessibility |
| Frontend Engineer | Frontend implementation |
| Backend Engineer | APIs, data, business logic |
| Full-stack Engineer | Cross-layer implementation |
| Security Engineer | Threat modelling and security review |
| QA Engineer | Tests, validation, regressions |
| DevOps Engineer | CI/CD and release preparation |

A role is identity and default behaviour. It grants no permissions. Capabilities,
tool allowlists and budgets are granted explicitly per task.

## Agent definition

```rust
pub struct AgentDefinition {
    pub id: AgentId,
    pub role: AgentRole,
    pub description: String,
    pub system_prompt: String,
    pub model_policy: ModelPolicy,
    pub capabilities: Vec<Capability>,
    pub tool_allowlist: Vec<ToolId>,
    pub max_tokens: u64,
    pub max_runtime_seconds: u64,
    pub review_policy: ReviewPolicy,
}
```

## Lifecycle

```text
Created → Planning → WaitingForApproval → Ready → Running
Running → WaitingForTool | WaitingForReview | Retrying
Running → Validating → Completed | Failed
Any non-terminal state → Cancelling → Cancelled
```

Terminal states are immutable. A retry is a new attempt with its own identifier,
not an edit of history. Pause and resume are only valid where the runtime can
guarantee no partial side effect is left unrecorded.

## Execution pipeline

1. **Understand** — retrieve context with provenance (files, symbols, conventions).
2. **Plan** — tasks, dependencies, acceptance criteria.
3. **Authorize** — tool permissions, resource limits, approval requirements.
4. **Execute** — run in an isolated worktree with a scoped tool allowlist.
5. **Publish** — structured patch, tests and evidence bound to the source tree.
6. **Review** — independent review with file and line references.
7. **Integrate** — human reviews diff and evidence; accept, reject or request changes.

## Isolation

Each task gets a dedicated worktree and branch, a scoped file policy, a tool
allowlist, a resource budget, a task log and an execution identifier. Agents do not
share writable workspaces by default.

## Team planning

Do not spawn the full roster for every request. The planner weighs complexity,
languages, affected areas, security sensitivity, model capabilities, budget,
dependencies and expected parallelism, then proposes a team and task graph. The
proposal requires approval and grants nothing implicitly.

| Request | Team |
| --- | --- |
| Typo fix | one developer |
| API endpoint | backend + QA |
| Dashboard | architect + UX + frontend + QA |
| Authentication | architect + backend + security + QA |
| New application | PM + architect + selected specialists |

## Conflict handling

File locks are necessary but insufficient: two agents can edit different files
that share an interface.

1. Detect overlapping paths and changed public symbols.
2. Refuse silent overwrite.
3. Rebase or reconstruct the proposal.
4. Re-run affected tests.
5. Present unresolved conflicts to the human.

## Budget and cancellation

Reserve budget before dispatch, record actual usage, release the remainder.
Enforce per-agent, per-task and per-project ceilings, output caps, timeouts, retry
limits and concurrency limits. Cancellation must reach the provider stream, tool
execution and runner process, and must be acknowledged within a bounded interval.

## Evidence

A validation result is bound to a source snapshot. It records workspace, task,
attempt, source tree hash, runner image digest, commands, exit status and
timestamps. If the source changes, the old result is not evidence for the new
source.
