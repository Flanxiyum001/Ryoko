# Context engine and project memory

Status: `ryoko-core` provides tree-sitter Rust context extraction (imports, symbol
outline, focused bodies). `SharedMemory` is an in-process blackboard. Durable
indexing, retrieval and memory do not exist yet.

## Indexing pipeline

```text
filesystem → discovery → ignore rules → language detection
  → tree-sitter parse → symbol extraction → LSP semantics
  → dependency edges → search indexes → retrieval
```

Indexed: paths, symbols (functions, types, traits, impls, modules, constants,
macros), imports, references where an LSP provides them, dependencies,
documentation, test files, Git history, approved decisions, existing diagnostics.

## Retrieval

Hybrid, ranked and token-budgeted:

1. exact path and filename;
2. symbol lookup;
3. full-text search;
4. semantic similarity;
5. dependency-graph traversal;
6. Git history.

Exact symbol lookup is frequently more reliable than embedding similarity for
programming tasks; semantic search complements it, it does not replace it.

Every context item carries: source file, line range or symbol, content hash,
retrieval reason, relevance score and timestamp. Agents cite that evidence, and
stale context (hash mismatch) is detectable.

## Incremental indexing

Consume filesystem events and document-change events to identify affected files.
Re-parse only those files and their dependents. Run a periodic reconciliation scan
to recover from missed events and from changes made while the indexer was stopped.

## Project memory

Memory categories:

| Kind | Example |
| --- | --- |
| Project | architecture, conventions, build commands |
| Decision | approved technical choices and rejected alternatives |
| Task | completed work, failures, regressions |
| Agent | role-specific working preferences |
| Developer | explicitly saved preferences |
| Session | current conversation and task state |

Rules:

1. AI-generated memories are **proposed**, never trusted truth.
2. Lifecycle: proposed → verified → human-approved → deprecated → deleted.
3. Each record stores provenance: source, author (human or agent), task, timestamp,
   confidence and the evidence it came from.
4. Users can list, inspect, edit and delete any record. Deletion is real deletion,
   not a hidden tombstone.
5. Secrets and credentials are never written to memory, prompts or indexes.
6. Memory is scoped per workspace by default; cross-project sharing is opt-in.

## Storage

Memories and index metadata live in SQLite. Large artifacts (snapshots, reports)
live in a managed artifact directory with content hashes in the database. The index
is rebuildable from source; memory is not, so memory is backed up and exportable.

## Evaluation

Track retrieval precision on a fixture set of real questions ("where is X
validated?", "what calls Y?"), stale-context rate, and the share of agent answers
that cite resolvable file and line references. Optimise for correct, cited answers,
not for index size.
