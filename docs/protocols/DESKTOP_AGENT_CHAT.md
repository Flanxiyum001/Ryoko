# Desktop agent chat, proposal review and usage meter

Status: initial integrated implementation (human-controlled; no autonomous file writes).

## Agent chat

The desktop inspector has a chat surface using the existing workspace model router
and provider catalog. The user can attach at most four exact workspace paths, by
typing `@path/to/file` or using the file picker, and can attach an editor
`@selection`. Attachments are read natively, not trusted from the renderer;
metadata paths, symlink escapes, oversized files (>32 KiB), and common credential
file names are rejected. The history is bounded to twelve turns. Chat streams
`ryoko-chat-chunk`, `ryoko-chat-done` and `ryoko-chat-error` by request ID.
Cancel invalidates an in-flight stream. Chat has no write, shell, or tool calls.
Untrusted repository content is delimited as data for the model; prompt injection
is still a model-level risk, so the output must not be treated as an instruction.

## Reviewable model edits

Ctrl+K and chat code-fence suggestions use the same proposed-text review planner.
The operator sees a Monaco diff, selects individual line-based hunks, then chooses
**Apply approved hunks to editor**. The preview remains read-only and compares
against an exact snapshot of the editor buffer. An intervening edit invalidates
the proposal. Approval changes only the Monaco buffer, **not the disk**; saving
uses the existing optimistic-concurrency filesystem command. Proposal selection
snapshots are not persisted or automatically retried.

## Worktree review inbox

Published agent worktree sessions are read from the existing session database.
For a listed file, the native backend first checks the published session and its
file manifest; it then reads the blob from the managed local `ryoko/*` Git
branch without checking it out, merging or mutating the index. The same diff
review control may apply approved hunks to an **existing** editor file.
New files, renames, binary files and large blobs must use the existing Git review
workflow; the inbox does not attempt to auto-merge them.

## Mission Control and usage

The event-log DAG remains the source of truth for live task states, usage and
published file lists. Cards show reported tokens, estimated ledger cost and the
first two published paths. The status bar reports mission plus chat tokens and combines Mission Control
ledger estimates with priced chat-model usage. Models without known pricing
contribute tokens but not a dollar estimate; this is never a provider invoice.

## Verification

From `apps/desktop`: `npm ci && npm run test && npm run build`.
From `apps/desktop/src-tauri`: `cargo fmt --all -- --check && cargo test --locked`
when the native GTK/WebKit prerequisites are installed.
From the repository root, run the workspace checks from `AGENTS.md`.

Manual acceptance:
1. Open a workspace, choose a configured model, send a message with a valid
   `@file`, then attach `@selection` and confirm incremental reply rendering.
2. Cancel a streaming response; ensure late chunks are ignored.
3. Send a code fence from a selection or use Ctrl+K; uncheck one of two hunks,
   apply, confirm only approved lines enter the dirty buffer, and save manually.
4. Change the buffer after the request; confirm stale proposals cannot apply.
5. Run an isolated team task and inspect published worktree file proposals
   through Review Inbox. Nothing is merged or saved by opening the diff.
6. Verify Mission Control cards and the status bar show real event-log usage;
   check that chat is included when model pricing is known, without treating estimates as billed costs.
