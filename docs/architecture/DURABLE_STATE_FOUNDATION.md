# Durable state foundation (phase 1)

This crate is a **new, independently tested, synchronous SQLite storage layer** for Ryoko. It replaces the unbuildable storage skeleton from the PR #16 archive, which referenced missing `memory_store`, `approval_store`, and `checkpoint` modules.

It provides task lifecycle states with optimistic revision checks, a 1 MiB-bounded append-only event journal, replay by monotonically increasing sequence, and read-only discovery of interrupted tasks. Reopening the database preserves both task state and event order. It never automatically re-executes old tool calls: resuming must be authorized by the operator.

**Not included yet:** encrypted secrets, complete checkpoint restoration, permission/approval persistence, orchestration runtime wiring, and cross-process concurrent write arbitration. Callers must protect the workspace state directory and avoid putting credentials in event payloads.

The original broad prototype remains at `archive/pr16-foundation-work-in-progress` for reference. This production-targeted crate deliberately has fewer guarantees than that prototype claimed.
