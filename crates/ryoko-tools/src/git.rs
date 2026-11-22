//! Git tools for status, diff, history, and branch operations.

use async_trait::async_trait;
use ryoko_worktree::{GitWorktreeManager, WorktreeSession, WorktreeDiff};
use ryoko_core::tools::{
    Capability, CapabilitySet, ReadScope, Tool, ToolContext, ToolOutput,
    ToolRegistry, ToolError, ToolResult,
};
use ryoko_core::workspace::WorkspaceLease;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio::process::Command;

/// Input for Git tools.
#[derive(Debug, Clone, serde::Deserialize)]
#[serde(tag = "operation", rename_all = "snake_case")]
pub enum GitToolInput {
    /// Get Git status.
    Status {
        #[serde(default)]
        path: Option<String>,
    },
    /// Get diff between branches or worktree and target.
    Diff {
        #[serde(default)]
        target_branch: Option<String>,
        #[serde(default)]
        path: Option<String>,
    },
    /// Get commit history.
    Log {
        #[serde(default)]
        branch: Option<String>,
        #[serde(default = "default_limit")]
        limit: usize,
    },
    /// List branches.
    Branch {
        #[serde(default)]
        all: bool,
    },
    /// Show a specific commit.
    Show {
        commit: String,
    },
    /// Get the current branch name.
    CurrentBranch,
    /// Get the current commit hash.
    CurrentCommit,
}

fn default_limit() -> usize {
    20
}

/// Output from Git tools.
#[derive(Debug, Clone, serde::Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum GitToolOutput {
    Status {
        path: String,
        clean: bool,
        staged: Vec<String>,
        unstaged: Vec<String>,
        untracked: Vec<String>,
    },
    Diff {
        target_branch: String,
        files: Vec<String>,
        patch: String,
    },
    Log {
        commits: Vec<CommitInfo>,
    },
    Branch {
        branches: Vec<BranchInfo>,
        current: String,
    },
    Show {
        commit: String,
        author: String,
        date: String,
        message: String,
        files: Vec<String>,
        patch: String,
    },
    CurrentBranch {
        branch: String,
    },
    CurrentCommit {
        commit: String,
    },
}

/// Commit information.
#[derive(Debug, Clone, serde::Serialize)]
pub struct CommitInfo {
    pub hash: String,
    pub short_hash: String,
    pub author: String,
    pub date: String,
    pub message: String,
}

/// Branch information.
#[derive(Debug, Clone, serde::Serialize)]
pub struct BranchInfo {
    pub name: String,
    pub current: bool,
    pub upstream: Option<String>,
}

/// Git tool for repository operations within a worktree.
pub struct GitTool {
    workspace: Option<Arc<dyn WorkspaceLease>>,
    worktree_manager: Option<Arc<GitWorktreeManager>>,
    session: Option<WorktreeSession>,
}

impl GitTool {
    /// Create a new Git tool.
    pub fn new(workspace: Option<Arc<dyn WorkspaceLease>>) -> Self {
        Self {
            workspace,
            worktree_manager: None,
            session: None,
        }
    }

    /// Create a Git tool with a worktree manager and session.
    pub fn with_worktree(
        workspace: Option<Arc<dyn WorkspaceLease>>,
        manager: Arc<GitWorktreeManager>,
        session: WorktreeSession,
    ) -> Self {
        Self {
            workspace,
            worktree_manager: Some(manager),
            session: Some(session),
        }
    }

    /// Get the working directory.
    fn work_dir(&self, path: Option<&str>) -> PathBuf {
        let base = self.workspace.as_ref()
            .map(|w| w.root().to_path_buf())
            .unwrap_or_else(|| PathBuf::from("."));

        if let Some(p) = path {
            base.join(p)
        } else {
            base
        }
    }

    /// Run a git command.
    async fn run_git(&self, args: &[&str], cwd: &Path) -> ToolResult<String> {
        let output = Command::new("git")
            .current_dir(cwd)
            .args(args)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .env("GIT_TERMINAL_PROMPT", "0")
            .env("LC_ALL", "C")
            .output()
            .await
            .map_err(|e| ToolError::ExecutionFailed {
                tool: "git".to_owned(),
                message: e.to_string(),
            })?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(ToolError::ExecutionFailed {
                tool: "git".to_owned(),
                message: stderr.trim().to_owned(),
            });
        }

        Ok(String::from_utf8_lossy(&output.stdout).trim().to_owned())
    }

    /// Get status.
    async fn status(&self, context: &ToolContext, path: Option<&str>) -> ToolResult<GitToolOutput> {
        let cwd = self.work_dir(path);
        let output = self.run_git(&["status", "--porcelain", "-z"], &cwd).await?;

        let mut staged = Vec::new();
        let mut unstaged = Vec::new();
        let mut untracked = Vec::new();

        for entry in output.split('\0') {
            if entry.is_empty() { continue; }
            if entry.len() < 3 { continue; }

            let status = &entry[..2];
            let file = &entry[3..];

            match status {
                "??" => untracked.push(file.to_owned()),
                s if s.chars().next() != Some(' ') => staged.push(file.to_owned()),
                s if s.chars().nth(1) != Some(' ') => unstaged.push(file.to_owned()),
                _ => {}
            }
        }

        Ok(GitToolOutput::Status {
            path: cwd.display().to_string(),
            clean: staged.is_empty() && unstaged.is_empty() && untracked.is_empty(),
            staged,
            unstaged,
            untracked,
        })
    }

    /// Get diff.
    async fn diff(&self, context: &ToolContext, target_branch: Option<&str>, path: Option<&str>) -> ToolResult<GitToolOutput> {
        let cwd = self.work_dir(path);
        let target = target_branch.unwrap_or("main");

        // If we have a worktree session, use it for better diffs
        if let (Some(manager), Some(session)) = (&self.worktree_manager, &self.session) {
            let diff = manager.diff_worktree(session, target).await
                .map_err(|e| ToolError::ExecutionFailed {
                    tool: "git".to_owned(),
                    message: e.to_string(),
                })?;

            return Ok(GitToolOutput::Diff {
                target_branch: target.to_owned(),
                files: diff.files,
                patch: diff.patch,
            });
        }

        // Otherwise use standard git diff
        let files_output = self.run_git(&["diff", "--name-only", &format!("{}...HEAD", target)], &cwd).await?;
        let files = files_output.lines().map(String::from).collect();

        let patch = self.run_git(&["diff", "--binary", "--no-color", &format!("{}...HEAD", target)], &cwd).await?;

        Ok(GitToolOutput::Diff {
            target_branch: target.to_owned(),
            files,
            patch,
        })
    }

    /// Get log.
    async fn log(&self, context: &ToolContext, branch: Option<&str>, limit: usize) -> ToolResult<GitToolOutput> {
        let cwd = self.work_dir(None);
        let branch = branch.unwrap_or("HEAD");

        let output = self.run_git(&[
            "log",
            &format!("-{}", limit),
            "--pretty=format:%H|%h|%an|%ad|%s",
            "--date=rfc3339",
            branch,
        ], &cwd).await?;

        let mut commits = Vec::new();
        for line in output.lines() {
            let parts: Vec<&str> = line.split('|').collect();
            if parts.len() >= 5 {
                commits.push(CommitInfo {
                    hash: parts[0].to_owned(),
                    short_hash: parts[1].to_owned(),
                    author: parts[2].to_owned(),
                    date: parts[3].to_owned(),
                    message: parts[4].to_owned(),
                });
            }
        }

        Ok(GitToolOutput::Log { commits })
    }

    /// List branches.
    async fn branch(&self, context: &ToolContext, all: bool) -> ToolResult<GitToolOutput> {
        let cwd = self.work_dir(None);
        let args = if all {
            vec!["branch", "-a", "--format=%(refname:short)|%(upstream:short)"]
        } else {
            vec!["branch", "--format=%(refname:short)|%(upstream:short)"]
        };

        let output = self.run_git(&args, &cwd).await?;

        let mut branches = Vec::new();
        let mut current = String::new();

        for line in output.lines() {
            let parts: Vec<&str> = line.split('|').collect();
            let name = parts[0].trim_start_matches('*').trim();
            let is_current = line.starts_with('*');

            if is_current {
                current = name.to_owned();
            }

            branches.push(BranchInfo {
                name: name.to_owned(),
                current: is_current,
                upstream: if parts.len() > 1 && !parts[1].is_empty() {
                    Some(parts[1].to_owned())
                } else {
                    None
                },
            });
        }

        Ok(GitToolOutput::Branch { branches, current })
    }

    /// Show a commit.
    async fn show(&self, context: &ToolContext, commit: &str) -> ToolResult<GitToolOutput> {
        let cwd = self.work_dir(None);

        let info_output = self.run_git(&[
            "show",
            "--no-patch",
            "--pretty=format:%an|%ad|%s",
            "--date=rfc3339",
            commit,
        ], &cwd).await?;

        let info_parts: Vec<&str> = info_output.split('|').collect();
        if info_parts.len() < 3 {
            return Err(ToolError::ExecutionFailed {
                tool: "git".to_owned(),
                message: "could not parse commit info".to_owned(),
            });
        }

        let files_output = self.run_git(&["show", "--name-only", "--pretty=format:", commit], &cwd).await?;
        let files = files_output.lines()
            .filter(|l| !l.is_empty())
            .map(String::from)
            .collect();

        let patch = self.run_git(&["show", "--binary", "--no-color", commit], &cwd).await?;

        Ok(GitToolOutput::Show {
            commit: commit.to_owned(),
            author: info_parts[0].to_owned(),
            date: info_parts[1].to_owned(),
            message: info_parts[2].to_owned(),
            files,
            patch,
        })
    }

    /// Get current branch.
    async fn current_branch(&self, context: &ToolContext) -> ToolResult<GitToolOutput> {
        let cwd = self.work_dir(None);
        let branch = self.run_git(&["symbolic-ref", "--short", "HEAD"], &cwd).await?;
        Ok(GitToolOutput::CurrentBranch { branch })
    }

    /// Get current commit.
    async fn current_commit(&self, context: &ToolContext) -> ToolResult<GitToolOutput> {
        let cwd = self.work_dir(None);
        let commit = self.run_git(&["rev-parse", "HEAD"], &cwd).await?;
        Ok(GitToolOutput::CurrentCommit { commit })
    }
}

#[async_trait]
impl Tool for GitTool {
    fn name(&self) -> &str {
        "git"
    }

    fn description(&self) -> &str {
        "Git operations: status, diff, log, branches, show commits"
    }

    fn input_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "oneOf": [
                {
                    "properties": {
                        "operation": { "const": "status" },
                        "path": { "type": "string" }
                    },
                    "required": ["operation"]
                },
                {
                    "properties": {
                        "operation": { "const": "diff" },
                        "target_branch": { "type": "string" },
                        "path": { "type": "string" }
                    },
                    "required": ["operation"]
                },
                {
                    "properties": {
                        "operation": { "const": "log" },
                        "branch": { "type": "string" },
                        "limit": { "type": "integer", "default": 20, "minimum": 1, "maximum": 100 }
                    },
                    "required": ["operation"]
                },
                {
                    "properties": {
                        "operation": { "const": "branch" },
                        "all": { "type": "boolean", "default": false }
                    },
                    "required": ["operation"]
                },
                {
                    "properties": {
                        "operation": { "const": "show" },
                        "commit": { "type": "string" }
                    },
                    "required": ["operation", "commit"]
                },
                {
                    "properties": {
                        "operation": { "const": "current_branch" }
                    },
                    "required": ["operation"]
                },
                {
                    "properties": {
                        "operation": { "const": "current_commit" }
                    },
                    "required": ["operation"]
                }
            ]
        })
    }

    fn required_capabilities(&self) -> CapabilitySet {
        CapabilitySet::from([
            Capability::Read(ReadScope::Workspace),
            Capability::GitRead,
        ])
    }

    fn is_mutating(&self) -> bool {
        false
    }

    async fn execute(&self, context: &ToolContext, input: serde_json::Value) -> ToolResult<ToolOutput> {
        let operation: GitToolInput = serde_json::from_value(input)
            .map_err(|e| ToolError::InvalidArguments {
                tool: "git".to_owned(),
                message: e.to_string(),
            })?;

        let output = match operation {
            GitToolInput::Status { path } => self.status(context, path.as_deref()).await?,
            GitToolInput::Diff { target_branch, path } => self.diff(context, target_branch.as_deref(), path.as_deref()).await?,
            GitToolInput::Log { branch, limit } => self.log(context, branch.as_deref(), limit).await?,
            GitToolInput::Branch { all } => self.branch(context, all).await?,
            GitToolInput::Show { commit } => self.show(context, &commit).await?,
            GitToolInput::CurrentBranch => self.current_branch(context).await?,
            GitToolInput::CurrentCommit => self.current_commit(context).await?,
        };

        Ok(ToolOutput::data(serde_json::to_value(output)?))
    }
}

/// Register Git tools in the registry.
pub fn register_git_tools(
    registry: &mut ToolRegistry,
    workspace: Option<Arc<dyn WorkspaceLease>>,
    manager: Option<Arc<GitWorktreeManager>>,
    session: Option<WorktreeSession>,
) {
    if let (Some(m), Some(s)) = (manager, session) {
        registry.register(Box::new(GitTool::with_worktree(workspace, m, s)));
    } else {
        registry.register(Box::new(GitTool::new(workspace)));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ryoko_core::model::{AgentRole, Task};
    use ryoko_models::ModelRouter;
    use tempfile::TempDir;

    #[tokio::test]
    async fn git_status() {
        let dir = TempDir::new().unwrap();
        // Initialize git repo
        Command::new("git")
            .current_dir(dir.path())
            .args(["init", "-b", "main"])
            .output().await.unwrap();
        Command::new("git")
            .current_dir(dir.path())
            .args(["config", "user.name", "Test"])
            .output().await.unwrap();
        Command::new("git")
            .current_dir(dir.path())
            .args(["config", "user.email", "test@test.com"])
            .output().await.unwrap();
        tokio::fs::write(dir.path().join("README.md"), "# Test").await.unwrap();
        Command::new("git")
            .current_dir(dir.path())
            .args(["add", "README.md"])
            .output().await.unwrap();
        Command::new("git")
            .current_dir(dir.path())
            .args(["commit", "-m", "Initial"])
            .output().await.unwrap();

        let workspace = ryoko_core::workspace::InMemoryWorkspaceProvider::new("main");
        let lease = workspace.allocate(&Task::new("test", AgentRole::SeniorDeveloper), &Default::default()).await.unwrap();

        let tool = GitTool::new(Some(lease));
        let context = ToolContext::new(
            Task::new("test", AgentRole::SeniorDeveloper),
            None,
            Arc::new(ModelRouter::new("mock")),
            Arc::new(ryoko_core::AllowAllValidator),
            Arc::new(ryoko_core::SharedMemory::new()),
            CapabilitySet::read_only(),
        );

        let input = serde_json::json!({
            "operation": "status"
        });
        let result = tool.execute(&context, input).await.unwrap();
        assert!(result.data["clean"] == true);
    }

    #[tokio::test]
    async fn git_log() {
        let dir = TempDir::new().unwrap();
        Command::new("git")
            .current_dir(dir.path())
            .args(["init", "-b", "main"])
            .output().await.unwrap();
        Command::new("git")
            .current_dir(dir.path())
            .args(["config", "user.name", "Test"])
            .output().await.unwrap();
        Command::new("git")
            .current_dir(dir.path())
            .args(["config", "user.email", "test@test.com"])
            .output().await.unwrap();
        tokio::fs::write(dir.path().join("README.md"), "# Test").await.unwrap();
        Command::new("git")
            .current_dir(dir.path())
            .args(["add", "README.md"])
            .output().await.unwrap();
        Command::new("git")
            .current_dir(dir.path())
            .args(["commit", "-m", "Initial commit"])
            .output().await.unwrap();

        let workspace = ryoko_core::workspace::InMemoryWorkspaceProvider::new("main");
        let lease = workspace.allocate(&Task::new("test", AgentRole::SeniorDeveloper), &Default::default()).await.unwrap();

        let tool = GitTool::new(Some(lease));
        let context = ToolContext::new(
            Task::new("test", AgentRole::SeniorDeveloper),
            None,
            Arc::new(ModelRouter::new("mock")),
            Arc::new(ryoko_core::AllowAllValidator),
            Arc::new(ryoko_core::SharedMemory::new()),
            CapabilitySet::read_only(),
        );

        let input = serde_json::json!({
            "operation": "log",
            "limit": 5
        });
        let result = tool.execute(&context, input).await.unwrap();
        let commits = result.data["commits"].as_array().unwrap();
        assert!(!commits.is_empty());
        assert!(commits[0]["message"].as_str().unwrap().contains("Initial"));
    }
}