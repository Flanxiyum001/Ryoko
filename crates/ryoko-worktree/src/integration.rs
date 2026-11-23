//! Merge-integrator agent for validated sibling branches.
//!
//! The integrator starts a fresh worktree at the current checkout tip and
//! three-way merges each validated dependency branch into it. Textual conflicts
//! are resolved by a model into complete file bodies; the model never supplies
//! paths outside the conflict set and never runs commands. The merged result is
//! returned as ordinary [`ProposedChange`]s, so the orchestrator still validates
//! and publishes it as one reviewable diff.

use std::collections::BTreeSet;
use std::path::Path;
use std::time::Duration;

use async_trait::async_trait;
use ryoko_core::agent::{AgentRuntime, TaskContext};
use ryoko_core::workspace::validate_relative_path;
use ryoko_core::{
    AgentIdentity, CoreResult, ProposedChange, Task, TaskResult, parse_structured_proposal,
    task_branch_key,
};
use ryoko_models::ChatRequest;
use tokio::process::Command;

use crate::{GitWorktreeManager, WorktreeSession};

const MAX_BRANCHES: usize = 4;
const MAX_CONFLICT_FILE_BYTES: usize = 512 * 1024;
const MAX_RESOLUTION_BYTES: usize = 1024 * 1024;
const MAX_PROPOSED_FILES: usize = 2_000;
const MAX_PROPOSED_BYTES: usize = 4 * 1024 * 1024;

const RESOLUTION_SYSTEM_PROMPT: &str = r#"You resolve Git merge conflicts in a software repository.
Return exactly one JSON object, with no Markdown fences or prose:
{"summary":"one sentence","files":[{"path":"relative/path","content":"complete resolved file"}]}

Rules:
- Resolve every listed path and no other path.
- Use base, ours and theirs snapshots to understand both sides of each conflict.
- Preserve both branches' intended behavior, tests and security fixes.
- If the intended semantics are ambiguous, do not fabricate a resolution.
- Never leave conflict markers (<<<<<<<, =======, >>>>>>>) in a file.
- Return complete file contents, not a patch.
- Never add commands, scripts, credentials or unrelated files.
"#;

#[derive(Debug, Clone)]
pub struct IntegrationAgent {
    identity: AgentIdentity,
}

impl IntegrationAgent {
    pub fn new(identity: AgentIdentity) -> Self {
        Self { identity }
    }

    pub fn with_model(mut self, model: impl Into<String>) -> Self {
        self.identity.model = Some(model.into());
        self
    }

    async fn integrate(
        &self,
        ctx: &TaskContext,
        task: &Task,
        branches: &[String],
    ) -> Result<Vec<ProposedChange>, String> {
        if branches.len() < 2 {
            return Err("integrator requires at least two published sibling branches".to_owned());
        }
        if branches.len() > MAX_BRANCHES {
            return Err(format!(
                "integrator supports at most {MAX_BRANCHES} sibling branches"
            ));
        }
        for branch in branches {
            if !branch.starts_with("ryoko/")
                || branch.len() > 300
                || branch.chars().any(char::is_control)
            {
                return Err(format!("invalid integration branch `{branch}`"));
            }
        }
        let manager = GitWorktreeManager::new(&ctx.project.project_root);
        let base = git_text(
            manager.base_project_path(),
            &["rev-parse", "HEAD"],
            "resolve integration base",
        )
        .await?;
        let base = base.trim().to_owned();
        let session = manager
            .create_worktree("merge-integrator", task.id)
            .await
            .map_err(|error| format!("cannot create integration worktree: {error}"))?;
        let outcome = self
            .integrate_in_worktree(ctx, task, &session, branches, &base)
            .await;
        let cleanup = manager.abandon_worktree(&session).await;
        let mut changes = outcome?;
        if let Err(error) = cleanup {
            eprintln!("warning: could not remove integration worktree: {error}");
        }
        changes.sort_by(|left, right| left.path.cmp(&right.path));
        Ok(changes)
    }

    async fn integrate_in_worktree(
        &self,
        ctx: &TaskContext,
        task: &Task,
        session: &WorktreeSession,
        branches: &[String],
        base: &str,
    ) -> Result<Vec<ProposedChange>, String> {
        for branch in branches {
            ctx.emit_progress(task.id, format!("merging {branch}"));
            let merge = git_output(
                &session.path,
                &["merge", "--no-ff", "--no-commit", branch],
                "merge sibling branch",
            )
            .await?;
            if merge.status.success() {
                commit_merge(&session.path, &format!("Integrate {branch}")).await?;
                continue;
            }
            let conflicts = conflicted_paths(&session.path).await?;
            if conflicts.is_empty() {
                return Err(format!(
                    "merge of {branch} failed: {}",
                    trim_output(&merge.stderr)
                ));
            }
            ctx.emit_progress(
                task.id,
                format!("resolving {} conflict(s) from {branch}", conflicts.len()),
            );
            self.resolve_conflicts(ctx, task, session, branch, &conflicts)
                .await?;
        }

        let head = git_text(&session.path, &["rev-parse", "HEAD"], "resolve merged HEAD").await?;
        let range = format!("{}..{}", base.trim(), head.trim());
        // A textual merge can still contain malformed whitespace/errors. The
        // normal orchestrator validation then runs compilation and regressions.
        git_text(
            &session.path,
            &["diff", "--check", &range],
            "check integrated patch",
        )
        .await?;
        let files = git_text(
            &session.path,
            &["diff", "--name-only", "--no-renames", &range],
            "list integrated files",
        )
        .await?;
        let mut changes = Vec::new();
        let mut total = 0usize;
        for path in files.lines().map(str::trim).filter(|line| !line.is_empty()) {
            if changes.len() >= MAX_PROPOSED_FILES {
                return Err("integrated change set exceeds the file limit".to_owned());
            }
            let relative = validate_relative_path(path).map_err(|error| error.to_string())?;
            let absolute = session.path.join(&relative);
            let metadata = tokio::fs::symlink_metadata(&absolute).await.map_err(|_| {
                format!("integrated file is missing (deletions are not supported): {path}")
            })?;
            if !metadata.is_file() || metadata.file_type().is_symlink() {
                return Err(format!("integrated path is not a regular file: {path}"));
            }
            if metadata.len() as usize > MAX_RESOLUTION_BYTES {
                return Err(format!("integrated file exceeds 1 MiB: {path}"));
            }
            let content = tokio::fs::read_to_string(&absolute)
                .await
                .map_err(|error| format!("cannot read integrated file {path}: {error}"))?;
            total = total.saturating_add(content.len());
            if total > MAX_PROPOSED_BYTES {
                return Err("integrated change set exceeds 4 MiB".to_owned());
            }
            changes.push(ProposedChange::new(path, content).with_language("text"));
        }
        if changes.is_empty() {
            return Err("sibling branches produced no integrated changes".to_owned());
        }
        Ok(changes)
    }

    async fn resolve_conflicts(
        &self,
        ctx: &TaskContext,
        task: &Task,
        session: &WorktreeSession,
        branch: &str,
        conflicts: &[String],
    ) -> Result<(), String> {
        let model = self
            .identity
            .model
            .as_deref()
            .map(str::trim)
            .filter(|model| !model.is_empty())
            .ok_or_else(|| {
                "integrator has no model configured for conflict resolution".to_owned()
            })?;
        let mut prompt = format!(
            "Goal:\n{}\n\nMerged branch: {branch}\nConflicted paths:\n{}\n\n",
            task.description,
            conflicts.join("\n")
        );
        for path in conflicts {
            let content = read_confined(session, path).await?;
            // Git index stages are the authoritative three-way conflict inputs.
            // Non-text/add-delete conflicts without all three stages fail closed
            // instead of asking a model to invent the missing source.
            let mut stages = Vec::new();
            for (number, label) in [(1, "base"), (2, "ours"), (3, "theirs")] {
                let object = format!(":{number}:{path}");
                let text = git_text(
                    &session.path,
                    &["show", &object],
                    "inspect three-way merge source",
                )
                .await
                .map_err(|error| format!(
                    "cannot safely resolve {path}: missing {label} stage ({error}); human review required"
                ))?;
                stages.push((label, text));
            }
            prompt.push_str(&format!(
                "\n<conflicted_file path=\"{path}\">\n{content}\n</conflicted_file>\n"
            ));
            for (label, source) in stages {
                prompt.push_str(&format!(
                    "<merge_stage name=\"{label}\" path=\"{path}\">\n{source}\n</merge_stage>\n"
                ));
            }
        }
        if prompt.len() > 80_000 {
            return Err("conflict context exceeds the resolution limit".to_owned());
        }
        let completion = ctx
            .models
            .complete(
                ChatRequest::new(model)
                    .with_system(RESOLUTION_SYSTEM_PROMPT)
                    .with_user(prompt)
                    .with_max_tokens(8_000)
                    .with_scope(task.id.to_string()),
            )
            .await
            .map_err(|error| format!("conflict-resolution model failed: {error}"))?;
        let proposal = parse_structured_proposal(&completion.text)
            .map_err(|error| format!("invalid conflict resolution: {error}"))?;
        validate_resolution(&proposal.files, conflicts)?;
        for file in &proposal.files {
            write_confined(session, file).await?;
        }
        let paths = proposal
            .files
            .iter()
            .map(|file| file.path.as_str())
            .collect::<Vec<_>>();
        let mut args = vec!["add", "--"];
        args.extend(paths);
        git_text(&session.path, &args, "stage resolved conflicts").await?;
        git_text(
            &session.path,
            &["diff", "--cached", "--check"],
            "check staged merge resolution",
        )
        .await?;
        if !conflicted_paths(&session.path).await?.is_empty() {
            return Err("conflicts remain after model resolution".to_owned());
        }
        commit_merge(
            &session.path,
            &format!("Resolve conflicts integrating {branch}"),
        )
        .await
    }
}

#[async_trait]
impl AgentRuntime for IntegrationAgent {
    fn identity(&self) -> AgentIdentity {
        self.identity.clone()
    }

    async fn execute_task(&self, ctx: TaskContext, task: Task) -> CoreResult<TaskResult> {
        let branches = task
            .dependencies
            .iter()
            .filter_map(|dependency| ctx.memory.get(&task_branch_key(*dependency)))
            .filter(|branch| !branch.trim().is_empty())
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect::<Vec<_>>();
        if branches.len() < 2 {
            return Ok(TaskResult::failure(
                task.id,
                "integrator found fewer than two published dependency branches",
            ));
        }
        match self.integrate(&ctx, &task, &branches).await {
            Ok(changes) => {
                let summary = format!(
                    "Integrated {} validated sibling branch(es) into one reviewable diff: {}",
                    branches.len(),
                    branches.join(", ")
                );
                Ok(TaskResult::success(task.id, summary).with_changes(changes))
            }
            Err(error) => Ok(TaskResult::failure(task.id, error)),
        }
    }
}

fn validate_resolution(files: &[ProposedChange], conflicts: &[String]) -> Result<(), String> {
    let expected = conflicts.iter().cloned().collect::<BTreeSet<_>>();
    let mut seen = BTreeSet::new();
    for file in files {
        validate_relative_path(&file.path).map_err(|error| error.to_string())?;
        if !expected.contains(&file.path) {
            return Err(format!(
                "resolution included a path that was not conflicted: {}",
                file.path
            ));
        }
        if !seen.insert(file.path.clone()) {
            return Err(format!("duplicate resolution for {}", file.path));
        }
        if file.content.len() > MAX_RESOLUTION_BYTES {
            return Err(format!("resolved file exceeds 1 MiB: {}", file.path));
        }
        if has_conflict_markers(&file.content) {
            return Err(format!(
                "resolution still contains conflict markers: {}",
                file.path
            ));
        }
    }
    let missing = expected
        .into_iter()
        .filter(|path| !seen.contains(path))
        .collect::<Vec<_>>();
    if !missing.is_empty() {
        return Err(format!(
            "resolution omitted conflicted path(s): {}",
            missing.join(", ")
        ));
    }
    Ok(())
}

fn has_conflict_markers(content: &str) -> bool {
    content.lines().any(|line| {
        line.starts_with("<<<<<<< ") || line.starts_with(">>>>>>> ") || line == "======="
    })
}

async fn conflicted_paths(root: &Path) -> Result<Vec<String>, String> {
    let text = git_text(
        root,
        &["diff", "--name-only", "--diff-filter=U", "--no-renames"],
        "list conflicted paths",
    )
    .await?;
    Ok(text
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(str::to_owned)
        .collect())
}

async fn read_confined(session: &WorktreeSession, path: &str) -> Result<String, String> {
    let relative = validate_relative_path(path).map_err(|error| error.to_string())?;
    let absolute = session.path.join(&relative);
    let metadata = tokio::fs::symlink_metadata(&absolute)
        .await
        .map_err(|error| format!("cannot inspect conflicted file {path}: {error}"))?;
    if !metadata.is_file() || metadata.file_type().is_symlink() {
        return Err(format!("conflicted path is not a regular file: {path}"));
    }
    if metadata.len() as usize > MAX_CONFLICT_FILE_BYTES {
        return Err(format!("conflicted file exceeds 512 KiB: {path}"));
    }
    tokio::fs::read_to_string(&absolute)
        .await
        .map_err(|error| format!("cannot read conflicted file {path}: {error}"))
}

async fn write_confined(session: &WorktreeSession, file: &ProposedChange) -> Result<(), String> {
    let relative = validate_relative_path(&file.path).map_err(|error| error.to_string())?;
    crate::confined::write(&session.path, &relative, file.content.as_bytes())
        .map_err(|error| format!("cannot write resolved {}: {error}", file.path))
}

async fn commit_merge(root: &Path, message: &str) -> Result<(), String> {
    let output = git_output(
        root,
        &[
            "-c",
            "user.name=Ryoko Integrator",
            "-c",
            "user.email=ryoko-integrator@localhost",
            "-c",
            "commit.gpgsign=false",
            "commit",
            "--no-verify",
            "-m",
            message,
        ],
        "commit integration merge",
    )
    .await?;
    if !output.status.success() {
        return Err(format!(
            "integration commit failed: {}",
            trim_output(&output.stderr)
        ));
    }
    Ok(())
}

async fn git_text(root: &Path, args: &[&str], action: &str) -> Result<String, String> {
    let output = git_output(root, args, action).await?;
    if !output.status.success() {
        return Err(format!(
            "git {action} failed: {}",
            trim_output(&output.stderr)
        ));
    }
    String::from_utf8(output.stdout).map_err(|_| format!("git {action} returned non-UTF-8 output"))
}

async fn git_output(
    root: &Path,
    args: &[&str],
    action: &str,
) -> Result<std::process::Output, String> {
    let mut command = Command::new("git");
    command
        .current_dir(root)
        .args(args)
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GIT_MERGE_AUTOEDIT", "no")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true);
    tokio::time::timeout(Duration::from_secs(120), command.output())
        .await
        .map_err(|_| format!("git {action} timed out"))?
        .map_err(|error| format!("git {action} could not run: {error}"))
}

fn trim_output(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes)
        .trim()
        .chars()
        .take(4_000)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    use ryoko_core::{
        AgentRole, AllowAllValidator, EventSink, ProjectContext, SharedMemory, WorkspaceProvider,
    };
    use ryoko_models::{MockProvider, ModelRouter, ScriptedTurn};
    use tempfile::TempDir;

    fn identity(model: &str) -> AgentIdentity {
        AgentIdentity::new(
            AgentRole::Integrator,
            "integrator",
            "merge integration agent",
        )
        .with_model(model)
    }

    #[test]
    fn rejects_missing_extra_and_marker_resolutions() {
        let conflicts = vec!["src/lib.rs".to_owned()];
        assert!(
            validate_resolution(
                &[ProposedChange::new("src/lib.rs", "fn merged() {}")],
                &conflicts
            )
            .is_ok()
        );
        assert!(validate_resolution(&[], &conflicts).is_err());
        assert!(
            validate_resolution(
                &[ProposedChange::new("src/other.rs", "fn other() {}")],
                &conflicts
            )
            .is_err()
        );
        assert!(
            validate_resolution(
                &[ProposedChange::new(
                    "src/lib.rs",
                    "<<<<<<< HEAD\nfn a() {}\n=======\nfn b() {}\n>>>>>>> other\n"
                )],
                &conflicts
            )
            .is_err()
        );
    }

    #[test]
    fn detects_only_line_leading_conflict_markers() {
        assert!(has_conflict_markers("<<<<<<< HEAD\n"));
        assert!(has_conflict_markers("=======\n"));
        assert!(has_conflict_markers(">>>>>>> branch\n"));
        assert!(!has_conflict_markers("let separator = \"=======\";\n"));
    }

    #[tokio::test]
    async fn merges_non_conflicting_sibling_branches_into_one_change_set() {
        let directory = TempDir::new().unwrap();
        let root = directory.path();
        run_git(root, &["init", "-q", "-b", "main"]);
        run_git(root, &["config", "user.name", "Test"]);
        run_git(root, &["config", "user.email", "test@example.com"]);
        std::fs::write(root.join("base.txt"), "base\n").unwrap();
        run_git(root, &["add", "."]);
        run_git(root, &["commit", "-q", "-m", "base"]);
        run_git(root, &["checkout", "-q", "-b", "ryoko/qa"]);
        std::fs::write(root.join("qa.txt"), "qa\n").unwrap();
        run_git(root, &["add", "."]);
        run_git(root, &["commit", "-q", "-m", "qa"]);
        run_git(root, &["checkout", "-q", "main"]);
        run_git(root, &["checkout", "-q", "-b", "ryoko/security"]);
        std::fs::write(root.join("security.txt"), "security\n").unwrap();
        run_git(root, &["add", "."]);
        run_git(root, &["commit", "-q", "-m", "security"]);
        run_git(root, &["checkout", "-q", "main"]);

        let qa_id = uuid::Uuid::new_v4();
        let security_id = uuid::Uuid::new_v4();
        let task = Task::new("integrate siblings", AgentRole::Integrator)
            .with_dependency(qa_id)
            .with_dependency(security_id);
        let memory = SharedMemory::new();
        memory.insert(task_branch_key(qa_id), "ryoko/qa".to_owned());
        memory.insert(task_branch_key(security_id), "ryoko/security".to_owned());
        // A real team run allocates this generic lease before the agent runs;
        // two dependencies must be accepted only for the Integrator role.
        let manager = GitWorktreeManager::new(root);
        let identity = identity("mock/model");
        let lease = manager
            .allocate(&task, &identity)
            .await
            .expect("integrator lease should accept multiple dependencies");
        let ctx = TaskContext {
            project: ProjectContext {
                project_root: root.display().to_string(),
                active_branch: "main".to_owned(),
                shared_memory: Default::default(),
            },
            memory,
            models: Arc::new(ModelRouter::new("mock")),
            validator: Arc::new(AllowAllValidator),
            workspace: Some(lease),
            mcp: None,
            events: EventSink::default(),
        };
        let agent = IntegrationAgent::new(identity);
        let result = agent.execute_task(ctx, task).await.unwrap();
        assert!(result.success, "{:?}", result.error);
        let paths = result
            .changes
            .iter()
            .map(|change| change.path.as_str())
            .collect::<Vec<_>>();
        assert!(paths.contains(&"qa.txt"));
        assert!(paths.contains(&"security.txt"));
    }

    #[tokio::test]
    async fn resolves_a_textual_conflict_with_the_model() {
        let directory = TempDir::new().unwrap();
        let root = directory.path();
        run_git(root, &["init", "-q", "-b", "main"]);
        run_git(root, &["config", "user.name", "Test"]);
        run_git(root, &["config", "user.email", "test@example.com"]);
        std::fs::write(root.join("shared.txt"), "base\n").unwrap();
        run_git(root, &["add", "."]);
        run_git(root, &["commit", "-q", "-m", "base"]);
        run_git(root, &["checkout", "-q", "-b", "ryoko/qa"]);
        std::fs::write(root.join("shared.txt"), "qa\n").unwrap();
        run_git(root, &["commit", "-q", "-am", "qa"]);
        run_git(root, &["checkout", "-q", "main"]);
        run_git(root, &["checkout", "-q", "-b", "ryoko/security"]);
        std::fs::write(root.join("shared.txt"), "security\n").unwrap();
        run_git(root, &["commit", "-q", "-am", "security"]);
        run_git(root, &["checkout", "-q", "main"]);

        let provider = Arc::new(MockProvider::new("mock"));
        provider.push_turn(ScriptedTurn::text(
            r#"{"summary":"keep both","files":[{"path":"shared.txt","content":"qa and security\n"}]}"#,
        ));
        let mut router = ModelRouter::new("mock");
        router.register(provider);
        let qa_id = uuid::Uuid::new_v4();
        let security_id = uuid::Uuid::new_v4();
        let task = Task::new("integrate conflict", AgentRole::Integrator)
            .with_dependency(qa_id)
            .with_dependency(security_id);
        let memory = SharedMemory::new();
        memory.insert(task_branch_key(qa_id), "ryoko/qa".to_owned());
        memory.insert(task_branch_key(security_id), "ryoko/security".to_owned());
        let ctx = TaskContext {
            project: ProjectContext {
                project_root: root.display().to_string(),
                active_branch: "main".to_owned(),
                shared_memory: Default::default(),
            },
            memory,
            models: Arc::new(router),
            validator: Arc::new(AllowAllValidator),
            workspace: None,
            mcp: None,
            events: EventSink::default(),
        };
        let agent = IntegrationAgent::new(identity("mock/model"));
        let result = agent.execute_task(ctx, task).await.unwrap();
        assert!(result.success, "{:?}", result.error);
        let shared = result
            .changes
            .iter()
            .find(|change| change.path == "shared.txt")
            .unwrap();
        assert_eq!(shared.content, "qa and security\n");
    }

    fn run_git(root: &Path, args: &[&str]) {
        let status = std::process::Command::new("git")
            .current_dir(root)
            .args(args)
            .status()
            .unwrap();
        assert!(status.success(), "git {args:?} failed");
    }
}
