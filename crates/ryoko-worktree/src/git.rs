//! Isolated Git worktree leases for parallel agents.
//!
//! Each task gets a hidden branch and directory under `.ryoko/worktrees`. The
//! main editor checkout is never modified until an approved merge is committed.
//! All Git calls are `tokio::process::Command` invocations with piped output;
//! no shell is involved and no subprocess blocks the async reactor.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::process::{Output, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use async_trait::async_trait;
use rusqlite::{Connection, OptionalExtension, params};
use ryoko_core::workspace::validate_relative_path;
use ryoko_core::{
    AgentIdentity, CoreError, CoreResult, ProposedChange, PullRequest, Task, ValidationReport,
    WorkspaceLease, WorkspaceProvider,
};
use serde::{Deserialize, Serialize};
use tokio::process::Command;
use uuid::Uuid;

const STATE_SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS worktree_sessions (
    task_id TEXT PRIMARY KEY,
    agent_id TEXT NOT NULL,
    branch_name TEXT NOT NULL,
    path TEXT NOT NULL,
    base_commit TEXT NOT NULL,
    base_branch TEXT NOT NULL,
    state TEXT NOT NULL,
    head_commit TEXT,
    merge_commit TEXT,
    summary TEXT,
    files_json TEXT NOT NULL DEFAULT '[]',
    patch TEXT,
    created_at INTEGER NOT NULL,
    updated_at INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS worktree_sessions_state
    ON worktree_sessions (state, updated_at);
"#;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorktreeSession {
    pub id: Uuid,
    pub agent_id: String,
    pub branch_name: String,
    pub path: PathBuf,
    pub base_commit: String,
    pub base_branch: String,
}

/// Lifecycle state persisted for every worktree lease.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorktreeState {
    /// The agent is still working in the lease.
    Active,
    /// The worktree has been committed and is waiting for review or merge.
    Published,
    /// The validated branch was integrated into its target branch.
    Merged,
    /// The lease was dropped without integration.
    Abandoned,
}

impl WorktreeState {
    fn as_str(self) -> &'static str {
        match self {
            Self::Active => "active",
            Self::Published => "published",
            Self::Merged => "merged",
            Self::Abandoned => "abandoned",
        }
    }

    fn parse(value: &str) -> Option<Self> {
        match value {
            "active" => Some(Self::Active),
            "published" => Some(Self::Published),
            "merged" => Some(Self::Merged),
            "abandoned" => Some(Self::Abandoned),
            _ => None,
        }
    }
}

/// Durable record for a worktree lease. The task graph can be rebuilt from
/// `open_sessions()` after a crash or daemon restart.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorktreeRecord {
    pub session: WorktreeSession,
    pub state: WorktreeState,
    pub head_commit: Option<String>,
    pub merge_commit: Option<String>,
    pub summary: Option<String>,
    pub files: Vec<String>,
    pub patch: Option<String>,
    pub updated_at: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorktreePullRequest {
    pub id: Uuid,
    pub task_id: Uuid,
    pub source_branch: String,
    pub target_branch: String,
    pub files: Vec<String>,
    pub patch: String,
    pub summary: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorktreeDiff {
    pub files: Vec<String>,
    pub patch: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MergeReceipt {
    pub target_branch: String,
    pub previous_tip: String,
    pub merge_commit: String,
    pub source_branch: String,
}

#[derive(Debug, thiserror::Error)]
pub enum WorktreeError {
    #[error("`{0}` is not a Git repository with a commit")]
    NotARepository(String),
    #[error("invalid agent id: {0}")]
    InvalidAgentId(String),
    #[error("invalid Git branch name: {0}")]
    InvalidBranch(String),
    #[error("path already exists: {0}")]
    PathExists(String),
    #[error("branch already exists: {0}")]
    BranchExists(String),
    #[error("worktree has uncommitted changes")]
    UncommittedChanges,
    #[error("main checkout has uncommitted changes; commit or stash them before merging")]
    DirtyMainCheckout,
    #[error("target branch `{0}` is checked out in another worktree")]
    TargetCheckedOut(String),
    #[error("merge conflict integrating `{branch}`: {details}")]
    MergeConflict { branch: String, details: String },
    #[error("nothing to publish in `{0}`")]
    NoChanges(String),
    #[error("merge requires a passed validation report: {0}")]
    NotApproved(String),
    #[error("worktree `{0}` has not been published")]
    NotPublished(String),
    #[error("worktree state store failed: {0}")]
    State(String),
    #[error("git `{action}` failed ({status}): {stderr}")]
    Git {
        action: String,
        status: String,
        stderr: String,
    },
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

impl From<WorktreeError> for CoreError {
    fn from(value: WorktreeError) -> Self {
        CoreError::Workspace(value.to_string())
    }
}

#[derive(Clone)]
pub struct GitWorktreeManager {
    base_project_path: PathBuf,
    worktrees_root: PathBuf,
    state_path: PathBuf,
    state: Arc<Mutex<Option<Connection>>>,
}

impl std::fmt::Debug for GitWorktreeManager {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GitWorktreeManager")
            .field("base_project_path", &self.base_project_path)
            .field("worktrees_root", &self.worktrees_root)
            .field("state_path", &self.state_path)
            .finish_non_exhaustive()
    }
}

impl GitWorktreeManager {
    pub fn new(project_path: impl AsRef<Path>) -> Self {
        let base = std::fs::canonicalize(project_path.as_ref())
            .unwrap_or_else(|_| project_path.as_ref().to_path_buf());
        let state_root = base.join(".ryoko").join("state");
        Self {
            worktrees_root: base.join(".ryoko").join("worktrees"),
            state_path: state_root.join("worktrees.sqlite3"),
            base_project_path: base,
            state: Arc::new(Mutex::new(None)),
        }
    }

    pub fn base_project_path(&self) -> &Path {
        &self.base_project_path
    }

    pub fn worktrees_root(&self) -> &Path {
        &self.worktrees_root
    }

    pub fn state_path(&self) -> &Path {
        &self.state_path
    }

    /// Creates an isolated branch and worktree at the current `HEAD`.
    pub async fn create_worktree(
        &self,
        agent_id: &str,
        task_id: Uuid,
    ) -> Result<WorktreeSession, WorktreeError> {
        self.ensure_repository().await?;
        let slug = sanitize_agent_id(agent_id)?;
        let base_commit = self
            .checked(
                "resolve HEAD",
                &["rev-parse", "HEAD"],
                &self.base_project_path,
            )
            .await?;
        let base_branch = self
            .checked(
                "resolve current branch",
                &["symbolic-ref", "--short", "-q", "HEAD"],
                &self.base_project_path,
            )
            .await
            .unwrap_or_else(|_| "HEAD".to_owned());

        let branch_name = format!("ryoko/{slug}/{task_id}");
        let worktree_path = self.worktrees_root.join(format!("{slug}-{task_id}"));

        if worktree_path.exists() {
            return Err(WorktreeError::PathExists(
                worktree_path.display().to_string(),
            ));
        }
        if self.branch_exists(&branch_name).await? {
            return Err(WorktreeError::BranchExists(branch_name));
        }
        tokio::fs::create_dir_all(&self.worktrees_root).await?;

        let path = worktree_path.to_string_lossy().into_owned();
        let output = self
            .git(
                &self.base_project_path,
                &[
                    "worktree",
                    "add",
                    "--no-track",
                    "-b",
                    &branch_name,
                    &path,
                    &base_commit,
                ],
            )
            .await?;

        if !output.status.success() {
            // Roll back a partially-created branch/worktree. Both operations are
            // best-effort and never mask the original error.
            if worktree_path.starts_with(&self.worktrees_root) && worktree_path.exists() {
                let _ = tokio::fs::remove_dir_all(&worktree_path).await;
            }
            let _ = self
                .git(&self.base_project_path, &["branch", "-D", &branch_name])
                .await;
            return Err(self.git_error("create worktree", output));
        }

        let session = WorktreeSession {
            id: task_id,
            agent_id: slug,
            branch_name,
            path: worktree_path,
            base_commit,
            base_branch,
        };
        if let Err(error) = self.save_session(&session) {
            // The lease is not usable if it cannot be recovered after a crash.
            // Remove the worktree and branch before surfacing the state error.
            let _ = self.remove_worktree_dir(&session).await;
            let _ = self
                .git(
                    &self.base_project_path,
                    &["branch", "-D", &session.branch_name],
                )
                .await;
            return Err(error);
        }
        Ok(session)
    }

    /// Reattaches a durable session to its branch after a crash or after a
    /// published worktree was released. Active and published leases can be
    /// resumed; merged or abandoned leases cannot.
    pub async fn resume_worktree(&self, id: Uuid) -> Result<WorktreeSession, WorktreeError> {
        self.ensure_repository().await?;
        let record = self
            .load_session(id)?
            .ok_or_else(|| WorktreeError::NotPublished(id.to_string()))?;
        if !matches!(
            record.state,
            WorktreeState::Active | WorktreeState::Published
        ) {
            return Err(WorktreeError::NotPublished(record.session.branch_name));
        }
        let session = record.session;
        self.ensure_managed_session(&session)?;

        if let Some((path, _)) = self.worktree_checkout(&session.branch_name).await? {
            if path == session.path {
                return Ok(session);
            }
            return Err(WorktreeError::TargetCheckedOut(path.display().to_string()));
        }
        if session.path.exists() {
            return Err(WorktreeError::PathExists(
                session.path.display().to_string(),
            ));
        }
        if !self.branch_exists(&session.branch_name).await? {
            return Err(WorktreeError::NotPublished(session.branch_name.clone()));
        }
        if let Some(parent) = session.path.parent() {
            tokio::fs::create_dir_all(parent).await?;
        }
        let path = session.path.to_string_lossy().into_owned();
        let output = self
            .git(
                &self.base_project_path,
                &["worktree", "add", &path, &session.branch_name],
            )
            .await?;
        if !output.status.success() {
            return Err(self.git_error("resume worktree", output));
        }
        Ok(session)
    }

    /// Removes the physical worktree and deletes its branch, unless the branch
    /// was already published. A published branch survives so a later merge can
    /// consume it; call [`Self::release_worktree`] to free disk while keeping
    /// the branch, or [`Self::abandon_worktree`] to discard it explicitly.
    ///
    /// Idempotent: repeated calls after cleanup succeed.
    pub async fn clean_worktree(&self, session: &WorktreeSession) -> Result<(), WorktreeError> {
        self.ensure_managed_session(session)?;
        let state = self
            .load_session(session.id)?
            .map(|record| record.state)
            .unwrap_or(WorktreeState::Active);
        match state {
            WorktreeState::Published => {
                self.release_worktree(session).await?;
                if !self.branch_exists(&session.branch_name).await? {
                    // A missing branch can no longer be reviewed or merged.
                    self.mark_state(session, WorktreeState::Abandoned)?;
                }
                Ok(())
            }
            WorktreeState::Merged => {
                self.remove_worktree_dir(session).await?;
                self.delete_managed_branch(&session.branch_name).await
            }
            WorktreeState::Active | WorktreeState::Abandoned => {
                self.abandon_worktree(session).await
            }
        }
    }

    /// Removes the worktree directory but keeps its branch. This is the safe
    /// cleanup for a published lease that still needs review or merge.
    pub async fn release_worktree(&self, session: &WorktreeSession) -> Result<(), WorktreeError> {
        self.ensure_managed_session(session)?;
        self.remove_worktree_dir(session).await
    }

    /// Removes the worktree directory and its branch, and records the lease as
    /// abandoned. Use this for failed tasks or an explicit review rejection.
    pub async fn abandon_worktree(&self, session: &WorktreeSession) -> Result<(), WorktreeError> {
        self.ensure_managed_session(session)?;
        self.remove_worktree_dir(session).await?;
        self.delete_managed_branch(&session.branch_name).await?;
        self.mark_state(session, WorktreeState::Abandoned)
    }

    /// Commits every change in the isolated worktree and returns a reviewable
    /// patch. The main checkout is untouched. This is used by direct
    /// integrations that manage the worktree themselves; orchestrator leases
    /// publish only the paths they applied so build artifacts are never
    /// committed.
    pub async fn publish_worktree(
        &self,
        session: &WorktreeSession,
        summary: &str,
    ) -> Result<WorktreePullRequest, WorktreeError> {
        self.publish_worktree_inner(session, summary, None).await
    }

    async fn publish_worktree_inner(
        &self,
        session: &WorktreeSession,
        summary: &str,
        paths: Option<&[String]>,
    ) -> Result<WorktreePullRequest, WorktreeError> {
        self.ensure_worktree(session).await?;
        match paths {
            Some(paths) => {
                if paths.is_empty() {
                    return Err(WorktreeError::NoChanges(session.branch_name.clone()));
                }
                let mut args = vec!["add", "--"];
                args.extend(paths.iter().map(String::as_str));
                self.checked("stage proposed files", &args, &session.path)
                    .await?;
            }
            None => {
                if self.is_dirty(&session.path).await? {
                    self.checked("stage worktree changes", &["add", "-A"], &session.path)
                        .await?;
                }
            }
        }

        let staged = self
            .checked(
                "list staged files",
                &["diff", "--cached", "--name-only"],
                &session.path,
            )
            .await?;
        if staged.is_empty() {
            return Err(WorktreeError::NoChanges(session.branch_name.clone()));
        }

        let commit = self
            .git(
                &session.path,
                &[
                    "-c",
                    "commit.gpgsign=false",
                    "commit",
                    "--no-verify",
                    "-m",
                    summary,
                ],
            )
            .await?;
        if !commit.status.success() {
            return Err(self.git_error("commit worktree", commit));
        }

        let head = self
            .checked(
                "resolve worktree HEAD",
                &["rev-parse", "HEAD"],
                &session.path,
            )
            .await?;
        if head == session.base_commit {
            return Err(WorktreeError::NoChanges(session.branch_name.clone()));
        }

        let files = self
            .checked(
                "list changed files",
                &[
                    "diff",
                    "--name-only",
                    &format!("{}..HEAD", session.base_commit),
                ],
                &session.path,
            )
            .await?
            .lines()
            .filter(|line| !line.trim().is_empty())
            .map(str::to_owned)
            .collect();
        let patch = self
            .checked(
                "render patch",
                &[
                    "diff",
                    "--binary",
                    &format!("{}..HEAD", session.base_commit),
                ],
                &session.path,
            )
            .await?;

        let pull_request = WorktreePullRequest {
            id: Uuid::new_v4(),
            task_id: session.id,
            source_branch: session.branch_name.clone(),
            target_branch: if session.base_branch == "HEAD" {
                "main".to_owned()
            } else {
                session.base_branch.clone()
            },
            files,
            patch,
            summary: summary.to_owned(),
        };
        self.save_published(session, &pull_request, &head)?;
        Ok(pull_request)
    }

    /// Renders the same review artifact a human would see in `git diff`:
    /// changed paths and a binary patch between the target branch and the
    /// worktree HEAD. The worktree must already be attached.
    pub async fn diff_worktree(
        &self,
        session: &WorktreeSession,
        target_branch: &str,
    ) -> Result<WorktreeDiff, WorktreeError> {
        self.ensure_managed_session(session)?;
        self.ensure_worktree(session).await?;
        if !self
            .git(
                &self.base_project_path,
                &["check-ref-format", "--branch", target_branch],
            )
            .await?
            .status
            .success()
        {
            return Err(WorktreeError::InvalidBranch(target_branch.to_owned()));
        }
        let range = format!("{target_branch}...HEAD");
        let files = self
            .checked(
                "list worktree diff files",
                &["diff", "--name-only", &range],
                &session.path,
            )
            .await?
            .lines()
            .filter(|line| !line.trim().is_empty())
            .map(str::to_owned)
            .collect();
        let patch = self
            .checked(
                "render worktree diff",
                &["diff", "--binary", "--no-color", &range],
                &session.path,
            )
            .await?;
        Ok(WorktreeDiff { files, patch })
    }

    /// Discards uncommitted agent output after a failed revision while keeping
    /// the last published commit. Only managed worktrees are touched.
    pub async fn reset_worktree(&self, session: &WorktreeSession) -> Result<(), WorktreeError> {
        self.ensure_managed_session(session)?;
        self.ensure_worktree(session).await?;
        self.checked(
            "reset worktree",
            &["reset", "--hard", "HEAD"],
            &session.path,
        )
        .await?;
        self.checked("clean worktree", &["clean", "-fd"], &session.path)
            .await?;
        Ok(())
    }

    /// Reattaches a durable lease as a [`WorkspaceLease`]. Used by the review
    /// loop to apply feedback and republish without recreating the branch.
    pub async fn resume_lease(&self, id: Uuid) -> Result<Arc<dyn WorkspaceLease>, WorktreeError> {
        let session = self.resume_worktree(id).await?;
        let root = tokio::fs::canonicalize(&session.path)
            .await
            .unwrap_or_else(|_| session.path.clone());
        Ok(Arc::new(GitWorktreeLease {
            manager: self.clone(),
            session,
            root,
            applied: Mutex::new(BTreeSet::new()),
        }))
    }

    /// Integrates a committed worktree into `target_branch`. Requires a passed
    /// [`ValidationReport`] from the WASM guardrail. The merge commit is built
    /// with `merge-tree`/`commit-tree` and the target ref is updated with a
    /// compare-and-swap, so a conflict or concurrent commit cannot corrupt the
    /// repository.
    pub async fn merge_worktree(
        &self,
        session: &WorktreeSession,
        target_branch: &str,
        approval: &ValidationReport,
    ) -> Result<MergeReceipt, WorktreeError> {
        if !approval.passed {
            return Err(WorktreeError::NotApproved(approval.summary.clone()));
        }
        self.ensure_repository().await?;
        self.ensure_managed_branch(&session.branch_name)?;
        let record = self.load_session(session.id)?;
        if let Some(record) = &record {
            if !matches!(
                record.state,
                WorktreeState::Published | WorktreeState::Merged
            ) {
                return Err(WorktreeError::NotPublished(session.branch_name.clone()));
            }
        } else if !session.path.exists() {
            return Err(WorktreeError::NotPublished(session.branch_name.clone()));
        }
        if session.path.starts_with(&self.worktrees_root)
            && session.path.exists()
            && self.is_dirty_tracked(&session.path).await?
        {
            return Err(WorktreeError::UncommittedChanges);
        }
        if !self
            .git(
                &self.base_project_path,
                &["check-ref-format", "--branch", target_branch],
            )
            .await?
            .status
            .success()
        {
            return Err(WorktreeError::InvalidBranch(target_branch.to_owned()));
        }
        if !session.branch_name.starts_with("ryoko/")
            || !self.branch_exists(&session.branch_name).await?
        {
            return Err(WorktreeError::NotPublished(session.branch_name.clone()));
        }

        let source_ref = format!("refs/heads/{}", session.branch_name);
        let source_head = self
            .checked(
                "resolve source branch",
                &["rev-parse", &source_ref],
                &self.base_project_path,
            )
            .await?;
        let target_ref = format!("refs/heads/{target_branch}");
        let target_tip = self
            .checked(
                "resolve target branch",
                &["rev-parse", &target_ref],
                &self.base_project_path,
            )
            .await?;

        let already_merged = self
            .git(
                &self.base_project_path,
                &["merge-base", "--is-ancestor", &source_head, &target_tip],
            )
            .await?
            .status
            .success();
        if already_merged {
            // A crash after update-ref but before the state write leaves a
            // published record for work that is already integrated. Reconcile
            // it on retry instead of keeping a stale open lease forever.
            if record.as_ref().map(|record| record.state) == Some(WorktreeState::Published) {
                self.mark_merged(session, &target_tip)?;
            }
            return Err(WorktreeError::NoChanges(session.branch_name.clone()));
        }

        let tree = self
            .git(
                &self.base_project_path,
                &[
                    "merge-tree",
                    "--write-tree",
                    "--name-only",
                    target_branch,
                    &session.branch_name,
                ],
            )
            .await?;
        if !tree.status.success() {
            return Err(WorktreeError::MergeConflict {
                branch: session.branch_name.clone(),
                details: String::from_utf8_lossy(&tree.stdout).trim().to_owned(),
            });
        }
        let tree_id = String::from_utf8_lossy(&tree.stdout)
            .lines()
            .next()
            .unwrap_or_default()
            .trim()
            .to_owned();
        if tree_id.is_empty() || !tree_id.chars().all(|c| c.is_ascii_hexdigit()) {
            return Err(WorktreeError::Git {
                action: "merge-tree".to_owned(),
                status: "parse".to_owned(),
                stderr: "missing tree id".to_owned(),
            });
        }

        let message = format!(
            "Ryoko merge: {} into {}",
            session.branch_name, target_branch
        );
        let commit = self
            .git(
                &self.base_project_path,
                &[
                    "commit-tree",
                    &tree_id,
                    "-p",
                    &target_tip,
                    "-p",
                    &source_head,
                    "-m",
                    &message,
                ],
            )
            .await?;
        if !commit.status.success() {
            return Err(self.git_error("create merge commit", commit));
        }
        let merge_commit = String::from_utf8_lossy(&commit.stdout).trim().to_owned();

        let checkout = self
            .worktree_checkout(target_branch)
            .await?
            .map(|(path, _)| path);
        if let Some(path) = &checkout
            && path != &self.base_project_path
        {
            return Err(WorktreeError::TargetCheckedOut(path.display().to_string()));
        }
        if checkout.is_some() && self.is_dirty_tracked(&self.base_project_path).await? {
            return Err(WorktreeError::DirtyMainCheckout);
        }

        let update = self
            .git(
                &self.base_project_path,
                &[
                    "update-ref",
                    "-m",
                    "ryoko: merge validated agent worktree",
                    &target_ref,
                    &merge_commit,
                    &target_tip,
                ],
            )
            .await?;
        if !update.status.success() {
            return Err(self.git_error("update target branch", update));
        }

        if checkout.is_some() {
            let reset = self
                .git(&self.base_project_path, &["reset", "--hard", &merge_commit])
                .await?;
            if !reset.status.success() {
                return Err(self.git_error("sync main checkout", reset));
            }
        }

        self.mark_merged(session, &merge_commit)?;

        Ok(MergeReceipt {
            target_branch: target_branch.to_owned(),
            previous_tip: target_tip,
            merge_commit,
            source_branch: session.branch_name.clone(),
        })
    }

    /// Returns every active or published lease, ordered by creation time. A
    /// daemon can rebuild its in-memory scheduler from this after a restart.
    pub fn open_sessions(&self) -> Result<Vec<WorktreeRecord>, WorktreeError> {
        self.with_state(|connection| {
            let mut statement = connection.prepare(
                "SELECT task_id, agent_id, branch_name, path, base_commit, base_branch, \
                 state, head_commit, merge_commit, summary, files_json, patch, updated_at \
                 FROM worktree_sessions WHERE state IN ('active', 'published') \
                 ORDER BY created_at, task_id",
            )?;
            let rows = statement.query_map([], record_from_row)?;
            rows.collect::<rusqlite::Result<Vec<_>>>()
        })
    }

    /// Loads one durable lease record, if it exists.
    pub fn load_session(&self, id: Uuid) -> Result<Option<WorktreeRecord>, WorktreeError> {
        let task_id = id.to_string();
        self.with_state(|connection| {
            connection
                .query_row(
                    "SELECT task_id, agent_id, branch_name, path, base_commit, base_branch, \
                     state, head_commit, merge_commit, summary, files_json, patch, updated_at \
                     FROM worktree_sessions WHERE task_id = ?1",
                    params![task_id],
                    record_from_row,
                )
                .optional()
        })
    }

    fn save_session(&self, session: &WorktreeSession) -> Result<(), WorktreeError> {
        let task_id = session.id.to_string();
        let path = session.path.to_string_lossy().into_owned();
        let now = unix_now();
        self.with_state(|connection| {
            connection
                .execute(
                    "INSERT INTO worktree_sessions (
                        task_id, agent_id, branch_name, path, base_commit, base_branch,
                        state, head_commit, merge_commit, summary, files_json, patch,
                        created_at, updated_at
                     ) VALUES (
                        ?1, ?2, ?3, ?4, ?5, ?6, 'active', NULL, NULL, NULL, '[]', NULL,
                        ?7, ?7
                     )
                     ON CONFLICT(task_id) DO UPDATE SET
                        agent_id = excluded.agent_id,
                        branch_name = excluded.branch_name,
                        path = excluded.path,
                        base_commit = excluded.base_commit,
                        base_branch = excluded.base_branch,
                        state = 'active',
                        head_commit = NULL,
                        merge_commit = NULL,
                        summary = NULL,
                        files_json = '[]',
                        patch = NULL,
                        updated_at = excluded.updated_at",
                    params![
                        task_id,
                        session.agent_id,
                        session.branch_name,
                        path,
                        session.base_commit,
                        session.base_branch,
                        now
                    ],
                )
                .map(|_| ())
        })
    }

    fn save_published(
        &self,
        session: &WorktreeSession,
        pull_request: &WorktreePullRequest,
        head_commit: &str,
    ) -> Result<(), WorktreeError> {
        let task_id = session.id.to_string();
        let path = session.path.to_string_lossy().into_owned();
        let files = serde_json::to_string(&pull_request.files)
            .map_err(|error| WorktreeError::State(error.to_string()))?;
        let now = unix_now();
        self.with_state(|connection| {
            connection
                .execute(
                    "INSERT INTO worktree_sessions (
                        task_id, agent_id, branch_name, path, base_commit, base_branch,
                        state, head_commit, merge_commit, summary, files_json, patch,
                        created_at, updated_at
                     ) VALUES (
                        ?1, ?2, ?3, ?4, ?5, ?6, 'published', ?7, NULL, ?8, ?9, ?10, ?11, ?11
                     )
                     ON CONFLICT(task_id) DO UPDATE SET
                        agent_id = excluded.agent_id,
                        branch_name = excluded.branch_name,
                        path = excluded.path,
                        base_commit = excluded.base_commit,
                        base_branch = excluded.base_branch,
                        state = 'published',
                        head_commit = excluded.head_commit,
                        merge_commit = NULL,
                        summary = excluded.summary,
                        files_json = excluded.files_json,
                        patch = excluded.patch,
                        updated_at = excluded.updated_at",
                    params![
                        task_id,
                        session.agent_id,
                        session.branch_name,
                        path,
                        session.base_commit,
                        session.base_branch,
                        head_commit,
                        pull_request.summary,
                        files,
                        pull_request.patch,
                        now
                    ],
                )
                .map(|_| ())
        })
    }

    fn mark_merged(
        &self,
        session: &WorktreeSession,
        merge_commit: &str,
    ) -> Result<(), WorktreeError> {
        self.ensure_record(session)?;
        let task_id = session.id.to_string();
        let now = unix_now();
        self.with_state(|connection| {
            connection
                .execute(
                    "UPDATE worktree_sessions
                     SET state = 'merged', merge_commit = ?2, updated_at = ?3
                     WHERE task_id = ?1",
                    params![task_id, merge_commit, now],
                )
                .map(|_| ())
        })
    }

    fn mark_state(
        &self,
        session: &WorktreeSession,
        state: WorktreeState,
    ) -> Result<(), WorktreeError> {
        self.ensure_record(session)?;
        let task_id = session.id.to_string();
        let now = unix_now();
        let state = state.as_str();
        self.with_state(|connection| {
            connection
                .execute(
                    "UPDATE worktree_sessions SET state = ?2, updated_at = ?3 WHERE task_id = ?1",
                    params![task_id, state, now],
                )
                .map(|_| ())
        })
    }

    fn ensure_record(&self, session: &WorktreeSession) -> Result<(), WorktreeError> {
        if self.load_session(session.id)?.is_none() {
            self.save_session(session)?;
        }
        Ok(())
    }

    fn with_state<T>(
        &self,
        action: impl FnOnce(&Connection) -> rusqlite::Result<T>,
    ) -> Result<T, WorktreeError> {
        let mut guard = self.state.lock().unwrap_or_else(|error| error.into_inner());
        if guard.is_none() {
            if let Some(parent) = self.state_path.parent() {
                std::fs::create_dir_all(parent)
                    .map_err(|error| WorktreeError::State(error.to_string()))?;
            }
            let connection = Connection::open(&self.state_path).map_err(|error| {
                WorktreeError::State(format!("open {}: {error}", self.state_path.display()))
            })?;
            connection
                .busy_timeout(Duration::from_secs(5))
                .map_err(|error| WorktreeError::State(error.to_string()))?;
            connection
                .execute_batch(STATE_SCHEMA)
                .map_err(|error| WorktreeError::State(error.to_string()))?;
            *guard = Some(connection);
        }
        let connection = guard
            .as_ref()
            .ok_or_else(|| WorktreeError::State("state store not initialized".to_owned()))?;
        action(connection).map_err(|error| WorktreeError::State(error.to_string()))
    }

    fn ensure_managed_session(&self, session: &WorktreeSession) -> Result<(), WorktreeError> {
        if !session.path.starts_with(&self.worktrees_root) {
            return Err(WorktreeError::InvalidAgentId(
                "session path escaped .ryoko/worktrees".to_owned(),
            ));
        }
        self.ensure_managed_branch(&session.branch_name)
    }

    fn ensure_managed_branch(&self, branch: &str) -> Result<(), WorktreeError> {
        if !branch.starts_with("ryoko/") {
            return Err(WorktreeError::InvalidBranch(branch.to_owned()));
        }
        Ok(())
    }

    async fn remove_worktree_dir(&self, session: &WorktreeSession) -> Result<(), WorktreeError> {
        if session.path.exists() {
            let path = session.path.to_string_lossy().into_owned();
            let output = self
                .git(
                    &self.base_project_path,
                    &["worktree", "remove", "--force", &path],
                )
                .await?;
            if !output.status.success() {
                return Err(self.git_error("remove worktree", output));
            }
        }
        let _ = self
            .git(&self.base_project_path, &["worktree", "prune"])
            .await;
        Ok(())
    }

    async fn delete_managed_branch(&self, branch: &str) -> Result<(), WorktreeError> {
        self.ensure_managed_branch(branch)?;
        if self.branch_exists(branch).await? {
            let output = self
                .git(&self.base_project_path, &["branch", "-D", branch])
                .await?;
            if !output.status.success() {
                return Err(self.git_error("delete worktree branch", output));
            }
        }
        Ok(())
    }

    async fn ensure_repository(&self) -> Result<(), WorktreeError> {
        let output = self
            .git(
                &self.base_project_path,
                &["rev-parse", "--is-inside-work-tree"],
            )
            .await?;
        if !output.status.success() || String::from_utf8_lossy(&output.stdout).trim() != "true" {
            return Err(WorktreeError::NotARepository(
                self.base_project_path.display().to_string(),
            ));
        }
        Ok(())
    }

    async fn ensure_worktree(&self, session: &WorktreeSession) -> Result<(), WorktreeError> {
        if !session.path.starts_with(&self.worktrees_root) || !session.path.exists() {
            return Err(WorktreeError::PathExists(format!(
                "worktree `{}` is missing or outside .ryoko/worktrees",
                session.path.display()
            )));
        }
        Ok(())
    }

    async fn branch_exists(&self, branch: &str) -> Result<bool, WorktreeError> {
        let output = self
            .git(
                &self.base_project_path,
                &[
                    "show-ref",
                    "--verify",
                    "--quiet",
                    &format!("refs/heads/{branch}"),
                ],
            )
            .await?;
        Ok(output.status.success())
    }

    async fn is_dirty(&self, path: &Path) -> Result<bool, WorktreeError> {
        let output = self
            .checked("inspect worktree status", &["status", "--porcelain"], path)
            .await?;
        Ok(!output.is_empty())
    }

    async fn is_dirty_tracked(&self, path: &Path) -> Result<bool, WorktreeError> {
        let output = self
            .checked(
                "inspect tracked changes",
                &["status", "--porcelain", "--untracked-files=no"],
                path,
            )
            .await?;
        Ok(!output.is_empty())
    }

    async fn worktree_checkout(
        &self,
        branch: &str,
    ) -> Result<Option<(PathBuf, String)>, WorktreeError> {
        let output = self
            .checked(
                "list worktrees",
                &["worktree", "list", "--porcelain"],
                &self.base_project_path,
            )
            .await?;
        let target = format!("refs/heads/{branch}");
        let mut path: Option<PathBuf> = None;
        for line in output.lines() {
            if let Some(value) = line.strip_prefix("worktree ") {
                path = Some(PathBuf::from(value));
            } else if line == format!("branch {target}")
                && let Some(path) = path.take()
            {
                return Ok(Some((path, branch.to_owned())));
            } else if line.trim().is_empty() {
                path = None;
            }
        }
        Ok(None)
    }

    async fn checked(
        &self,
        action: &str,
        args: &[&str],
        cwd: &Path,
    ) -> Result<String, WorktreeError> {
        let output = self.git(cwd, args).await?;
        if !output.status.success() {
            return Err(self.git_error(action, output));
        }
        Ok(String::from_utf8_lossy(&output.stdout).trim().to_owned())
    }

    async fn git(&self, cwd: &Path, args: &[&str]) -> Result<Output, WorktreeError> {
        Command::new("git")
            .current_dir(cwd)
            .args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .env("GIT_TERMINAL_PROMPT", "0")
            .env("LC_ALL", "C")
            .env("GIT_AUTHOR_NAME", "Ryoko Agent")
            .env("GIT_AUTHOR_EMAIL", "agent@ryoko.local")
            .env("GIT_COMMITTER_NAME", "Ryoko Agent")
            .env("GIT_COMMITTER_EMAIL", "agent@ryoko.local")
            .kill_on_drop(true)
            .output()
            .await
            .map_err(WorktreeError::Io)
    }

    fn git_error(&self, action: &str, output: Output) -> WorktreeError {
        WorktreeError::Git {
            action: action.to_owned(),
            status: output
                .status
                .code()
                .map(|code| code.to_string())
                .unwrap_or_else(|| "signal".to_owned()),
            stderr: String::from_utf8_lossy(&output.stderr).trim().to_owned(),
        }
    }
}

fn record_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<WorktreeRecord> {
    let task_id: String = row.get(0)?;
    let id = Uuid::parse_str(&task_id).map_err(|error| conversion_error(0, error))?;
    let agent_id: String = row.get(1)?;
    let branch_name: String = row.get(2)?;
    let path: String = row.get(3)?;
    let base_commit: String = row.get(4)?;
    let base_branch: String = row.get(5)?;
    let state: String = row.get(6)?;
    let state = WorktreeState::parse(&state).ok_or_else(|| {
        conversion_error(
            6,
            std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!("unknown worktree state `{state}`"),
            ),
        )
    })?;
    let files_json: String = row.get(10)?;
    let files = serde_json::from_str(&files_json).map_err(|error| conversion_error(10, error))?;
    let updated_at: i64 = row.get(12)?;
    Ok(WorktreeRecord {
        session: WorktreeSession {
            id,
            agent_id,
            branch_name,
            path: PathBuf::from(path),
            base_commit,
            base_branch,
        },
        state,
        head_commit: row.get(7)?,
        merge_commit: row.get(8)?,
        summary: row.get(9)?,
        files,
        patch: row.get(11)?,
        updated_at: updated_at.max(0) as u64,
    })
}

fn conversion_error(
    index: usize,
    error: impl std::error::Error + Send + Sync + 'static,
) -> rusqlite::Error {
    rusqlite::Error::FromSqlConversionFailure(index, rusqlite::types::Type::Text, Box::new(error))
}

fn unix_now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs() as i64)
        .unwrap_or(0)
}

fn sanitize_agent_id(agent_id: &str) -> Result<String, WorktreeError> {
    let mut slug = String::with_capacity(agent_id.len());
    let mut last_dash = false;
    for character in agent_id.chars() {
        if character.is_ascii_alphanumeric() || character == '_' {
            slug.push(character);
            last_dash = false;
        } else if !last_dash {
            slug.push('-');
            last_dash = true;
        }
    }
    let slug = slug.trim_matches('-').to_lowercase();
    if slug.is_empty() || slug.len() > 48 || slug == "." || slug == ".." {
        return Err(WorktreeError::InvalidAgentId(agent_id.to_owned()));
    }
    Ok(slug)
}

/// `WorkspaceProvider` adapter used by the orchestrator. Every task receives a
/// real Git worktree; publication goes through the WASM validator first.
#[async_trait]
impl WorkspaceProvider for GitWorktreeManager {
    async fn allocate(
        &self,
        task: &Task,
        agent: &AgentIdentity,
    ) -> CoreResult<Arc<dyn WorkspaceLease>> {
        let session = self
            .create_worktree(&agent.name, task.id)
            .await
            .map_err(CoreError::from)?;
        let root = tokio::fs::canonicalize(&session.path)
            .await
            .unwrap_or_else(|_| session.path.clone());
        Ok(Arc::new(GitWorktreeLease {
            manager: self.clone(),
            session,
            root,
            applied: Mutex::new(BTreeSet::new()),
        }))
    }
}

struct GitWorktreeLease {
    manager: GitWorktreeManager,
    session: WorktreeSession,
    root: PathBuf,
    applied: Mutex<BTreeSet<String>>,
}

#[async_trait]
impl WorkspaceLease for GitWorktreeLease {
    fn root(&self) -> &Path {
        &self.root
    }

    fn branch(&self) -> &str {
        &self.session.branch_name
    }

    async fn apply(&self, changes: &[ProposedChange]) -> CoreResult<()> {
        for change in changes {
            let relative = validate_relative_path(&change.path)?;
            let target = self.root.join(&relative);
            if let Some(parent) = target.parent() {
                tokio::fs::create_dir_all(parent)
                    .await
                    .map_err(|err| CoreError::Workspace(err.to_string()))?;
                let canonical_parent = tokio::fs::canonicalize(parent)
                    .await
                    .map_err(|err| CoreError::Workspace(err.to_string()))?;
                if !canonical_parent.starts_with(&self.root) {
                    return Err(CoreError::Workspace(format!(
                        "path escapes worktree: {}",
                        change.path
                    )));
                }
            }
            if let Ok(metadata) = tokio::fs::symlink_metadata(&target).await
                && metadata.file_type().is_symlink()
            {
                return Err(CoreError::Workspace(format!(
                    "refusing to write through symlink: {}",
                    change.path
                )));
            }
            tokio::fs::write(&target, &change.content)
                .await
                .map_err(|err| CoreError::Workspace(err.to_string()))?;
            self.applied
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .insert(change.path.clone());
        }
        Ok(())
    }

    async fn publish(&self, task: &Task, summary: &str) -> CoreResult<PullRequest> {
        let paths: Vec<String> = self
            .applied
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .iter()
            .cloned()
            .collect();
        let pr = self
            .manager
            .publish_worktree_inner(&self.session, summary, Some(&paths))
            .await
            .map_err(CoreError::from)?;
        self.applied
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .clear();
        Ok(PullRequest {
            id: pr.id.to_string(),
            task_id: task.id,
            branch: pr.source_branch,
            base_branch: pr.target_branch,
            files: pr.files,
            summary: pr.summary,
            patch: pr.patch,
        })
    }

    async fn discard(&self) -> CoreResult<()> {
        self.manager
            .clean_worktree(&self.session)
            .await
            .map_err(CoreError::from)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use ryoko_core::{
        AgentRole, AgentRuntime, AllowAllValidator, Orchestrator, OrchestratorConfig,
        ProjectContext, Task, TaskContext, TaskResult,
    };
    use ryoko_models::ModelRouter;
    use std::time::Duration;
    use tempfile::TempDir;

    async fn git(cwd: &Path, args: &[&str]) {
        let output = Command::new("git")
            .current_dir(cwd)
            .args(args)
            .env("GIT_AUTHOR_NAME", "Test")
            .env("GIT_AUTHOR_EMAIL", "test@example.com")
            .env("GIT_COMMITTER_NAME", "Test")
            .env("GIT_COMMITTER_EMAIL", "test@example.com")
            .output()
            .await
            .unwrap();
        assert!(
            output.status.success(),
            "git {args:?} failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    async fn repo() -> (TempDir, GitWorktreeManager) {
        let dir = tempfile::tempdir().unwrap();
        git(dir.path(), &["init", "-b", "main"]).await;
        git(dir.path(), &["config", "user.name", "Test"]).await;
        git(dir.path(), &["config", "user.email", "test@example.com"]).await;
        tokio::fs::write(dir.path().join("README.md"), "# project\n")
            .await
            .unwrap();
        git(dir.path(), &["add", "-A"]).await;
        git(dir.path(), &["commit", "-m", "initial"]).await;
        let manager = GitWorktreeManager::new(dir.path());
        (dir, manager)
    }

    fn approved() -> ValidationReport {
        ValidationReport::passed(
            "cargo test passed",
            vec!["5 passed".to_owned()],
            Duration::ZERO,
        )
    }

    #[tokio::test]
    async fn isolates_publishes_merges_and_cleans_worktree() {
        let (dir, manager) = repo().await;
        let task = Task::new("add feature", AgentRole::SeniorDeveloper);
        let identity = AgentIdentity::new(AgentRole::SeniorDeveloper, "dev-01", "developer");
        let lease = manager.allocate(&task, &identity).await.unwrap();

        lease
            .apply(&[ProposedChange::new(
                "src/feature.rs",
                "pub fn feature() {}\n",
            )])
            .await
            .unwrap();
        assert!(!dir.path().join("src/feature.rs").exists());
        assert!(lease.root().join("src/feature.rs").exists());

        // Pre-flight build output is untracked and must not be committed as
        // part of the agent proposal.
        tokio::fs::create_dir_all(lease.root().join("target"))
            .await
            .unwrap();
        tokio::fs::write(lease.root().join("target/artifact"), "build output\n")
            .await
            .unwrap();

        let pr = lease.publish(&task, "add feature").await.unwrap();
        assert_eq!(pr.files, vec!["src/feature.rs"]);
        assert!(!pr.files.iter().any(|path| path.starts_with("target/")));
        assert!(pr.patch.contains("+pub fn feature() {}"));

        let session = WorktreeSession {
            id: task.id,
            agent_id: "dev-01".to_owned(),
            branch_name: pr.branch.clone(),
            path: lease.root().to_path_buf(),
            base_commit: manager
                .checked("head", &["rev-parse", "HEAD"], dir.path())
                .await
                .unwrap(),
            base_branch: "main".to_owned(),
        };
        manager
            .merge_worktree(&session, "main", &approved())
            .await
            .unwrap();
        assert_eq!(
            tokio::fs::read_to_string(dir.path().join("src/feature.rs"))
                .await
                .unwrap(),
            "pub fn feature() {}\n"
        );

        lease.discard().await.unwrap();
        assert!(!lease.root().exists());
        assert!(!dir.path().join("target/artifact").exists());
        let branches = manager
            .checked("branches", &["branch", "--list", "ryoko/*"], dir.path())
            .await
            .unwrap();
        assert!(branches.is_empty());
    }

    #[tokio::test]
    async fn published_branch_survives_cleanup_and_manager_restart() {
        let (dir, manager) = repo().await;
        let task = Task::new("durable feature", AgentRole::SeniorDeveloper);
        let session = manager.create_worktree("dev-01", task.id).await.unwrap();
        tokio::fs::write(session.path.join("durable.rs"), "pub fn durable() {}\n")
            .await
            .unwrap();
        let pull_request = manager
            .publish_worktree(&session, "persist the lease")
            .await
            .unwrap();
        assert_eq!(
            manager.load_session(task.id).unwrap().unwrap().state,
            WorktreeState::Published
        );

        // The orchestrator calls discard after a successful publish. That must
        // free the worktree without deleting the branch needed for review.
        manager.clean_worktree(&session).await.unwrap();
        manager.clean_worktree(&session).await.unwrap();
        assert!(!session.path.exists());
        assert!(manager.branch_exists(&session.branch_name).await.unwrap());
        assert_eq!(manager.open_sessions().unwrap().len(), 1);

        // A released published lease can be reattached for inspection without
        // losing the branch or its review evidence.
        let resumed = manager.resume_worktree(task.id).await.unwrap();
        assert!(resumed.path.join("durable.rs").exists());
        manager.release_worktree(&resumed).await.unwrap();
        assert!(!resumed.path.exists());

        // A fresh manager simulates a daemon restart: the branch and its
        // review evidence are recoverable from SQLite.
        let restored = GitWorktreeManager::new(dir.path());
        let record = restored.load_session(task.id).unwrap().unwrap();
        assert_eq!(record.state, WorktreeState::Published);
        assert_eq!(record.patch.as_deref(), Some(pull_request.patch.as_str()));
        restored
            .merge_worktree(&record.session, "main", &approved())
            .await
            .unwrap();
        assert_eq!(
            tokio::fs::read_to_string(dir.path().join("durable.rs"))
                .await
                .unwrap(),
            "pub fn durable() {}\n"
        );
        let merged = restored.load_session(task.id).unwrap().unwrap();
        assert_eq!(merged.state, WorktreeState::Merged);
        assert!(merged.merge_commit.is_some());

        // Cleanup after merge may finally delete the surviving branch.
        restored.clean_worktree(&record.session).await.unwrap();
        assert!(!restored.branch_exists(&session.branch_name).await.unwrap());
    }

    #[tokio::test]
    async fn renders_review_diff_and_resets_failed_revisions() {
        let (_dir, manager) = repo().await;
        let task = Task::new("review me", AgentRole::SeniorDeveloper);
        let session = manager.create_worktree("dev-01", task.id).await.unwrap();
        tokio::fs::write(session.path.join("feature.rs"), "pub fn feature() {}\n")
            .await
            .unwrap();
        manager
            .publish_worktree(&session, "add feature")
            .await
            .unwrap();

        let diff = manager.diff_worktree(&session, "main").await.unwrap();
        assert_eq!(diff.files, vec!["feature.rs"]);
        assert!(diff.patch.contains("+pub fn feature() {}"));

        tokio::fs::write(session.path.join("dirty.rs"), "uncommitted\n")
            .await
            .unwrap();
        assert!(manager.is_dirty(&session.path).await.unwrap());
        manager.reset_worktree(&session).await.unwrap();
        assert!(!manager.is_dirty(&session.path).await.unwrap());
        assert!(!session.path.join("dirty.rs").exists());

        manager.clean_worktree(&session).await.unwrap();
    }

    #[tokio::test]
    async fn failed_approval_cannot_merge() {
        let (_dir, manager) = repo().await;
        let task = Task::new("reject me", AgentRole::SeniorDeveloper);
        let session = manager.create_worktree("dev-01", task.id).await.unwrap();
        tokio::fs::write(session.path.join("bad.rs"), "nope")
            .await
            .unwrap();
        manager.publish_worktree(&session, "bad").await.unwrap();
        let rejected =
            ValidationReport::failed("tests failed", vec!["FAILED".to_owned()], Duration::ZERO);
        let error = manager
            .merge_worktree(&session, "main", &rejected)
            .await
            .unwrap_err();
        assert!(matches!(error, WorktreeError::NotApproved(_)));
    }

    #[tokio::test]
    async fn discarding_leaves_main_checkout_untouched() {
        let (dir, manager) = repo().await;
        let task = Task::new("experiment", AgentRole::SeniorDeveloper);
        let session = manager.create_worktree("dev-02", task.id).await.unwrap();
        tokio::fs::write(session.path.join("scratch.txt"), "temporary")
            .await
            .unwrap();
        manager.clean_worktree(&session).await.unwrap();
        assert!(!dir.path().join("scratch.txt").exists());
        assert!(!session.path.exists());
        assert_eq!(
            manager.load_session(task.id).unwrap().unwrap().state,
            WorktreeState::Abandoned
        );
        assert!(!manager.branch_exists(&session.branch_name).await.unwrap());
    }

    #[tokio::test]
    async fn rejects_path_traversal_in_lease() {
        let (_dir, manager) = repo().await;
        let task = Task::new("escape", AgentRole::SeniorDeveloper);
        let identity = AgentIdentity::new(AgentRole::SeniorDeveloper, "dev-03", "developer");
        let lease = manager.allocate(&task, &identity).await.unwrap();
        assert!(
            lease
                .apply(&[ProposedChange::new("../escape.rs", "bad")])
                .await
                .is_err()
        );
        lease.discard().await.unwrap();
    }

    #[tokio::test]
    async fn agent_ids_are_sanitized() {
        assert_eq!(sanitize_agent_id("senior dev/01").unwrap(), "senior-dev-01");
        assert_eq!(sanitize_agent_id("../../etc").unwrap(), "etc");
        assert!(sanitize_agent_id("").is_err());
    }

    #[tokio::test]
    async fn parallel_agents_receive_distinct_worktrees() {
        let (_dir, manager) = repo().await;
        let (left, right) = tokio::join!(
            manager.create_worktree("dev-01", Uuid::new_v4()),
            manager.create_worktree("dev-02", Uuid::new_v4())
        );
        let left = left.unwrap();
        let right = right.unwrap();
        assert_ne!(left.path, right.path);
        assert_ne!(left.branch_name, right.branch_name);
        assert!(left.path.join("README.md").exists());
        assert!(right.path.join("README.md").exists());
        manager.clean_worktree(&left).await.unwrap();
        manager.clean_worktree(&right).await.unwrap();
    }

    #[tokio::test]
    async fn conflicting_merge_is_rejected_without_moving_target() {
        let (dir, manager) = repo().await;
        let left = manager
            .create_worktree("dev-01", Uuid::new_v4())
            .await
            .unwrap();
        let right = manager
            .create_worktree("dev-02", Uuid::new_v4())
            .await
            .unwrap();
        tokio::fs::write(left.path.join("shared.txt"), "left\n")
            .await
            .unwrap();
        tokio::fs::write(right.path.join("shared.txt"), "right\n")
            .await
            .unwrap();
        manager.publish_worktree(&left, "left").await.unwrap();
        manager.publish_worktree(&right, "right").await.unwrap();

        manager
            .merge_worktree(&left, "main", &approved())
            .await
            .unwrap();
        let tip_before = manager
            .checked("target tip", &["rev-parse", "main"], dir.path())
            .await
            .unwrap();
        let error = manager
            .merge_worktree(&right, "main", &approved())
            .await
            .unwrap_err();
        assert!(matches!(error, WorktreeError::MergeConflict { .. }));
        assert_eq!(
            manager
                .checked("target tip", &["rev-parse", "main"], dir.path())
                .await
                .unwrap(),
            tip_before
        );
        assert_eq!(
            tokio::fs::read_to_string(dir.path().join("shared.txt"))
                .await
                .unwrap(),
            "left\n"
        );
        manager.clean_worktree(&left).await.unwrap();
        manager.clean_worktree(&right).await.unwrap();
    }

    struct StaticAgent {
        identity: AgentIdentity,
    }

    #[async_trait]
    impl AgentRuntime for StaticAgent {
        fn identity(&self) -> AgentIdentity {
            self.identity.clone()
        }

        async fn execute_task(&self, _ctx: TaskContext, task: Task) -> CoreResult<TaskResult> {
            Ok(
                TaskResult::success(task.id, "done").with_changes(vec![ProposedChange::new(
                    "src/orchestrated.rs",
                    "pub fn orchestrated() {}\n",
                )]),
            )
        }
    }

    #[tokio::test]
    async fn orchestrator_keeps_published_branch_after_discard() {
        let (dir, manager) = repo().await;
        let task = Task::new("orchestrated task", AgentRole::SeniorDeveloper);
        let task_id = task.id;
        let agent: Arc<dyn AgentRuntime> = Arc::new(StaticAgent {
            identity: AgentIdentity::new(AgentRole::SeniorDeveloper, "dev-01", "developer"),
        });
        let orchestrator = Orchestrator::new(
            Arc::new(manager.clone()),
            Arc::new(AllowAllValidator),
            Arc::new(ModelRouter::new("mock")),
        )
        .with_config(OrchestratorConfig {
            max_concurrency: 1,
            max_attempts: 1,
        });

        let report = orchestrator
            .run(vec![task], vec![agent], ProjectContext::default())
            .await
            .unwrap();
        assert!(report.all_succeeded());
        let branch = report.succeeded[0].branch.clone();
        let record = manager.load_session(task_id).unwrap().unwrap();
        assert_eq!(record.state, WorktreeState::Published);
        assert!(!record.session.path.exists());
        assert!(manager.branch_exists(&branch).await.unwrap());

        manager
            .merge_worktree(&record.session, "main", &approved())
            .await
            .unwrap();
        assert_eq!(
            tokio::fs::read_to_string(dir.path().join("src/orchestrated.rs"))
                .await
                .unwrap(),
            "pub fn orchestrated() {}\n"
        );
    }
}
