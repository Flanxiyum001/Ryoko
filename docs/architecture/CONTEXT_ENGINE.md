# Context engine and project memory

Status: `ryoko-core` provides tree-sitter context extraction for Rust,
TypeScript/TSX, JavaScript, Python, Go, C/C++, Java, Kotlin, C#, Swift and SQL
(imports, symbol outline, intra-file call edges, focused bodies, and SQL schema
objects such as tables, views and indexes). `ryoko run` scans supported source
files with bounded size/depth limits and injects their outlines into the
developer agent's prompt; the review revise flow injects the same outline
alongside the changed file contents. `SharedMemory` is an in-process
blackboard. A bounded, human-editable project memory store now lives under
`.ryoko/memory/`. Durable semantic retrieval and automatic memory promotion
do not exist yet.

Outlines never contain bodies: each symbol is rendered with its declaration
signature (first line up to the body, whitespace-collapsed, at most 200 bytes)
so agents see parameter and return types of unrelated files without paying for
their implementations. `ModelAgent` places the shared instructions and this
outline in the request's system prefix, ahead of the role and goal, and sets
`ChatRequest::cache_system_prefix`. The Anthropic adapter then sends the system
prompt as content blocks with an `ephemeral` `cache_control` breakpoint after
that prefix; OpenAI-compatible providers cache identical prefixes automatically
and need only the ordering. Cache reads are reported as
`Usage::cached_input_tokens`.

## Indexing pipeline

```text
filesystem → discovery → ignore rules → language detection
  → tree-sitter parse → symbol extraction → call edges → LSP semantics
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

`ryoko-core::ProjectMemory` loads three Markdown files from
`.ryoko/memory/`: `decisions.md`, `conventions.md` and `rejections.md`. Entries
are separated by `## ` headings and may contain freeform Markdown, so an
operator can inspect and edit them directly. `ryoko run` and the review revise
flow include a bounded rendering of these files as reference context. When the
operator rejects a published task, the CLI prepends an entry to
`rejections.md` so later runs can avoid repeating the same proposal.

The store is intentionally conservative: memory files must be regular files
beneath the workspace, symlinks are rejected, each file is capped at 256 KiB
and each entry is capped at 8 KiB. Memory is untrusted project text and is
never executed. Automatic memory writing by agents remains out of scope; only
an explicit human edit or review rejection changes the store.

The longer-term memory categories remain:

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
