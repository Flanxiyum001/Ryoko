//! Git worktree orchestration for Project Ryoko.
//!
//! The implementation lives in [`git`], mirroring the architecture brief's
//! `src/git.rs` module. Every agent task receives an isolated worktree under
//! `.ryoko/worktrees`, so parallel builds and edits cannot touch the user's
//! checkout. Publication returns a patch, and merging requires a passed
//! validation report.

pub mod git;

pub use git::{
    GitWorktreeManager, MergeReceipt, WorktreeError, WorktreePullRequest, WorktreeSession,
};
