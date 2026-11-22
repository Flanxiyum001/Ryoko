# Durable DAG planner: recovery-safe scheduling foundation

The PR #16 archive proposed an `ryoko-orchestrator` manifest that depended on a missing `ryoko-events` crate and supplied no source implementation. This PR replaces the missing executable with a small, tested scheduling layer built on the already-merged `ryoko-storage` crate.

- Validates a bounded task dependency graph, detecting missing dependencies, duplicates, self-references, and cycles.
- Produces a stable topological order.
- Starts only queued tasks whose prerequisites are durably marked complete.
- Uses revision-checked state transitions to prevent stale retries/approvals.
- Handles explicit approval decisions and validation outcomes.
- Reopens a store without executing any tools or replaying external side effects automatically.

This is **not** a production autonomous engineering agent. The existing core orchestrator still owns agent execution. A later integration PR needs verified worktree leases, validation receipts, model/tool wiring, and operator-controlled review before orchestration can mutate or merge code.
