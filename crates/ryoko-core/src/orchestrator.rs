//! Dependency-aware, concurrent task orchestration.
//!
//! Every task runs in its own [`WorkspaceLease`](crate::WorkspaceLease). Agent
//! output is staged there, validated by a [`ChangeValidator`] (the WASM sandbox
//! in production), and only then published as an internal pull request. A task
//! that fails validation is rolled back, its lease discarded, and its dependents
//! are skipped.

use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::Arc;
use std::time::Instant;

use futures::FutureExt;
use serde::Serialize;
use tokio::sync::{Semaphore, watch};
use tokio::task::JoinSet;
use uuid::Uuid;

use crate::agent::{AgentRuntime, TaskContext};
use crate::agents::RETRY_FEEDBACK_METADATA;
use crate::error::{CoreError, CoreResult};
use crate::event::{EventSink, OrchestratorEvent};
use crate::memory::SharedMemory;
use crate::model::{AgentRole, ProjectContext, Task, TaskResult};
use crate::validation::{ChangeValidator, StagedChanges, ValidationReport};
use crate::workspace::WorkspaceProvider;
use ryoko_models::ModelRouter;

#[derive(Debug, Clone)]
pub struct OrchestratorConfig {
    /// Maximum number of agents executing at once. Dependency waits do not hold
    /// a permit, so a blocked DAG can never deadlock the pool.
    pub max_concurrency: usize,
    /// Total attempts per task, including the first one.
    pub max_attempts: u32,
}

impl Default for OrchestratorConfig {
    fn default() -> Self {
        Self {
            max_concurrency: num_cpus_hint(),
            max_attempts: 2,
        }
    }
}

fn num_cpus_hint() -> usize {
    std::thread::available_parallelism()
        .map(|value| value.get())
        .unwrap_or(4)
        .clamp(1, 8)
}

#[derive(Debug, Clone, Serialize)]
pub struct TaskSuccess {
    pub task_id: Uuid,
    pub role: AgentRole,
    pub agent: String,
    pub output: String,
    pub pr_id: String,
    pub branch: String,
    pub files: Vec<String>,
    pub attempts: u32,
}

#[derive(Debug, Clone, Serialize)]
pub struct TaskFailure {
    pub task_id: Uuid,
    pub role: AgentRole,
    pub error: String,
    pub attempts: u32,
}

#[derive(Debug, Clone, Serialize)]
pub struct SkippedTask {
    pub task_id: Uuid,
    pub role: AgentRole,
    pub reason: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct OrchestrationReport {
    pub run_id: Uuid,
    pub succeeded: Vec<TaskSuccess>,
    pub failed: Vec<TaskFailure>,
    pub skipped: Vec<SkippedTask>,
    pub duration_ms: u64,
}

impl OrchestrationReport {
    pub fn all_succeeded(&self) -> bool {
        self.failed.is_empty() && self.skipped.is_empty()
    }

    pub fn pull_requests(&self) -> Vec<&TaskSuccess> {
        self.succeeded.iter().collect()
    }
}

#[derive(Clone, Debug)]
enum TaskOutcome {
    Succeeded { attempts: u32 },
    Failed { reason: String, attempts: u32 },
    Skipped { reason: String },
}

pub struct Orchestrator {
    workspace: Arc<dyn WorkspaceProvider>,
    validator: Arc<dyn ChangeValidator>,
    models: Arc<ModelRouter>,
    config: OrchestratorConfig,
    events: EventSink,
}

impl Orchestrator {
    pub fn new(
        workspace: Arc<dyn WorkspaceProvider>,
        validator: Arc<dyn ChangeValidator>,
        models: Arc<ModelRouter>,
    ) -> Self {
        Self {
            workspace,
            validator,
            models,
            config: OrchestratorConfig::default(),
            events: EventSink::default(),
        }
    }

    pub fn with_config(mut self, config: OrchestratorConfig) -> Self {
        self.config = config;
        self
    }

    pub fn with_events(mut self, events: EventSink) -> Self {
        self.events = events;
        self
    }

    pub fn events(&self) -> EventSink {
        self.events.clone()
    }

    pub fn config(&self) -> &OrchestratorConfig {
        &self.config
    }

    /// Runs a task DAG to completion. Tasks without unmet dependencies execute
    /// concurrently; impossible plans fail before any agent is invoked.
    pub async fn run(
        &self,
        tasks: Vec<Task>,
        agents: Vec<Arc<dyn AgentRuntime>>,
        project: ProjectContext,
    ) -> CoreResult<OrchestrationReport> {
        let started_at = Instant::now();
        let run_id = Uuid::new_v4();
        Self::validate_plan(&tasks)?;
        let registry = Self::register_agents(agents)?;

        for task in &tasks {
            if !registry.contains_key(&task.assigned_to) {
                return Err(CoreError::MissingAgent {
                    role: task.assigned_to.clone(),
                });
            }
        }

        self.events.emit(OrchestratorEvent::RunStarted {
            run_id,
            task_count: tasks.len(),
        });

        let memory = SharedMemory::new();
        let mut outcomes: HashMap<Uuid, watch::Sender<Option<TaskOutcome>>> = HashMap::new();
        let mut receivers: HashMap<Uuid, watch::Receiver<Option<TaskOutcome>>> = HashMap::new();
        for task in &tasks {
            let (tx, rx) = watch::channel(None);
            outcomes.insert(task.id, tx);
            receivers.insert(task.id, rx);
        }

        let semaphore = Arc::new(Semaphore::new(self.config.max_concurrency.max(1)));
        let mut join_set: JoinSet<(Task, Option<TaskSuccess>, TaskOutcome)> = JoinSet::new();

        for task in tasks {
            let context = TaskContext {
                project: project.clone(),
                memory: memory.clone(),
                models: self.models.clone(),
                validator: self.validator.clone(),
                workspace: None,
                events: self.events.clone(),
            };
            let agent = registry[&task.assigned_to].clone();
            let workspace = self.workspace.clone();
            let events = self.events.clone();
            let semaphore = semaphore.clone();
            let mut dependencies = Vec::with_capacity(task.dependencies.len());
            for dependency in &task.dependencies {
                dependencies.push((*dependency, receivers[dependency].clone()));
            }

            self.events.emit(OrchestratorEvent::TaskScheduled {
                task_id: task.id,
                role: task.assigned_to.clone(),
                description: task.description.clone(),
            });

            let max_attempts = self.config.max_attempts.max(1);
            let task_on_panic = task.clone();
            join_set.spawn(async move {
                let operation = async {
                    let task = task;
                    if let Some(reason) = wait_for_dependencies(&dependencies).await {
                        events.emit(OrchestratorEvent::TaskSkipped {
                            task_id: task.id,
                            role: task.assigned_to.clone(),
                            reason: reason.clone(),
                        });
                        return (task, None, TaskOutcome::Skipped { reason });
                    }

                    let permit = match semaphore.acquire_owned().await {
                        Ok(permit) => permit,
                        Err(_) => {
                            return (
                                task.clone(),
                                None,
                                TaskOutcome::Failed {
                                    reason: "orchestrator semaphore closed".to_owned(),
                                    attempts: 0,
                                },
                            );
                        }
                    };

                    let (outcome, success) = execute_task(
                        &task,
                        agent,
                        context,
                        workspace,
                        events.clone(),
                        max_attempts,
                    )
                    .await;
                    drop(permit);

                    (task, success, outcome)
                };
                match std::panic::AssertUnwindSafe(operation).catch_unwind().await {
                    Ok(value) => value,
                    Err(_) => (
                        task_on_panic,
                        None,
                        TaskOutcome::Failed {
                            reason: "agent task panicked".to_owned(),
                            attempts: 1,
                        },
                    ),
                }
            });
        }

        let mut report = OrchestrationReport {
            run_id,
            succeeded: Vec::new(),
            failed: Vec::new(),
            skipped: Vec::new(),
            duration_ms: 0,
        };

        while let Some(joined) = join_set.join_next().await {
            let (task, success, outcome) = match joined {
                Ok(value) => value,
                Err(err) => {
                    // A panicking agent must not abort the whole run.
                    report.failed.push(TaskFailure {
                        task_id: Uuid::nil(),
                        role: AgentRole::Custom("unknown".to_owned()),
                        error: format!("agent task panicked: {err}"),
                        attempts: 0,
                    });
                    continue;
                }
            };

            if let Some(tx) = outcomes.get(&task.id) {
                let _ = tx.send(Some(outcome.clone()));
            }

            match outcome {
                TaskOutcome::Succeeded { attempts } => {
                    if let Some(mut success) = success {
                        success.attempts = attempts;
                        report.succeeded.push(success);
                    }
                }
                TaskOutcome::Failed { reason, attempts } => {
                    report.failed.push(TaskFailure {
                        task_id: task.id,
                        role: task.assigned_to.clone(),
                        error: reason,
                        attempts,
                    });
                }
                TaskOutcome::Skipped { reason } => {
                    report.skipped.push(SkippedTask {
                        task_id: task.id,
                        role: task.assigned_to.clone(),
                        reason,
                    });
                }
            }
        }

        report.duration_ms = started_at.elapsed().as_millis() as u64;
        self.events.emit(OrchestratorEvent::RunFinished {
            run_id,
            succeeded: report.succeeded.len(),
            failed: report.failed.len(),
            skipped: report.skipped.len(),
            duration_ms: report.duration_ms,
        });
        Ok(report)
    }

    /// Kahn's algorithm over the dependency graph. Also rejects duplicate IDs,
    /// unknown dependencies and self-dependencies.
    pub fn validate_plan(tasks: &[Task]) -> CoreResult<()> {
        let mut known = HashSet::new();
        for task in tasks {
            if !known.insert(task.id) {
                return Err(CoreError::DuplicateTask(task.id));
            }
        }
        let mut indegree: HashMap<Uuid, usize> = tasks.iter().map(|t| (t.id, 0)).collect();
        let mut edges: HashMap<Uuid, Vec<Uuid>> = HashMap::new();
        for task in tasks {
            for dependency in &task.dependencies {
                if *dependency == task.id {
                    return Err(CoreError::DependencyCycle {
                        tasks: vec![task.id],
                    });
                }
                if !known.contains(dependency) {
                    return Err(CoreError::UnknownDependency {
                        task: task.id,
                        dependency: *dependency,
                    });
                }
                edges.entry(*dependency).or_default().push(task.id);
                *indegree.entry(task.id).or_insert(0) += 1;
            }
        }

        let mut ready: VecDeque<Uuid> = indegree
            .iter()
            .filter(|(_, degree)| **degree == 0)
            .map(|(id, _)| *id)
            .collect();
        let mut visited = 0;
        while let Some(id) = ready.pop_front() {
            visited += 1;
            if let Some(children) = edges.get(&id) {
                for child in children {
                    if let Some(degree) = indegree.get_mut(child) {
                        *degree -= 1;
                        if *degree == 0 {
                            ready.push_back(*child);
                        }
                    }
                }
            }
        }
        if visited != tasks.len() {
            let tasks = indegree
                .into_iter()
                .filter(|(_, degree)| *degree > 0)
                .map(|(id, _)| id)
                .collect();
            return Err(CoreError::DependencyCycle { tasks });
        }
        Ok(())
    }

    fn register_agents(
        agents: Vec<Arc<dyn AgentRuntime>>,
    ) -> CoreResult<HashMap<AgentRole, Arc<dyn AgentRuntime>>> {
        let mut registry = HashMap::new();
        for agent in agents {
            let identity = agent.identity();
            if registry.contains_key(&identity.role) {
                return Err(CoreError::DuplicateAgent {
                    agent: identity.name,
                });
            }
            registry.insert(identity.role, agent);
        }
        Ok(registry)
    }
}

async fn wait_for_dependencies(
    dependencies: &[(Uuid, watch::Receiver<Option<TaskOutcome>>)],
) -> Option<String> {
    for (dependency_id, receiver) in dependencies {
        let mut receiver = receiver.clone();
        loop {
            if receiver.borrow().is_some() {
                break;
            }
            if receiver.changed().await.is_err() {
                return Some(format!("dependency {dependency_id} disappeared"));
            }
        }
        let borrowed = receiver.borrow();
        match borrowed.as_ref() {
            Some(TaskOutcome::Succeeded { .. }) => {}
            Some(TaskOutcome::Failed { reason, .. }) => {
                return Some(format!("dependency {dependency_id} failed: {reason}"));
            }
            Some(TaskOutcome::Skipped { reason }) => {
                return Some(format!("dependency {dependency_id} was skipped: {reason}"));
            }
            None => {}
        }
    }
    None
}

async fn execute_task(
    task: &Task,
    agent: Arc<dyn AgentRuntime>,
    context: TaskContext,
    workspace_provider: Arc<dyn WorkspaceProvider>,
    events: EventSink,
    max_attempts: u32,
) -> (TaskOutcome, Option<TaskSuccess>) {
    let mut last_error = String::from("task did not run");
    let mut feedback: Option<String> = None;
    for attempt in 1..=max_attempts {
        events.emit(OrchestratorEvent::TaskStarted {
            task_id: task.id,
            role: task.assigned_to.clone(),
            agent: agent.identity().name.clone(),
            attempt,
        });

        let lease = match workspace_provider.allocate(task, &agent.identity()).await {
            Ok(lease) => lease,
            Err(err) => {
                last_error = err.to_string();
                feedback = Some(last_error.clone());
                events.emit(OrchestratorEvent::TaskFailed {
                    task_id: task.id,
                    role: task.assigned_to.clone(),
                    error: last_error.clone(),
                    attempts: attempt,
                    retrying: attempt < max_attempts,
                });
                continue;
            }
        };
        let ctx = context.clone().with_workspace(Some(lease.clone()));

        // The next attempt sees the exact validation/compiler failure. This is
        // what makes the retry loop self-correcting instead of a blind replay.
        let mut attempt_task = task.clone();
        if let Some(feedback) = &feedback {
            attempt_task
                .metadata
                .insert(RETRY_FEEDBACK_METADATA.to_owned(), cap_feedback(feedback));
        }

        let result = match agent.execute_task(ctx.clone(), attempt_task.clone()).await {
            Ok(result) => result,
            Err(err) => TaskResult::failure(task.id, err.to_string()),
        };

        if !result.success {
            let reason = result
                .error
                .clone()
                .unwrap_or_else(|| "agent reported failure".to_owned());
            let _ = agent.rollback(ctx.clone(), task, &reason).await;
            let _ = lease.discard().await;
            last_error = reason.clone();
            feedback = Some(reason);
            events.emit(OrchestratorEvent::TaskFailed {
                task_id: task.id,
                role: task.assigned_to.clone(),
                error: last_error.clone(),
                attempts: attempt,
                retrying: attempt < max_attempts,
            });
            continue;
        }

        if result.task_id != task.id {
            let reason = format!(
                "agent returned result for task {}, expected {}",
                result.task_id, task.id
            );
            let _ = agent.rollback(ctx.clone(), task, &reason).await;
            let _ = lease.discard().await;
            last_error = reason.clone();
            feedback = Some(reason);
            continue;
        }

        if let Err(err) = lease.apply(&result.changes).await {
            let reason = err.to_string();
            let _ = agent.rollback(ctx.clone(), task, &reason).await;
            let _ = lease.discard().await;
            last_error = reason.clone();
            feedback = Some(reason);
            continue;
        }

        events.emit(OrchestratorEvent::ValidationStarted {
            task_id: task.id,
            file_count: result.changes.len(),
        });
        let staged = StagedChanges::new(task.id, result.changes.clone())
            .with_root(lease.root().to_path_buf());
        let report = match context.validator.validate(&staged).await {
            Ok(report) => report,
            Err(err) => ValidationReport {
                passed: false,
                summary: err.to_string(),
                logs: Vec::new(),
                duration_ms: 0,
            },
        };
        events.emit(OrchestratorEvent::ValidationFinished {
            task_id: task.id,
            passed: report.passed,
            summary: report.summary.clone(),
            logs: report.logs.clone(),
            duration_ms: report.duration_ms,
        });

        if !report.passed {
            let reason = format!("validation rejected changes: {}", report.summary);
            let _ = agent.rollback(ctx.clone(), task, &reason).await;
            let _ = lease.discard().await;
            let mut next = reason.clone();
            if !report.logs.is_empty() {
                next.push_str("\n\n");
                next.push_str(&report.logs.join("\n"));
            }
            last_error = reason;
            feedback = Some(next);
            continue;
        }

        match lease.publish(task, &result.output).await {
            Ok(pr) => {
                events.emit(OrchestratorEvent::PullRequestOpened {
                    task_id: task.id,
                    pr_id: pr.id.clone(),
                    branch: pr.branch.clone(),
                    files: pr.files.clone(),
                    summary: pr.summary.clone(),
                });
                let success = TaskSuccess {
                    task_id: task.id,
                    role: task.assigned_to.clone(),
                    agent: agent.identity().name.clone(),
                    output: result.output.clone(),
                    pr_id: pr.id.clone(),
                    branch: pr.branch.clone(),
                    files: pr.files.clone(),
                    attempts: attempt,
                };
                let _ = lease.discard().await;
                events.emit(OrchestratorEvent::TaskSucceeded {
                    task_id: task.id,
                    role: task.assigned_to.clone(),
                    output: result.output.clone(),
                    attempts: attempt,
                });
                return (TaskOutcome::Succeeded { attempts: attempt }, Some(success));
            }
            Err(err) => {
                let reason = format!("publish failed: {err}");
                let _ = agent.rollback(ctx.clone(), task, &reason).await;
                let _ = lease.discard().await;
                last_error = reason.clone();
                feedback = Some(reason);
            }
        }
    }

    (
        TaskOutcome::Failed {
            reason: last_error,
            attempts: max_attempts,
        },
        None,
    )
}

fn cap_feedback(feedback: &str) -> String {
    const MAX_BYTES: usize = 16_000;
    if feedback.len() <= MAX_BYTES {
        return feedback.to_owned();
    }
    let mut end = MAX_BYTES;
    while end > 0 && !feedback.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}...", &feedback[..end])
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{AgentIdentity, ProposedChange};
    use crate::{AllowAllValidator, InMemoryWorkspaceProvider};
    use async_trait::async_trait;
    use std::sync::Mutex as StdMutex;
    use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
    use std::time::Duration;

    fn router() -> Arc<ModelRouter> {
        Arc::new(ModelRouter::new("mock"))
    }

    fn workspace() -> Arc<InMemoryWorkspaceProvider> {
        Arc::new(InMemoryWorkspaceProvider::new("main"))
    }

    fn change() -> Vec<ProposedChange> {
        vec![ProposedChange::new(
            "src/generated.rs",
            "pub fn generated() {}",
        )]
    }

    struct SleepAgent {
        identity: AgentIdentity,
        delay: Duration,
    }

    #[async_trait]
    impl AgentRuntime for SleepAgent {
        fn identity(&self) -> AgentIdentity {
            self.identity.clone()
        }

        async fn execute_task(&self, _ctx: TaskContext, task: Task) -> CoreResult<TaskResult> {
            tokio::time::sleep(self.delay).await;
            Ok(TaskResult::success(task.id, "done").with_changes(change()))
        }
    }

    struct RecordingAgent {
        identity: AgentIdentity,
        started: Arc<StdMutex<Vec<Uuid>>>,
    }

    #[async_trait]
    impl AgentRuntime for RecordingAgent {
        fn identity(&self) -> AgentIdentity {
            self.identity.clone()
        }

        async fn execute_task(&self, _ctx: TaskContext, task: Task) -> CoreResult<TaskResult> {
            self.started.lock().unwrap().push(task.id);
            Ok(TaskResult::success(task.id, "done").with_changes(change()))
        }
    }

    struct FlakyAgent {
        identity: AgentIdentity,
        calls: AtomicU32,
        failures: u32,
    }

    #[async_trait]
    impl AgentRuntime for FlakyAgent {
        fn identity(&self) -> AgentIdentity {
            self.identity.clone()
        }

        async fn execute_task(&self, _ctx: TaskContext, task: Task) -> CoreResult<TaskResult> {
            let call = self.calls.fetch_add(1, Ordering::SeqCst);
            if call < self.failures {
                Ok(TaskResult::failure(task.id, "transient model error"))
            } else {
                Ok(TaskResult::success(task.id, "recovered").with_changes(change()))
            }
        }
    }

    struct FailingAgent {
        identity: AgentIdentity,
        rolled_back: Arc<AtomicBool>,
    }

    #[async_trait]
    impl AgentRuntime for FailingAgent {
        fn identity(&self) -> AgentIdentity {
            self.identity.clone()
        }

        async fn execute_task(&self, _ctx: TaskContext, task: Task) -> CoreResult<TaskResult> {
            Ok(TaskResult::failure(task.id, "broken output"))
        }

        async fn rollback(&self, _ctx: TaskContext, _task: &Task, _reason: &str) -> CoreResult<()> {
            self.rolled_back.store(true, Ordering::SeqCst);
            Ok(())
        }
    }

    struct PanicAgent {
        identity: AgentIdentity,
    }

    #[async_trait]
    impl AgentRuntime for PanicAgent {
        fn identity(&self) -> AgentIdentity {
            self.identity.clone()
        }

        async fn execute_task(&self, _ctx: TaskContext, _task: Task) -> CoreResult<TaskResult> {
            panic!("simulated agent panic");
        }
    }

    #[tokio::test]
    async fn panicking_agents_do_not_hang_dependent_tasks() {
        let orchestrator = Orchestrator::new(workspace(), Arc::new(AllowAllValidator), router())
            .with_config(OrchestratorConfig {
                max_concurrency: 2,
                max_attempts: 1,
            });
        let first = Task::new("crash", AgentRole::ProjectManager);
        let second = Task::new("must skip", AgentRole::SeniorDeveloper).with_dependency(first.id);
        let manager: Arc<dyn AgentRuntime> = Arc::new(PanicAgent {
            identity: AgentIdentity::new(AgentRole::ProjectManager, "pm-1", "planner"),
        });
        let developer: Arc<dyn AgentRuntime> = Arc::new(SleepAgent {
            identity: dev(),
            delay: Duration::ZERO,
        });
        let report = tokio::time::timeout(
            Duration::from_secs(3),
            orchestrator.run(
                vec![first.clone(), second],
                vec![manager, developer],
                ProjectContext::default(),
            ),
        )
        .await
        .expect("orchestrator hung after an agent panic")
        .unwrap();
        assert_eq!(report.failed.len(), 1);
        assert_eq!(report.failed[0].task_id, first.id);
        assert_eq!(report.skipped.len(), 1);
    }

    struct DenyValidator;

    #[async_trait]
    impl ChangeValidator for DenyValidator {
        async fn validate(&self, _changes: &StagedChanges) -> CoreResult<ValidationReport> {
            Ok(ValidationReport::failed(
                "cargo test failed",
                vec!["test generated ... FAILED".to_owned()],
                Duration::from_millis(3),
            ))
        }
    }

    struct PassAfterApplyAgent {
        identity: AgentIdentity,
        rolled_back: Option<Arc<AtomicBool>>,
    }

    #[async_trait]
    impl AgentRuntime for PassAfterApplyAgent {
        fn identity(&self) -> AgentIdentity {
            self.identity.clone()
        }

        async fn execute_task(&self, _ctx: TaskContext, task: Task) -> CoreResult<TaskResult> {
            Ok(TaskResult::success(task.id, "staged").with_changes(change()))
        }

        async fn rollback(&self, _ctx: TaskContext, _task: &Task, _reason: &str) -> CoreResult<()> {
            if let Some(flag) = &self.rolled_back {
                flag.store(true, Ordering::SeqCst);
            }
            Ok(())
        }
    }

    fn dev() -> AgentIdentity {
        AgentIdentity::new(AgentRole::SeniorDeveloper, "dev-01", "implementer")
    }

    #[tokio::test]
    async fn independent_tasks_run_concurrently() {
        let orchestrator = Orchestrator::new(workspace(), Arc::new(AllowAllValidator), router())
            .with_config(OrchestratorConfig {
                max_concurrency: 3,
                max_attempts: 1,
            });
        let agent: Arc<dyn AgentRuntime> = Arc::new(SleepAgent {
            identity: dev(),
            delay: Duration::from_millis(200),
        });
        let tasks = vec![
            Task::new("one", AgentRole::SeniorDeveloper),
            Task::new("two", AgentRole::SeniorDeveloper),
            Task::new("three", AgentRole::SeniorDeveloper),
        ];

        let started = Instant::now();
        let report = orchestrator
            .run(tasks, vec![agent], ProjectContext::default())
            .await
            .unwrap();
        assert_eq!(report.succeeded.len(), 3);
        assert!(started.elapsed() < Duration::from_millis(500));
    }

    #[tokio::test]
    async fn dependencies_execute_in_order() {
        let orchestrator = Orchestrator::new(workspace(), Arc::new(AllowAllValidator), router())
            .with_config(OrchestratorConfig {
                max_concurrency: 4,
                max_attempts: 1,
            });
        let started = Arc::new(StdMutex::new(Vec::new()));
        let agent: Arc<dyn AgentRuntime> = Arc::new(RecordingAgent {
            identity: dev(),
            started: started.clone(),
        });
        let first = Task::new("first", AgentRole::SeniorDeveloper);
        let second = Task::new("second", AgentRole::SeniorDeveloper).with_dependency(first.id);
        let ids = (first.id, second.id);

        let report = orchestrator
            .run(vec![first, second], vec![agent], ProjectContext::default())
            .await
            .unwrap();
        assert_eq!(report.succeeded.len(), 2);
        assert_eq!(*started.lock().unwrap(), vec![ids.0, ids.1]);
    }

    #[tokio::test]
    async fn retries_transient_failures() {
        let orchestrator = Orchestrator::new(workspace(), Arc::new(AllowAllValidator), router())
            .with_config(OrchestratorConfig {
                max_concurrency: 1,
                max_attempts: 2,
            });
        let agent: Arc<dyn AgentRuntime> = Arc::new(FlakyAgent {
            identity: dev(),
            calls: AtomicU32::new(0),
            failures: 1,
        });

        let report = orchestrator
            .run(
                vec![Task::new("flaky", AgentRole::SeniorDeveloper)],
                vec![agent],
                ProjectContext::default(),
            )
            .await
            .unwrap();
        assert_eq!(report.succeeded.len(), 1);
        assert_eq!(report.succeeded[0].attempts, 2);
    }

    #[tokio::test]
    async fn failed_dependencies_skip_descendants() {
        let orchestrator = Orchestrator::new(workspace(), Arc::new(AllowAllValidator), router())
            .with_config(OrchestratorConfig {
                max_concurrency: 2,
                max_attempts: 1,
            });
        let rolled_back = Arc::new(AtomicBool::new(false));
        let failing: Arc<dyn AgentRuntime> = Arc::new(FailingAgent {
            identity: AgentIdentity::new(AgentRole::ProjectManager, "pm-01", "planner"),
            rolled_back: rolled_back.clone(),
        });
        let started = Arc::new(StdMutex::new(Vec::new()));
        let dependent: Arc<dyn AgentRuntime> = Arc::new(RecordingAgent {
            identity: dev(),
            started: started.clone(),
        });
        let first = Task::new("broken", AgentRole::ProjectManager);
        let second = Task::new("dependent", AgentRole::SeniorDeveloper).with_dependency(first.id);

        let report = orchestrator
            .run(
                vec![first, second],
                vec![failing, dependent],
                ProjectContext::default(),
            )
            .await
            .unwrap();
        assert_eq!(report.failed.len(), 1);
        assert_eq!(report.skipped.len(), 1);
        assert!(rolled_back.load(Ordering::SeqCst));
        assert!(started.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn validation_rejection_rolls_back_and_discards() {
        let orchestrator = Orchestrator::new(workspace(), Arc::new(DenyValidator), router())
            .with_config(OrchestratorConfig {
                max_concurrency: 1,
                max_attempts: 1,
            });
        let rolled_back = Arc::new(AtomicBool::new(false));
        let agent: Arc<dyn AgentRuntime> = Arc::new(PassAfterApplyAgent {
            identity: dev(),
            rolled_back: Some(rolled_back.clone()),
        });
        let report = orchestrator
            .run(
                vec![Task::new("rejected", AgentRole::SeniorDeveloper)],
                vec![agent],
                ProjectContext::default(),
            )
            .await
            .unwrap();
        assert_eq!(report.failed.len(), 1);
        assert!(report.failed[0].error.contains("validation rejected"));
        assert!(rolled_back.load(Ordering::SeqCst));
    }

    #[tokio::test]
    async fn dependency_cycles_are_rejected_before_execution() {
        let orchestrator = Orchestrator::new(workspace(), Arc::new(AllowAllValidator), router());
        let first = Task::new("a", AgentRole::SeniorDeveloper);
        let second = Task::new("b", AgentRole::SeniorDeveloper);
        let first = first.with_dependency(second.id);
        let second = second.with_dependency(first.id);
        let error = orchestrator
            .run(vec![first, second], Vec::new(), ProjectContext::default())
            .await
            .unwrap_err();
        assert!(matches!(error, CoreError::DependencyCycle { .. }));
    }

    #[tokio::test]
    async fn missing_agent_is_reported() {
        let orchestrator = Orchestrator::new(workspace(), Arc::new(AllowAllValidator), router());
        let task = Task::new("design", AgentRole::UiUxDesigner);
        let error = orchestrator
            .run(vec![task], Vec::new(), ProjectContext::default())
            .await
            .unwrap_err();
        assert!(matches!(error, CoreError::MissingAgent { .. }));
    }

    #[tokio::test]
    async fn successful_run_opens_pull_request() {
        let orchestrator = Orchestrator::new(workspace(), Arc::new(AllowAllValidator), router())
            .with_config(OrchestratorConfig {
                max_concurrency: 1,
                max_attempts: 1,
            });
        let agent: Arc<dyn AgentRuntime> = Arc::new(PassAfterApplyAgent {
            identity: dev(),
            rolled_back: None,
        });
        let report = orchestrator
            .run(
                vec![Task::new("ship", AgentRole::SeniorDeveloper)],
                vec![agent],
                ProjectContext::default(),
            )
            .await
            .unwrap();
        assert!(report.all_succeeded());
        assert_eq!(report.succeeded[0].files, vec!["src/generated.rs"]);
        assert!(report.succeeded[0].pr_id.starts_with("pr-"));
    }

    struct FeedbackAgent {
        identity: AgentIdentity,
        feedback: Arc<StdMutex<Vec<Option<String>>>>,
    }

    #[async_trait]
    impl AgentRuntime for FeedbackAgent {
        fn identity(&self) -> AgentIdentity {
            self.identity.clone()
        }

        async fn execute_task(&self, _ctx: TaskContext, task: Task) -> CoreResult<TaskResult> {
            self.feedback.lock().unwrap().push(
                task.metadata
                    .get(RETRY_FEEDBACK_METADATA)
                    .map(ToOwned::to_owned),
            );
            Ok(TaskResult::success(task.id, "fixed").with_changes(change()))
        }
    }

    struct FailOnceValidator {
        calls: AtomicU32,
    }

    #[async_trait]
    impl ChangeValidator for FailOnceValidator {
        async fn validate(&self, _changes: &StagedChanges) -> CoreResult<ValidationReport> {
            let call = self.calls.fetch_add(1, Ordering::SeqCst);
            if call == 0 {
                Ok(ValidationReport::failed(
                    "cargo test failed",
                    vec!["error[E0425]: cannot find function `generated`".to_owned()],
                    Duration::from_millis(1),
                ))
            } else {
                Ok(ValidationReport::passed(
                    "cargo test passed",
                    Vec::new(),
                    Duration::from_millis(1),
                ))
            }
        }
    }

    #[tokio::test]
    async fn validation_feedback_is_sent_to_the_next_attempt() {
        let feedback = Arc::new(StdMutex::new(Vec::new()));
        let agent: Arc<dyn AgentRuntime> = Arc::new(FeedbackAgent {
            identity: dev(),
            feedback: feedback.clone(),
        });
        let orchestrator = Orchestrator::new(
            workspace(),
            Arc::new(FailOnceValidator {
                calls: AtomicU32::new(0),
            }),
            router(),
        )
        .with_config(OrchestratorConfig {
            max_concurrency: 1,
            max_attempts: 2,
        });

        let report = orchestrator
            .run(
                vec![Task::new("fix compiler error", AgentRole::SeniorDeveloper)],
                vec![agent],
                ProjectContext::default(),
            )
            .await
            .unwrap();

        assert_eq!(report.succeeded.len(), 1);
        assert_eq!(report.succeeded[0].attempts, 2);
        let seen = feedback.lock().unwrap();
        assert!(seen[0].is_none());
        let retry = seen[1].as_deref().expect("second attempt gets feedback");
        assert!(retry.contains("E0425"));
        assert!(retry.contains("cargo test failed"));
    }
}
