use std::collections::BTreeMap;
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, RwLock};

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::error::{CoreError, CoreResult};
use crate::model::{AgentIdentity, ProposedChange, Task};

/// An internal pull request produced from a validated workspace lease. Ryoko
/// never writes directly to the integration branch; a human or an explicit
/// merge command consumes this object.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PullRequest {
    pub id: String,
    pub task_id: Uuid,
    pub branch: String,
    pub base_branch: String,
    pub files: Vec<String>,
    pub summary: String,
    pub patch: String,
}

/// A task-scoped copy of the project. Implementations must prevent one agent's
/// writes from being visible to another running task.
#[async_trait]
pub trait WorkspaceLease: Send + Sync {
    fn root(&self) -> &Path;
    fn branch(&self) -> &str;

    /// Writes changes into the isolated workspace (not the project root).
    async fn apply(&self, changes: &[ProposedChange]) -> CoreResult<()>;

    /// Commits the staged changes and returns a reviewable pull request.
    async fn publish(&self, task: &Task, summary: &str) -> CoreResult<PullRequest>;

    /// Removes the workspace and its branch. Must be idempotent.
    async fn discard(&self) -> CoreResult<()>;
}

/// Creates one [`WorkspaceLease`] per task.
#[async_trait]
pub trait WorkspaceProvider: Send + Sync {
    async fn allocate(
        &self,
        task: &Task,
        agent: &AgentIdentity,
    ) -> CoreResult<Arc<dyn WorkspaceLease>>;
}

/// Rejects absolute paths, parent traversal and Windows prefixes. Every
/// workspace implementation funnels agent-supplied paths through this.
pub fn validate_relative_path(path: &str) -> CoreResult<PathBuf> {
    if path.trim().is_empty() {
        return Err(CoreError::Workspace("empty change path".to_owned()));
    }
    // Reject cross-platform drive prefixes, alternate streams and separators even
    // when validating a Windows-targeted project on a Unix host.
    if path.contains('\\') || path.contains(':') || path.contains('\0') {
        return Err(CoreError::Workspace(
            "unsafe path separator or prefix".to_owned(),
        ));
    }
    let candidate = Path::new(path);
    if candidate.is_absolute() {
        return Err(CoreError::Workspace(format!(
            "absolute paths are not allowed: {path}"
        )));
    }
    let mut clean = PathBuf::new();
    for component in candidate.components() {
        match component {
            Component::Normal(part) => {
                let name = part.to_string_lossy();
                if name.eq_ignore_ascii_case(".git") || name.eq_ignore_ascii_case(".ryoko") {
                    return Err(CoreError::Workspace(
                        "writing Git/Ryoko metadata is forbidden".to_owned(),
                    ));
                }
                clean.push(part);
            }
            Component::CurDir => {}
            Component::ParentDir => {
                return Err(CoreError::Workspace(format!(
                    "parent traversal is not allowed: {path}"
                )));
            }
            Component::RootDir | Component::Prefix(_) => {
                return Err(CoreError::Workspace(format!(
                    "absolute paths are not allowed: {path}"
                )));
            }
        }
    }
    if clean.as_os_str().is_empty() {
        return Err(CoreError::Workspace(format!("invalid change path: {path}")));
    }
    Ok(clean)
}

/// Lightweight provider used by tests, the CLI demo and preview mode when a Git
/// repository is not available. It exercises the full apply/validate/publish
/// flow without touching disk.
#[derive(Debug, Default)]
pub struct InMemoryWorkspaceProvider {
    base_branch: String,
    counter: AtomicU64,
}

impl InMemoryWorkspaceProvider {
    pub fn new(base_branch: impl Into<String>) -> Self {
        Self {
            base_branch: base_branch.into(),
            counter: AtomicU64::new(0),
        }
    }
}

#[async_trait]
impl WorkspaceProvider for InMemoryWorkspaceProvider {
    async fn allocate(
        &self,
        task: &Task,
        _agent: &AgentIdentity,
    ) -> CoreResult<Arc<dyn WorkspaceLease>> {
        let sequence = self.counter.fetch_add(1, Ordering::Relaxed) + 1;
        let short = task.id.simple().to_string();
        let branch = format!("ryoko/{}-{sequence}", &short[..8]);
        Ok(Arc::new(InMemoryLease {
            id: sequence,
            branch,
            base_branch: self.base_branch.clone(),
            root: PathBuf::from(format!("/ryoko/in-memory/lease-{sequence}")),
            files: RwLock::new(BTreeMap::new()),
            discarded: AtomicBool::new(false),
        }))
    }
}

#[derive(Debug)]
struct InMemoryLease {
    id: u64,
    branch: String,
    base_branch: String,
    root: PathBuf,
    files: RwLock<BTreeMap<String, ProposedChange>>,
    discarded: AtomicBool,
}

#[async_trait]
impl WorkspaceLease for InMemoryLease {
    fn root(&self) -> &Path {
        &self.root
    }

    fn branch(&self) -> &str {
        &self.branch
    }

    async fn apply(&self, changes: &[ProposedChange]) -> CoreResult<()> {
        if self.discarded.load(Ordering::Relaxed) {
            return Err(CoreError::Workspace("lease has been discarded".to_owned()));
        }
        for change in changes {
            validate_relative_path(&change.path)?;
        }
        let mut files = self.files.write().unwrap_or_else(|e| e.into_inner());
        for change in changes {
            files.insert(change.path.clone(), change.clone());
        }
        Ok(())
    }

    async fn publish(&self, task: &Task, summary: &str) -> CoreResult<PullRequest> {
        let files = self.files.read().unwrap_or_else(|e| e.into_inner());
        if files.is_empty() {
            return Err(CoreError::Workspace(
                "cannot publish an empty workspace".to_owned(),
            ));
        }
        let mut patch = String::new();
        for (path, change) in files.iter() {
            patch.push_str(&format!("--- a/{path}\n+++ b/{path}\n"));
            for line in change.content.lines() {
                patch.push('+');
                patch.push_str(line);
                patch.push('\n');
            }
        }
        Ok(PullRequest {
            id: format!("pr-{}-{}", self.id, task.id.simple()),
            task_id: task.id,
            branch: self.branch.clone(),
            base_branch: self.base_branch.clone(),
            files: files.keys().cloned().collect(),
            summary: summary.to_owned(),
            patch,
        })
    }

    async fn discard(&self) -> CoreResult<()> {
        self.discarded.store(true, Ordering::Relaxed);
        self.files
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .clear();
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_unsafe_paths() {
        assert!(validate_relative_path("src/main.rs").is_ok());
        assert!(validate_relative_path("./src/../lib.rs").is_err());
        assert!(validate_relative_path("/etc/passwd").is_err());
        assert!(validate_relative_path("../outside").is_err());
        assert!(validate_relative_path("").is_err());
        assert!(validate_relative_path(".git/config").is_err());
        assert!(validate_relative_path("src/.ryoko/settings").is_err());
        assert!(validate_relative_path("C:\\\\Windows\\\\win.ini").is_err());
        assert!(validate_relative_path("file.txt:alternate").is_err());
    }

    #[tokio::test]
    async fn applies_validates_and_publishes() {
        let provider = InMemoryWorkspaceProvider::new("main");
        let task = Task::new("add endpoint", crate::AgentRole::SeniorDeveloper);
        let identity =
            crate::AgentIdentity::new(crate::AgentRole::SeniorDeveloper, "dev", "developer agent");
        let lease = provider.allocate(&task, &identity).await.unwrap();

        lease
            .apply(&[ProposedChange::new("src/api.rs", "pub fn health() {}")])
            .await
            .unwrap();
        let pr = lease.publish(&task, "add endpoint").await.unwrap();
        assert_eq!(pr.files, vec!["src/api.rs"]);
        assert_eq!(pr.base_branch, "main");
        assert!(pr.patch.contains("+pub fn health() {}"));

        lease.discard().await.unwrap();
        let err = lease
            .apply(&[ProposedChange::new("src/late.rs", "")])
            .await
            .unwrap_err();
        assert!(matches!(err, CoreError::Workspace(_)));
    }
}
