# Incremental symbol indexing in Ryoko

The existing `ryoko-core::WorkspaceIndex` and CLI `ryoko index` now use
content hashes for changed-file detection. This replaces the previous
unconditional full reparse on every `ryoko index` and `ryoko run`.

- The CLI walks its existing bounded, gitignore-aware candidate set, hashes
  source bytes, and invokes tree-sitter only for changed candidates.
- A single SQLite transaction replaces changed symbols/callers and removes
  entries for files that have disappeared from the candidate set.
- Legacy `.ryoko/state/symbols.sqlite3` databases are migrated idempotently
  by adding the `content_hash` column. They are reparsed once.
- Unchanged files keep their previous symbol and caller rows.
- The existing file-count CLI output reports considered source candidates.
- Symbol parsing stays bounded at 2,000 files, 512 KiB per file.

**Trade-off:** The CLI still reads source bytes to compute hashes, which is
typically cheaper than reparsing all files. A file that cannot be read during
a scan remains in the candidate set and is retried on the next run. This
implementation does not yet add filesystem watcher integration or benchmarks.

The PR #16 archive's separate `ryoko-index` crate was not copied because
Ryoko already had an operational index in `ryoko-core`. Maintaining two
competing stores would make prompts and migrations less reliable.
