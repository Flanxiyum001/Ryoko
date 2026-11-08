//! Git worktree orchestration for Project Ryoko.
//!
//! The implementation lives in [`git`], mirroring the architecture brief's
//! `src/git.rs` module. Every agent task receives an isolated worktree under
//! `.ryoko/worktrees`, so parallel builds and edits cannot touch the user's
//! checkout. Lease state is persisted in SQLite under `.ryoko/state`, so a
//! daemon can recover published branches after a restart. Publication returns
//! a patch, and merging requires a passed validation report.

pub mod git;

pub use git::{
    GitWorktreeManager, MergeReceipt, WorktreeDiff, WorktreeError, WorktreePullRequest,
    WorktreeRecord, WorktreeSession, WorktreeState,
};
