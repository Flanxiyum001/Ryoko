//! Isolated Git worktree leases for parallel agents.
//!
//! Each task gets a hidden branch and directory under `.ryoko/worktrees`. The
//! main editor checkout is never modified until an approved merge is committed.
//! All Git calls are `tokio::process::Command` invocations with piped output;
//! no shell is involved and no subprocess blocks the async reactor.

use std::path::{Path, PathBuf};
use std::process::{Output, Stdio};
use std::sync::Arc;

use async_trait::async_trait;
use ryoko_core::workspace::validate_relative_path;
use ryoko_core::{
    AgentIdentity, CoreError, CoreResult, ProposedChange, PullRequest, Task, ValidationReport,
    WorkspaceLease, WorkspaceProvider,
};
use serde::{Deserialize, Serialize};
use tokio::process::Command;
use uuid::Uuid;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorktreeSession {
    pub id: Uuid,
    pub agent_id: String,
    pub branch_name: String,
    pub path: PathBuf,
    pub base_commit: String,
    pub base_branch: String,
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

#[derive(Debug, Clone)]
pub struct GitWorktreeManager {
    base_project_path: PathBuf,
    worktrees_root: PathBuf,
}

impl GitWorktreeManager {
    pub fn new(project_path: impl AsRef<Path>) -> Self {
        let base = std::fs::canonicalize(project_path.as_ref())
            .unwrap_or_else(|_| project_path.as_ref().to_path_buf());
        Self {
            worktrees_root: base.join(".ryoko").join("worktrees"),
            base_project_path: base,
        }
    }

    pub fn base_project_path(&self) -> &Path {
        &self.base_project_path
    }

    pub fn worktrees_root(&self) -> &Path {
        &self.worktrees_root
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

        Ok(WorktreeSession {
            id: task_id,
            agent_id: slug,
            branch_name,
            path: worktree_path,
            base_commit,
            base_branch,
        })
    }

    /// Removes the worktree, prunes metadata and deletes its branch. Idempotent.
    pub async fn clean_worktree(&self, session: &WorktreeSession) -> Result<(), WorktreeError> {
        if !session.path.starts_with(&self.worktrees_root) {
            return Err(WorktreeError::InvalidAgentId(
                "session path escaped .ryoko/worktrees".to_owned(),
            ));
        }
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
        if self.branch_exists(&session.branch_name).await? {
            let output = self
                .git(
                    &self.base_project_path,
                    &["branch", "-D", &session.branch_name],
                )
                .await?;
            if !output.status.success() {
                return Err(self.git_error("delete worktree branch", output));
            }
        }
        Ok(())
    }

    /// Commits every change in the isolated worktree and returns a reviewable
    /// patch. The main checkout is untouched.
    pub async fn publish_worktree(
        &self,
        session: &WorktreeSession,
        summary: &str,
    ) -> Result<WorktreePullRequest, WorktreeError> {
        self.ensure_worktree(session).await?;
        if self.is_dirty(&session.path).await? {
            self.checked("stage worktree changes", &["add", "-A"], &session.path)
                .await?;
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

        Ok(WorktreePullRequest {
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
        })
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
        self.ensure_worktree(session).await?;
        if self.is_dirty(&session.path).await? {
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

        let source_head = self
            .checked("resolve source HEAD", &["rev-parse", "HEAD"], &session.path)
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

        Ok(MergeReceipt {
            target_branch: target_branch.to_owned(),
            previous_tip: target_tip,
            merge_commit,
            source_branch: session.branch_name.clone(),
        })
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
        }))
    }
}

struct GitWorktreeLease {
    manager: GitWorktreeManager,
    session: WorktreeSession,
    root: PathBuf,
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
        }
        Ok(())
    }

    async fn publish(&self, task: &Task, summary: &str) -> CoreResult<PullRequest> {
        let pr = self
            .manager
            .publish_worktree(&self.session, summary)
            .await
            .map_err(CoreError::from)?;
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
    use ryoko_core::{AgentRole, Task};
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

        let pr = lease.publish(&task, "add feature").await.unwrap();
        assert_eq!(pr.files, vec!["src/feature.rs"]);
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
        let branches = manager
            .checked("branches", &["branch", "--list", "ryoko/*"], dir.path())
            .await
            .unwrap();
        assert!(branches.is_empty());
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
}
