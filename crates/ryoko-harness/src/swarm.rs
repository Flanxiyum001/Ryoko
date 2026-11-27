//! Agent swarm: one coordinator splits a long task into independent
//! sub-tasks. Each sub-task runs in its own Git worktree and branch, with its
//! own agent, is reviewed by Ryoko Review, and is committed as its own branch
//! with a ready-to-open pull request.
//!
//! The swarm never touches the user's checkout and never pushes. Pushing a
//! branch is a separate, user-triggered step ([`push_branch`]).

use std::path::{Component, Path, PathBuf};
use std::sync::Arc;

use async_trait::async_trait;
use ryoko_models::{ChatRequest, ModelRouter};
use serde::{Deserialize, Serialize};
use tokio::sync::Semaphore;

use crate::context::FastContext;
use crate::harness::{Harness, HarnessConfig, HarnessEvent, StopCause};
use crate::review::{self, ReviewReport, Severity};
use crate::tools::ProposedChange;

pub const MAX_SWARM_TASKS: usize = 6;

/// An isolated checkout leased for one sub-task.
#[derive(Debug, Clone)]
pub struct SwarmLease {
    pub branch: String,
    pub path: PathBuf,
    pub(crate) key: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct PublishedBranch {
    pub branch: String,
    pub base_branch: String,
    pub files: Vec<String>,
}

/// Where sub-tasks run. The production implementation is [`GitSwarmWorkspace`].
#[async_trait]
pub trait SwarmWorkspace: Send + Sync {
    async fn lease(&self, slug: &str) -> Result<SwarmLease, String>;
    /// Commits everything in the lease on its branch.
    async fn publish(&self, lease: &SwarmLease, message: &str) -> Result<PublishedBranch, String>;
    /// Drops a lease that produced nothing.
    async fn abandon(&self, lease: &SwarmLease);
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SwarmTask {
    pub title: String,
    pub goal: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "state", content = "detail", rename_all = "snake_case")]
pub enum SwarmTaskStatus {
    /// Committed on its branch, ready for a pull request.
    Ready,
    NoChanges,
    Failed(String),
}

#[derive(Debug, Clone, Serialize)]
pub struct SwarmTaskResult {
    pub task: SwarmTask,
    pub status: SwarmTaskStatus,
    pub branch: Option<String>,
    pub base_branch: Option<String>,
    pub files: Vec<String>,
    pub summary: String,
    pub review: Option<ReviewReport>,
    pub pr_title: String,
    pub pr_body: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct SwarmReport {
    pub goal: String,
    pub tasks: Vec<SwarmTaskResult>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum SwarmEvent {
    Planned {
        tasks: Vec<SwarmTask>,
    },
    TaskStarted {
        index: usize,
        branch: String,
    },
    TaskProgress {
        index: usize,
        event: HarnessEvent,
    },
    TaskReviewing {
        index: usize,
    },
    TaskFinished {
        index: usize,
        result: Box<SwarmTaskResult>,
    },
}

#[derive(Clone)]
pub struct SwarmConfig {
    pub model: String,
    /// Model for Ryoko Review; defaults to `model`.
    pub review_model: Option<String>,
    pub max_parallel: usize,
    pub max_turns_per_task: usize,
}

impl SwarmConfig {
    pub fn new(model: impl Into<String>) -> Self {
        Self {
            model: model.into(),
            review_model: None,
            max_parallel: 3,
            max_turns_per_task: 40,
        }
    }
}

type SwarmEventFn = Arc<dyn Fn(SwarmEvent) + Send + Sync>;

pub struct Swarm {
    models: Arc<ModelRouter>,
    workspace: Arc<dyn SwarmWorkspace>,
    config: SwarmConfig,
    events: Option<SwarmEventFn>,
}

const PLAN_SYSTEM: &str = "You are Ryoko's coordinator. Split the user's goal into 1 to 6 \
independent sub-tasks that separate engineers could finish in parallel on separate branches \
without touching the same files. Each sub-task must make sense as its own pull request. \
Prefer fewer, larger tasks over many tiny ones. If the goal is small, return one task.\n\
Reply with JSON only: {\"tasks\":[{\"title\":\"short PR title\",\"goal\":\"complete instructions for this sub-task\"}]}";

impl Swarm {
    pub fn new(
        models: Arc<ModelRouter>,
        workspace: Arc<dyn SwarmWorkspace>,
        config: SwarmConfig,
    ) -> Self {
        Self {
            models,
            workspace,
            config,
            events: None,
        }
    }

    pub fn on_event(mut self, callback: impl Fn(SwarmEvent) + Send + Sync + 'static) -> Self {
        self.events = Some(Arc::new(callback));
        self
    }

    fn emit(&self, event: SwarmEvent) {
        if let Some(callback) = &self.events {
            callback(event);
        }
    }

    /// Asks the coordinator model for sub-tasks. Falls back to a single task
    /// when the reply is unusable.
    pub async fn plan(&self, goal: &str) -> Vec<SwarmTask> {
        let request = ChatRequest::new(self.config.model.clone())
            .with_system(PLAN_SYSTEM)
            .with_user(format!("<goal>\n{goal}\n</goal>"))
            .with_max_tokens(2048);
        let fallback = || {
            vec![SwarmTask {
                title: title_from(goal),
                goal: goal.to_owned(),
            }]
        };
        match self.models.complete(request).await {
            Ok(completion) => parse_plan(&completion.text).unwrap_or_else(fallback),
            Err(_) => fallback(),
        }
    }

    /// Plans and runs the whole swarm.
    pub async fn run(&self, goal: &str) -> SwarmReport {
        let tasks = self.plan(goal).await;
        self.run_tasks(goal, tasks).await
    }

    /// Runs already-planned tasks, at most `max_parallel` at a time.
    pub async fn run_tasks(&self, goal: &str, tasks: Vec<SwarmTask>) -> SwarmReport {
        self.emit(SwarmEvent::Planned {
            tasks: tasks.clone(),
        });
        let gate = Arc::new(Semaphore::new(
            self.config.max_parallel.clamp(1, MAX_SWARM_TASKS),
        ));
        let runs = tasks.iter().cloned().enumerate().map(|(index, task)| {
            let gate = gate.clone();
            let overall = goal.to_owned();
            async move {
                let _permit = gate.acquire().await.expect("semaphore open");
                let result = self.run_one(index, &overall, task).await;
                self.emit(SwarmEvent::TaskFinished {
                    index,
                    result: Box::new(result.clone()),
                });
                result
            }
        });
        SwarmReport {
            goal: goal.to_owned(),
            tasks: futures_join_all(runs).await,
        }
    }

    async fn run_one(&self, index: usize, overall: &str, task: SwarmTask) -> SwarmTaskResult {
        let mut result = SwarmTaskResult {
            pr_title: task.title.clone(),
            task: task.clone(),
            status: SwarmTaskStatus::NoChanges,
            branch: None,
            base_branch: None,
            files: Vec::new(),
            summary: String::new(),
            review: None,
            pr_body: String::new(),
        };
        let lease = match self.workspace.lease(&format!("swarm-{}", index + 1)).await {
            Ok(lease) => lease,
            Err(error) => {
                result.status = SwarmTaskStatus::Failed(error);
                return result;
            }
        };
        self.emit(SwarmEvent::TaskStarted {
            index,
            branch: lease.branch.clone(),
        });
        let mut config = HarnessConfig::new(self.config.model.clone());
        config.max_turns = self.config.max_turns_per_task;
        config.max_delegation_depth = 0;
        config.scope = Some(format!("swarm-{index}"));
        let harness = match Harness::new(&lease.path, self.models.clone(), config) {
            Ok(harness) => harness,
            Err(error) => {
                self.workspace.abandon(&lease).await;
                result.status = SwarmTaskStatus::Failed(error.to_string());
                return result;
            }
        };
        let events = self.events.clone();
        let harness = harness.on_event(move |event| {
            if let Some(callback) = &events {
                callback(SwarmEvent::TaskProgress { index, event });
            }
        });
        let prompt = format!(
            "You are one of several agents working in parallel on this overall goal:\n{overall}\n\n\
             Your sub-task, on its own branch:\n{}\n\nOnly do your sub-task.",
            task.goal
        );
        let outcome = harness.run(&prompt).await;
        result.summary = outcome.final_text.clone();
        if let StopCause::ModelError(error) = &outcome.stop {
            self.workspace.abandon(&lease).await;
            result.status = SwarmTaskStatus::Failed(error.clone());
            return result;
        }
        if outcome.changes.is_empty() {
            self.workspace.abandon(&lease).await;
            return result;
        }
        if let Err(error) = write_changes(&lease.path, &outcome.changes) {
            self.workspace.abandon(&lease).await;
            result.status = SwarmTaskStatus::Failed(error);
            return result;
        }

        self.emit(SwarmEvent::TaskReviewing { index });
        let diff = review::diff_changes(&outcome.changes);
        let index_for_review = FastContext::build(&lease.path).ok();
        let review_model = self
            .config
            .review_model
            .clone()
            .unwrap_or_else(|| self.config.model.clone());
        result.review = review::review_diff(
            &self.models,
            &review_model,
            &diff,
            index_for_review.as_ref(),
        )
        .await
        .ok();
        result.pr_body = pr_body(
            &task,
            &outcome.final_text,
            result.review.as_ref(),
            &outcome.changes,
        );

        match self.workspace.publish(&lease, &task.title).await {
            Ok(published) => {
                result.branch = Some(published.branch);
                result.base_branch = Some(published.base_branch);
                result.files = published.files;
                result.status = SwarmTaskStatus::Ready;
            }
            Err(error) => {
                self.workspace.abandon(&lease).await;
                result.status = SwarmTaskStatus::Failed(error);
            }
        }
        result
    }
}

async fn futures_join_all<F: std::future::Future>(
    futures: impl Iterator<Item = F>,
) -> Vec<F::Output> {
    // Polls every task concurrently on the current task; no extra runtime threads.
    let futures: Vec<_> = futures.map(Box::pin).collect();
    let mut outputs: Vec<Option<F::Output>> = futures.iter().map(|_| None).collect();
    let mut pending = futures.into_iter().map(Some).collect::<Vec<_>>();
    std::future::poll_fn(move |cx| {
        let mut done = true;
        for (slot, output) in pending.iter_mut().zip(outputs.iter_mut()) {
            if let Some(future) = slot {
                match future.as_mut().poll(cx) {
                    std::task::Poll::Ready(value) => {
                        *output = Some(value);
                        *slot = None;
                    }
                    std::task::Poll::Pending => done = false,
                }
            }
        }
        if done {
            std::task::Poll::Ready(
                outputs
                    .iter_mut()
                    .map(|out| out.take().expect("output"))
                    .collect(),
            )
        } else {
            std::task::Poll::Pending
        }
    })
    .await
}

fn title_from(goal: &str) -> String {
    let first = goal.lines().next().unwrap_or(goal).trim();
    let mut title: String = first.chars().take(72).collect();
    if first.chars().count() > 72 {
        title.push('…');
    }
    if title.is_empty() {
        "Ryoko task".to_owned()
    } else {
        title
    }
}

/// Parses the coordinator's plan.
pub fn parse_plan(reply: &str) -> Option<Vec<SwarmTask>> {
    #[derive(Deserialize)]
    struct Raw {
        tasks: Vec<SwarmTask>,
    }
    let start = reply.find('{')?;
    let end = reply.rfind('}')?;
    let raw: Raw = serde_json::from_str(reply.get(start..=end)?).ok()?;
    let tasks: Vec<SwarmTask> = raw
        .tasks
        .into_iter()
        .filter_map(|task| {
            let goal: String = task.goal.trim().chars().take(8000).collect();
            let title: String = task
                .title
                .chars()
                .filter(|ch| !ch.is_control())
                .take(100)
                .collect::<String>()
                .trim()
                .to_owned();
            (!goal.is_empty()).then(|| SwarmTask {
                title: if title.is_empty() {
                    title_from(&goal)
                } else {
                    title
                },
                goal,
            })
        })
        .take(MAX_SWARM_TASKS)
        .collect();
    (!tasks.is_empty()).then_some(tasks)
}

fn pr_body(
    task: &SwarmTask,
    summary: &str,
    review: Option<&ReviewReport>,
    changes: &[ProposedChange],
) -> String {
    let mut body = format!(
        "## Summary\n\n{}\n\n## Task\n\n{}\n\n## Files\n\n",
        summary.trim(),
        task.goal.trim()
    );
    for change in changes {
        let kind = if change.original.is_none() {
            "added"
        } else {
            "changed"
        };
        body.push_str(&format!("- `{}` ({kind})\n", change.path));
    }
    match review {
        Some(report) => {
            body.push_str(&format!("\n## Review\n\n{}\n", report.summary));
            for finding in &report.findings {
                let level = match finding.severity {
                    Severity::Blocker => "Blocker",
                    Severity::Warning => "Warning",
                    Severity::Suggestion => "Suggestion",
                };
                let at = finding.line.map_or_else(
                    || finding.path.clone(),
                    |line| format!("{}:{line}", finding.path),
                );
                body.push_str(&format!("- **{level}** `{at}`: {}\n", finding.title));
            }
        }
        None => body.push_str("\n## Review\n\nAutomatic review was not available.\n"),
    }
    body
}

/// Writes staged changes into an isolated worktree.
pub fn write_changes(root: &Path, changes: &[ProposedChange]) -> Result<(), String> {
    let root = root.canonicalize().map_err(|error| error.to_string())?;
    for change in changes {
        let relative = Path::new(&change.path);
        let safe = !change.path.contains('\\')
            && !relative.is_absolute()
            && relative.components().all(|component| {
                matches!(component, Component::Normal(name) if !name.to_string_lossy().starts_with('.'))
            });
        if !safe {
            return Err(format!("unsafe path `{}`", change.path));
        }
        let target = root.join(relative);
        if target.is_symlink() {
            return Err(format!(
                "refusing to write through symlink `{}`",
                change.path
            ));
        }
        if let Some(parent) = target.parent() {
            std::fs::create_dir_all(parent).map_err(|error| error.to_string())?;
            let parent = parent.canonicalize().map_err(|error| error.to_string())?;
            if !parent.starts_with(&root) {
                return Err(format!("`{}` resolves outside the worktree", change.path));
            }
        }
        std::fs::write(&target, &change.content).map_err(|error| error.to_string())?;
    }
    Ok(())
}

/// The production workspace: one `ryoko-worktree` lease per sub-task, so each
/// branch also appears in Ryoko's existing session review views.
pub struct GitSwarmWorkspace {
    manager: ryoko_worktree::GitWorktreeManager,
    sessions: std::sync::Mutex<std::collections::HashMap<String, ryoko_worktree::WorktreeSession>>,
}

impl GitSwarmWorkspace {
    pub fn new(project: &Path) -> Self {
        Self {
            manager: ryoko_worktree::GitWorktreeManager::new(project),
            sessions: std::sync::Mutex::new(std::collections::HashMap::new()),
        }
    }

    fn session(&self, lease: &SwarmLease) -> Option<ryoko_worktree::WorktreeSession> {
        self.sessions
            .lock()
            .expect("sessions")
            .get(&lease.key)
            .cloned()
    }
}

#[async_trait]
impl SwarmWorkspace for GitSwarmWorkspace {
    async fn lease(&self, slug: &str) -> Result<SwarmLease, String> {
        let id = uuid::Uuid::new_v4();
        let session = self
            .manager
            .create_worktree(slug, id)
            .await
            .map_err(|error| error.to_string())?;
        let lease = SwarmLease {
            branch: session.branch_name.clone(),
            path: session.path.clone(),
            key: id.to_string(),
        };
        self.sessions
            .lock()
            .expect("sessions")
            .insert(lease.key.clone(), session);
        Ok(lease)
    }

    async fn publish(&self, lease: &SwarmLease, message: &str) -> Result<PublishedBranch, String> {
        let session = self.session(lease).ok_or("unknown swarm lease")?;
        let published = self
            .manager
            .publish_worktree(&session, message)
            .await
            .map_err(|error| error.to_string())?;
        Ok(PublishedBranch {
            branch: published.source_branch,
            base_branch: published.target_branch,
            files: published.files,
        })
    }

    async fn abandon(&self, lease: &SwarmLease) {
        if let Some(session) = self.session(lease) {
            let _ = self.manager.abandon_worktree(&session).await;
        }
    }
}

/// Pushes one swarm branch to `origin` and returns a link that opens a
/// prefilled pull request on GitHub, when `origin` is a GitHub remote.
/// Call this only after the user asks to publish the branch.
pub async fn push_branch(
    project: &Path,
    branch: &str,
    base: &str,
    title: &str,
    body: &str,
) -> Result<Option<String>, String> {
    if !valid_branch(branch) || !valid_branch(base) {
        return Err("invalid branch name".into());
    }
    let output = tokio::process::Command::new("git")
        .args(["push", "--set-upstream", "origin", branch])
        .current_dir(project)
        .stdin(std::process::Stdio::null())
        .output()
        .await
        .map_err(|error| format!("could not run git: {error}"))?;
    if !output.status.success() {
        return Err(format!(
            "git push failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    let remote = tokio::process::Command::new("git")
        .args(["remote", "get-url", "origin"])
        .current_dir(project)
        .output()
        .await
        .map_err(|error| error.to_string())?;
    let remote = String::from_utf8_lossy(&remote.stdout).trim().to_owned();
    Ok(github_repo(&remote).map(|repo| compare_url(&repo, base, branch, title, body)))
}

fn valid_branch(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 200
        && !name.starts_with('-')
        && !name.contains("..")
        && name
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '/' | '-' | '_' | '.'))
}

/// `owner/repo` for GitHub HTTPS or SSH remotes.
pub fn github_repo(remote: &str) -> Option<String> {
    let rest = remote
        .strip_prefix("https://github.com/")
        .or_else(|| remote.strip_prefix("git@github.com:"))
        .or_else(|| remote.strip_prefix("ssh://git@github.com/"))?;
    let repo = rest.trim_end_matches('/').trim_end_matches(".git");
    let mut parts = repo.split('/');
    let (owner, name) = (parts.next()?, parts.next()?);
    (parts.next().is_none() && !owner.is_empty() && !name.is_empty())
        .then(|| format!("{owner}/{name}"))
}

pub fn compare_url(repo: &str, base: &str, branch: &str, title: &str, body: &str) -> String {
    let body: String = body.chars().take(4000).collect();
    format!(
        "https://github.com/{repo}/compare/{}...{}?expand=1&title={}&body={}",
        encode(base),
        encode(branch),
        encode(title),
        encode(&body)
    )
}

fn encode(text: &str) -> String {
    let mut out = String::new();
    for byte in text.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'~' | b'/') {
            out.push(byte as char);
        } else {
            out.push_str(&format!("%{byte:02X}"));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use ryoko_models::{MockProvider, ScriptedToolCall, ScriptedTurn};
    use serde_json::json;
    use std::sync::Mutex;

    struct DirWorkspace {
        base: tempfile::TempDir,
        published: Mutex<Vec<String>>,
        abandoned: Mutex<Vec<String>>,
    }

    #[async_trait]
    impl SwarmWorkspace for DirWorkspace {
        async fn lease(&self, slug: &str) -> Result<SwarmLease, String> {
            let path = self.base.path().join(slug);
            std::fs::create_dir_all(&path).unwrap();
            std::fs::write(path.join("lib.rs"), "fn a() {}\n").unwrap();
            Ok(SwarmLease {
                branch: format!("ryoko/{slug}"),
                path,
                key: slug.into(),
            })
        }
        async fn publish(
            &self,
            lease: &SwarmLease,
            message: &str,
        ) -> Result<PublishedBranch, String> {
            self.published
                .lock()
                .unwrap()
                .push(format!("{}: {message}", lease.branch));
            Ok(PublishedBranch {
                branch: lease.branch.clone(),
                base_branch: "main".into(),
                files: vec!["lib.rs".into()],
            })
        }
        async fn abandon(&self, lease: &SwarmLease) {
            self.abandoned.lock().unwrap().push(lease.branch.clone());
        }
    }

    #[test]
    fn parses_plans_and_falls_back() {
        let plan = parse_plan(r#"{"tasks":[{"title":"API","goal":"Add endpoint"},{"title":"","goal":"Write docs"},{"title":"x","goal":"  "}]}"#).unwrap();
        assert_eq!(plan.len(), 2);
        assert_eq!(plan[1].title, "Write docs");
        assert!(parse_plan("nope").is_none());
    }

    #[test]
    fn builds_github_pull_request_links() {
        assert_eq!(
            github_repo("git@github.com:acme/app.git").as_deref(),
            Some("acme/app")
        );
        assert_eq!(
            github_repo("https://github.com/acme/app").as_deref(),
            Some("acme/app")
        );
        assert_eq!(github_repo("https://gitlab.com/acme/app"), None);
        let url = compare_url(
            "acme/app",
            "main",
            "ryoko/swarm-1/x",
            "Add API",
            "Body & more",
        );
        assert_eq!(
            url,
            "https://github.com/acme/app/compare/main...ryoko/swarm-1/x?expand=1&title=Add%20API&body=Body%20%26%20more"
        );
        assert!(!valid_branch("--upload-pack=x"));
        assert!(!valid_branch("a..b"));
    }

    #[tokio::test]
    async fn runs_each_task_on_its_own_branch_and_reviews_it() {
        let mock = Arc::new(MockProvider::new("mock"));
        // With max_parallel = 1 the scripted turns are consumed in order.
        mock.push_turn(ScriptedTurn::text(
            r#"{"tasks":[{"title":"Rename a","goal":"Rename a to b"},{"title":"Nothing","goal":"Look only"}]}"#,
        ));
        // Task 1: edit, finish, then the reviewer replies.
        mock.push_turn(ScriptedTurn::text("").with_tool_call(ScriptedToolCall::new(
            "1",
            "edit_file",
            json!({"path":"lib.rs","old":"fn a","new":"fn b"}),
        )));
        mock.push_turn(ScriptedTurn::text("Renamed a to b."));
        mock.push_turn(ScriptedTurn::text(
            r#"{"verdict":"needs_changes","summary":"Callers not updated.","findings":[{"path":"lib.rs","line":1,"severity":"warning","title":"Update callers","detail":"x"}]}"#,
        ));
        // Task 2: no changes.
        mock.push_turn(ScriptedTurn::text("Nothing to change."));
        let mut router = ModelRouter::new("mock");
        router.register(mock.clone());

        let workspace = Arc::new(DirWorkspace {
            base: tempfile::tempdir().unwrap(),
            published: Mutex::new(Vec::new()),
            abandoned: Mutex::new(Vec::new()),
        });
        let mut config = SwarmConfig::new("mock/coder");
        config.max_parallel = 1;
        let events = Arc::new(Mutex::new(0));
        let counter = events.clone();
        let swarm = Swarm::new(Arc::new(router), workspace.clone(), config)
            .on_event(move |_| *counter.lock().unwrap() += 1);
        let report = swarm.run("Rename things").await;

        assert_eq!(report.tasks.len(), 2);
        let first = &report.tasks[0];
        assert!(matches!(first.status, SwarmTaskStatus::Ready));
        assert_eq!(first.branch.as_deref(), Some("ryoko/swarm-1"));
        assert!(first.pr_body.contains("Callers not updated."));
        assert!(first.pr_body.contains("**Warning** `lib.rs:1`"));
        assert!(!first.pr_body.to_lowercase().contains("generated"));
        let written =
            std::fs::read_to_string(workspace.base.path().join("swarm-1/lib.rs")).unwrap();
        assert_eq!(written, "fn b() {}\n");
        assert!(matches!(report.tasks[1].status, SwarmTaskStatus::NoChanges));
        assert_eq!(*workspace.abandoned.lock().unwrap(), ["ryoko/swarm-2"]);
        assert_eq!(workspace.published.lock().unwrap().len(), 1);
        assert!(*events.lock().unwrap() > 4);
    }
    #[tokio::test]
    async fn git_workspace_commits_each_task_on_its_own_branch() {
        let dir = tempfile::tempdir().unwrap();
        let git = |args: &[&str]| {
            let status = std::process::Command::new("git")
                .args(["-c", "user.name=Test", "-c", "user.email=test@example.com"])
                .args(args)
                .current_dir(dir.path())
                .output()
                .unwrap();
            assert!(
                status.status.success(),
                "{args:?}: {}",
                String::from_utf8_lossy(&status.stderr)
            );
            String::from_utf8_lossy(&status.stdout).trim().to_owned()
        };
        git(&["init", "-q", "-b", "main"]);
        std::fs::write(dir.path().join("lib.rs"), "fn a() {}\n").unwrap();
        std::fs::write(dir.path().join(".gitignore"), ".ryoko/\n").unwrap();
        git(&["add", "-A"]);
        git(&["commit", "-qm", "init"]);
        git(&["config", "user.name", "Test"]);
        git(&["config", "user.email", "test@example.com"]);

        let workspace = GitSwarmWorkspace::new(dir.path());
        let lease = workspace.lease("swarm-1").await.unwrap();
        write_changes(
            &lease.path,
            &[ProposedChange {
                path: "lib.rs".into(),
                original: None,
                content: "fn b() {}\n".into(),
            }],
        )
        .unwrap();
        let published = workspace.publish(&lease, "Rename a").await.unwrap();
        assert_eq!(published.files, ["lib.rs"]);
        assert_eq!(published.base_branch, "main");
        let shown = git(&["show", &format!("{}:lib.rs", published.branch)]);
        assert_eq!(shown, "fn b() {}");
        // The user's checkout is untouched.
        assert_eq!(
            std::fs::read_to_string(dir.path().join("lib.rs")).unwrap(),
            "fn a() {}\n"
        );
    }
}
