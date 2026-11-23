# Agent runtime

Status: `ryoko-core` implements the `AgentRuntime` contract, a `ModelAgent`
with a bounded host-approved MCP tool loop, a dependency-aware orchestrator,
isolated worktrees and IACP v1. The CLI supports single-agent and multi-agent
`--team` execution with an Integrator. Durable orchestration state and
structured review findings do not exist yet.

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
| Integrator | Merge validated sibling branches and publish one review diff |
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

### Dynamic DAG routing

`ryoko_core::team::dynamic_plan` implements the proposal step for the built-in
team. The Project Manager task always runs first and owns decomposition; the
remaining nodes are activated from `GoalSignals`, which is a keyword classifier
over the goal:

- **UI/UX** runs only when the goal names an interface concern (screen, layout,
  accessibility, component, theme, ...). Its task carries
  `ryoko.verify = headless-browser,screenshot`, and QA repeats the same
  verification metadata so browser evidence is requested rather than assumed.
- **DevOps** runs only when the goal names delivery work (deploy, CI, container,
  monitoring, rollback, ...).
- Architect, backend, full-stack, security and QA are always present, because a
  change without a producer or a review is not reviewable.
- The Integrator is appended only when QA and Security both publish a branch,
  which is its documented minimum.

Signals are routing hints only. They grant no filesystem, network or deployment
permission, and the generated plan still has to pass
`Orchestrator::validate_plan` before anything executes.

## Speculative read-only tasks

`Task::speculative()` (metadata `ryoko.speculative = "true"`) marks a task as a
read-only check, such as a dependency audit, static analysis or test-plan
draft, that may run ahead of the producers it depends on:

- it acquires a concurrency permit and starts immediately, without waiting for
  its dependencies, so it overlaps code generation;
- it does not see its dependencies' summaries, because they do not exist yet;
- a file change from it is a contract violation: the lease is discarded and
  the task fails without retry;
- after it finishes it releases its permit and then waits for its
  dependencies. If any of them fails or is skipped, its result is discarded
  and it is reported as skipped (`speculative result discarded: ...`), so its
  own dependents are skipped too.

The built-in team plans do not mark any task speculative: their QA and Security
nodes publish branches and feed the repair loop, which requires the producer's
output. Speculation is opt-in for callers that build their own DAG.

## Self-healing reviews

A failing QA or Security node is feedback for a producer, not a dead end. After
the scheduled DAG finishes, `Orchestrator` runs a bounded repair loop
(`OrchestratorConfig::max_repair_cycles`, default 1, 0 disables it):

1. Each failed QA/Security task is matched to the producer dependency that owns
   the branch under review (architect, backend, full-stack or UI/UX).
2. The producer is replayed with the review's test logs or audit findings in
   `ryoko.feedback`, so a retry is corrective rather than a blind replay.
3. The review is replayed against the repaired branch.
4. A review that still fails consumes another cycle until the budget is spent,
   then the task is reported in `OrchestrationReport::needs_human_review` with
   the producer it tried and the cycles spent. It stays in `failed` as well.
5. Nodes that were skipped only because a repaired review had failed are replayed
   once their dependencies are satisfied again.

Every replay still allocates a fresh workspace lease, must pass validation and
publishes an ordinary reviewable branch. The loop never merges, pushes or
deploys.

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
