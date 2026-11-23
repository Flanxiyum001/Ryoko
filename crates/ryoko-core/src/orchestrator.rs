//! Dependency-aware, concurrent task orchestration.
//!
//! Every task runs in its own [`WorkspaceLease`](crate::WorkspaceLease). Agent
//! output is staged there, validated by a [`ChangeValidator`] (the WASM sandbox
//! in production), and only then published as an internal pull request. A task
//! that fails validation is rolled back, its lease discarded, and its dependents
//! are skipped.
//!
//! A failing QA or Security node is not the end of the run. Its reported test
//! logs or audit findings are routed back to the producer that created the
//! branch under review, the review is repeated, and only after a configurable
//! number of repair cycles is the task escalated to human review.

use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::Arc;
use std::time::Instant;

use futures::FutureExt;
use serde::Serialize;
use tokio::sync::{Semaphore, watch};
use tokio::task::JoinSet;
use uuid::Uuid;

use crate::agent::{AgentRuntime, TaskContext};
use crate::agents::{RETRY_FEEDBACK_METADATA, task_branch_key, task_summary_key};
use crate::diagnostic::{DiagnosticLevel, TaskDiagnostic, TaskDiagnosticSink};
use crate::error::{CoreError, CoreResult};
use crate::event::{AgentPhase, EventSink, OrchestratorEvent, TaskFailureStage};
use crate::mcp::McpToolService;
use crate::memory::SharedMemory;
use crate::model::{AgentRole, ProjectContext, Task, TaskResult};
use crate::team::{ARCHITECT_ROLE, BACKEND_ROLE, SECURITY_ROLE};
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
    /// How many times a failing QA or Security node may send its failure back to
    /// the producer that owns the branch under review before the run escalates
    /// it to a human. Zero disables the repair loop entirely.
    pub max_repair_cycles: u32,
}

impl Default for OrchestratorConfig {
    fn default() -> Self {
        Self {
            max_concurrency: num_cpus_hint(),
            max_attempts: 2,
            max_repair_cycles: 1,
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
    /// Design feedback from a parsed visual review. Present only when the failure
    /// is actionable for the agent that produced the UI.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub visual_feedback: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct SkippedTask {
    pub task_id: Uuid,
    pub role: AgentRole,
    pub reason: String,
}

/// A failed review that the repair loop could not resolve. The run stops
/// mutating the repository for this task and hands it to a person.
#[derive(Debug, Clone, Serialize)]
pub struct HumanReview {
    pub task_id: Uuid,
    pub role: AgentRole,
    pub error: String,
    /// Producer task that received the failure logs, when one was identified.
    pub repaired_task: Option<Uuid>,
    /// Repair cycles spent before escalating.
    pub repair_cycles: u32,
}

/// One pass of the QA/Security repair loop, recorded so the operator can see
/// which producer was re-run and with what feedback.
#[derive(Debug, Clone, Serialize)]
pub struct RepairCycle {
    pub review_task: Uuid,
    pub producer_task: Uuid,
    pub role: AgentRole,
    pub cycle: u32,
    pub resolved: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct OrchestrationReport {
    pub run_id: Uuid,
    pub succeeded: Vec<TaskSuccess>,
    pub failed: Vec<TaskFailure>,
    pub skipped: Vec<SkippedTask>,
    /// Repairs the loop attempted, in execution order.
    #[serde(default)]
    pub repairs: Vec<RepairCycle>,
    /// Reviews that exhausted the repair budget. Those tasks remain in
    /// `failed`; this list explains what the loop already tried.
    #[serde(default)]
    pub needs_human_review: Vec<HumanReview>,
    pub duration_ms: u64,
}

impl OrchestrationReport {
    pub fn all_succeeded(&self) -> bool {
        self.failed.is_empty() && self.skipped.is_empty()
    }

    /// True when the repair loop gave up and a person must decide.
    pub fn escalated(&self) -> bool {
        !self.needs_human_review.is_empty()
    }

    pub fn pull_requests(&self) -> Vec<&TaskSuccess> {
        self.succeeded.iter().collect()
    }
}

#[derive(Clone, Debug)]
enum TaskOutcome {
    Succeeded {
        attempts: u32,
    },
    Failed {
        reason: String,
        attempts: u32,
        visual_feedback: Option<String>,
    },
    Skipped {
        reason: String,
    },
}

pub struct Orchestrator {
    workspace: Arc<dyn WorkspaceProvider>,
    validator: Arc<dyn ChangeValidator>,
    models: Arc<ModelRouter>,
    config: OrchestratorConfig,
    events: EventSink,
    /// Optional editor channel; absent when no editor host is attached.
    diagnostic_sink: Option<Arc<dyn TaskDiagnosticSink>>,
    /// Optional host-approved external tool surface shared by every model agent.
    mcp: Option<Arc<dyn McpToolService>>,
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
            diagnostic_sink: None,
            mcp: None,
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

    /// Attaches a host-approved MCP toolbox to every model agent. Approval,
    /// allowlisting and budget checks remain inside the toolbox.
    pub fn with_mcp(mut self, mcp: Option<Arc<dyn McpToolService>>) -> Self {
        self.mcp = mcp;
        self
    }

    /// Routes a task's validation result to a host-provided editor sink.
    ///
    /// The trait lives in this crate, so the orchestrator stays independent of
    /// any particular editor or language-server library. `ryoko-native`
    /// implements it over its LSP diagnostic channel.
    pub fn with_diagnostic_sink(mut self, sink: Arc<dyn TaskDiagnosticSink>) -> Self {
        self.diagnostic_sink = Some(sink);
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
        // The plan outlives the scheduler so the repair loop can identify each
        // review node's producer after the first pass has finished.
        let plan_tasks = tasks.clone();
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
                mcp: self.mcp.clone(),
                events: self.events.clone(),
            };
            let agent = registry[&task.assigned_to].clone();
            let workspace = self.workspace.clone();
            let events = self.events.clone();
            let diagnostic_sink = self.diagnostic_sink.clone();
            let semaphore = semaphore.clone();
            let mut dependencies = Vec::with_capacity(task.dependencies.len());
            for dependency in &task.dependencies {
                dependencies.push((*dependency, receivers[dependency].clone()));
            }

            self.events.emit(OrchestratorEvent::TaskScheduled {
                task_id: task.id,
                role: task.assigned_to.clone(),
                description: task.description.clone(),
                dependencies: task.dependencies.clone(),
            });

            let max_attempts = self.config.max_attempts.max(1);
            let task_on_panic = task.clone();
            join_set.spawn(async move {
                let operation = async {
                    let task = task;
                    let speculative = task.is_speculative();
                    if !speculative {
                        if let Some(reason) = wait_for_dependencies(&dependencies).await {
                            events.emit(OrchestratorEvent::TaskSkipped {
                                task_id: task.id,
                                role: task.assigned_to.clone(),
                                reason: reason.clone(),
                            });
                            return (task, None, TaskOutcome::Skipped { reason });
                        }
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
                                    visual_feedback: None,
                                },
                            );
                        }
                    };

                    let (outcome, success) = execute_task(
                        &task,
                        agent,
                        context.clone(),
                        workspace,
                        events.clone(),
                        max_attempts,
                        diagnostic_sink,
                    )
                    .await;
                    // Released before a speculative task waits on its
                    // dependencies, so it can never starve them of a permit.
                    drop(permit);

                    if speculative {
                        // Do not publish a speculative success to the event
                        // stream or shared memory until all dependencies pass.
                        if let Some(reason) = wait_for_dependencies(&dependencies).await {
                            let reason = format!("speculative result discarded: {reason}");
                            events.emit(OrchestratorEvent::TaskSkipped {
                                task_id: task.id,
                                role: task.assigned_to.clone(),
                                reason: reason.clone(),
                            });
                            return (task, None, TaskOutcome::Skipped { reason });
                        }
                        if let (TaskOutcome::Succeeded { attempts }, Some(success)) =
                            (&outcome, &success)
                        {
                            context.memory.insert(
                                task_summary_key(task.id),
                                success.output.chars().take(2_000).collect::<String>(),
                            );
                            events.emit(OrchestratorEvent::TaskSucceeded {
                                task_id: task.id,
                                role: task.assigned_to.clone(),
                                output: success.output.clone(),
                                attempts: *attempts,
                            });
                        }
                    }

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
                            visual_feedback: None,
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
            repairs: Vec::new(),
            needs_human_review: Vec::new(),
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
                        visual_feedback: None,
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
                TaskOutcome::Failed {
                    reason,
                    attempts,
                    visual_feedback,
                } => {
                    report.failed.push(TaskFailure {
                        task_id: task.id,
                        role: task.assigned_to.clone(),
                        error: reason,
                        attempts,
                        visual_feedback,
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

        self.repair_review_failures(&mut report, &plan_tasks, &registry, &project, &memory)
            .await;

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

    /// Routes a failing QA or Security node's findings back to the producer that
    /// owns the branch under review, then repeats the review. The loop is
    /// bounded by [`OrchestratorConfig::max_repair_cycles`]; whatever is still
    /// failing afterwards is escalated instead of silently skipped.
    ///
    /// Only producer roles receive repair work: a review never repairs another
    /// review. A repaired attempt still needs a fresh workspace lease, must pass
    /// validation and is published as an ordinary reviewable branch, so the loop
    /// never merges, pushes or deploys anything on its own.
    async fn repair_review_failures(
        &self,
        report: &mut OrchestrationReport,
        plan: &[Task],
        registry: &HashMap<AgentRole, Arc<dyn AgentRuntime>>,
        project: &ProjectContext,
        memory: &SharedMemory,
    ) {
        if self.config.max_repair_cycles == 0 {
            return;
        }
        for cycle in 1..=self.config.max_repair_cycles {
            let failures = report
                .failed
                .iter()
                .filter(|failure| is_actionable_review_failure(failure))
                .cloned()
                .collect::<Vec<_>>();
            if failures.is_empty() {
                break;
            }
            let mut progressed = false;
            for failure in failures {
                let Some(review_task) = plan.iter().find(|task| task.id == failure.task_id) else {
                    continue;
                };
                let Some(producer) = producer_for(review_task, plan) else {
                    continue;
                };
                self.events.emit(OrchestratorEvent::TaskProgress {
                    task_id: review_task.id,
                    message: format!(
                        "repair cycle {cycle}: routing {} findings to {} ({})",
                        failure.role, producer.assigned_to, producer.id
                    ),
                });
                // A visual review states the defects in design terms; that is
                // what the producer needs, so it takes precedence over the
                // short error line.
                let findings = failure.visual_feedback.as_deref().unwrap_or(&failure.error);
                let feedback = format!(
                    "{} rejected the previous change from {} with:\n\n{}\n\nFix that specific failure and keep the rest of the change intact. Return the full corrected file set.",
                    failure.role, producer.id, findings
                );
                let repaired = self
                    .replay(producer, &feedback, registry, project, memory)
                    .await;
                let repaired_ok = repaired.is_some();
                let resolved = if repaired_ok {
                    self.replay(review_task, findings, registry, project, memory)
                        .await
                } else {
                    None
                };
                report.repairs.push(RepairCycle {
                    review_task: review_task.id,
                    producer_task: producer.id,
                    role: producer.assigned_to.clone(),
                    cycle,
                    resolved: resolved.is_some(),
                });
                if let Some(success) = repaired {
                    record_success(report, success);
                }
                if let Some(success) = resolved {
                    report
                        .failed
                        .retain(|entry| entry.task_id != review_task.id);
                    record_success(report, success);
                }
                // The cycle is worth repeating whenever the producer actually
                // re-ran: a review that still fails after a real repair is a
                // new failure, not a repeat of the one that triggered the loop.
                if repaired_ok {
                    progressed = true;
                }
            }
            if !progressed {
                break;
            }
        }

        // Reviews still failing become an explicit human decision.
        let unresolved = report
            .failed
            .iter()
            .filter(|failure| is_actionable_review_failure(failure))
            .cloned()
            .collect::<Vec<_>>();
        for failure in unresolved {
            let mut repair_cycles = 0u32;
            let mut repaired_task = None;
            for repair in report
                .repairs
                .iter()
                .filter(|repair| repair.review_task == failure.task_id)
            {
                repair_cycles = repair_cycles.saturating_add(1);
                repaired_task = Some(repair.producer_task);
            }
            report.needs_human_review.push(HumanReview {
                task_id: failure.task_id,
                role: failure.role.clone(),
                error: failure.error.clone(),
                repaired_task,
                repair_cycles,
            });
            self.events.emit(OrchestratorEvent::TaskProgress {
                task_id: failure.task_id,
                message: format!(
                    "escalated to human review after {repair_cycles} repair cycle(s): {}",
                    failure.error
                ),
            });
        }

        // A node skipped only because a repaired dependency failed can run now
        // that its input exists again. Tasks are visited in plan order, which the
        // planners produce in topological order.
        let mut satisfied = report
            .succeeded
            .iter()
            .map(|success| success.task_id)
            .collect::<HashSet<_>>();
        let skipped = std::mem::take(&mut report.skipped);
        for entry in skipped {
            let Some(task) = plan.iter().find(|task| task.id == entry.task_id) else {
                report.skipped.push(entry);
                continue;
            };
            if !task.dependencies.iter().all(|id| satisfied.contains(id)) {
                report.skipped.push(entry);
                continue;
            }
            let feedback = "The dependency this task was waiting for failed and has since been repaired. Run the task again.";
            match self.replay(task, feedback, registry, project, memory).await {
                Some(success) => {
                    satisfied.insert(success.task_id);
                    record_success(report, success);
                }
                None => report.skipped.push(entry),
            }
        }
    }

    /// Runs one task outside the scheduler. Used only by the repair loop; the
    /// task still gets a fresh workspace lease, validation and publication, and
    /// its failure is returned as `None` instead of spawning new work.
    async fn replay(
        &self,
        task: &Task,
        feedback: &str,
        registry: &HashMap<AgentRole, Arc<dyn AgentRuntime>>,
        project: &ProjectContext,
        memory: &SharedMemory,
    ) -> Option<TaskSuccess> {
        let agent = registry.get(&task.assigned_to)?.clone();
        let mut attempt_task = task.clone();
        attempt_task
            .metadata
            .insert(RETRY_FEEDBACK_METADATA.to_owned(), cap_feedback(feedback));
        let context = TaskContext {
            project: project.clone(),
            memory: memory.clone(),
            models: self.models.clone(),
            validator: self.validator.clone(),
            workspace: None,
            mcp: self.mcp.clone(),
            events: self.events.clone(),
        };
        let (outcome, success) = execute_task(
            &attempt_task,
            agent,
            context,
            self.workspace.clone(),
            self.events.clone(),
            self.config.max_attempts.max(1),
            self.diagnostic_sink.clone(),
        )
        .await;
        match outcome {
            TaskOutcome::Succeeded { .. } => success,
            _ => None,
        }
    }
}

/// Failures the repair loop can hand to a producer: QA and Security reviews, plus
/// any failure that carries parsed visual-review feedback.
fn is_actionable_review_failure(failure: &TaskFailure) -> bool {
    is_reviewer_role(&failure.role) || failure.visual_feedback.is_some()
}

/// Reviews whose failure is actionable feedback for a producer rather than a
/// bug a producer can fix. QA and Security only.
fn is_reviewer_role(role: &AgentRole) -> bool {
    matches!(role, AgentRole::QaEngineer)
        || matches!(role, AgentRole::Custom(name) if name == SECURITY_ROLE)
}

/// Producers that can act on a review failure.
fn is_producer_role(role: &AgentRole) -> bool {
    matches!(role, AgentRole::SeniorDeveloper | AgentRole::UiUxDesigner)
        || matches!(role, AgentRole::Custom(name) if name == BACKEND_ROLE || name == ARCHITECT_ROLE)
}

/// Records a published task, replacing any earlier entry for the same task so a
/// replayed producer or review does not appear twice in the report.
fn record_success(report: &mut OrchestrationReport, success: TaskSuccess) {
    report
        .succeeded
        .retain(|entry| entry.task_id != success.task_id);
    report.succeeded.push(success);
}

/// The dependency that produced the branch a review node examined. The most
/// recent producer dependency wins when a reviewer has several, because that is
/// the branch the review most likely ran against.
fn producer_for<'a>(review: &Task, plan: &'a [Task]) -> Option<&'a Task> {
    review.dependencies.iter().rev().find_map(|id| {
        let candidate = plan.iter().find(|task| task.id == *id)?;
        is_producer_role(&candidate.assigned_to).then_some(candidate)
    })
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
    diagnostic_sink: Option<Arc<dyn TaskDiagnosticSink>>,
) -> (TaskOutcome, Option<TaskSuccess>) {
    let mut last_error = String::from("task did not run");
    let mut feedback: Option<String> = None;
    // Carried out of the attempt loop so a failed visual review reaches the
    // repair loop as design feedback rather than an opaque failure.
    let mut visual: Option<String> = None;
    let mut attempted_files: Vec<String> = Vec::new();
    let mut failure: Option<(FailureStage, String)> = None;
    for attempt in 1..=max_attempts {
        events.emit(OrchestratorEvent::TaskStarted {
            task_id: task.id,
            role: task.assigned_to.clone(),
            agent: agent.identity().name.clone(),
            attempt,
        });
        events.emit(OrchestratorEvent::TaskPhase {
            task_id: task.id,
            phase: AgentPhase::Thinking,
        });

        let lease = match workspace_provider.allocate(task, &agent.identity()).await {
            Ok(lease) => lease,
            Err(err) => {
                last_error = err.to_string();
                feedback = Some(last_error.clone());
                // No lease means this attempt proposed no files: do not report
                // files from an earlier attempt as if they belonged to it.
                attempted_files.clear();
                failure = None;
                events.emit(OrchestratorEvent::TaskFailed {
                    task_id: task.id,
                    role: task.assigned_to.clone(),
                    error: last_error.clone(),
                    attempts: attempt,
                    retrying: attempt < max_attempts,
                    stage: TaskFailureStage::Workspace,
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
        if let Some(usage) = ctx
            .models
            .ledger()
            .snapshot()
            .by_task
            .get(&task.id.to_string())
        {
            events.emit(OrchestratorEvent::TaskUsage {
                task_id: task.id,
                requests: usage.requests,
                input_tokens: usage.usage.input_tokens,
                output_tokens: usage.usage.output_tokens,
                ledger_cost_usd: usage.cost_usd,
            });
        }
        attempted_files = result.changes.iter().map(|c| c.path.clone()).collect();

        if !result.success {
            let reason = result
                .error
                .clone()
                .unwrap_or_else(|| "agent reported failure".to_owned());
            if let Some(feedback) = result.visual_feedback.clone() {
                visual = Some(feedback);
            }
            let _ = agent.rollback(ctx.clone(), task, &reason).await;
            let _ = lease.discard().await;
            last_error = reason.clone();
            feedback = Some(reason);
            failure = Some((FailureStage::Agent, last_error.clone()));
            events.emit(OrchestratorEvent::TaskFailed {
                task_id: task.id,
                role: task.assigned_to.clone(),
                error: last_error.clone(),
                attempts: attempt,
                retrying: attempt < max_attempts,
                stage: TaskFailureStage::Agent,
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
            failure = Some((FailureStage::Agent, last_error.clone()));
            events.emit(OrchestratorEvent::TaskFailed {
                task_id: task.id,
                role: task.assigned_to.clone(),
                error: last_error.clone(),
                attempts: attempt,
                retrying: attempt < max_attempts,
                stage: TaskFailureStage::Agent,
            });
            continue;
        }

        if task.is_speculative() && !result.changes.is_empty() {
            // Speculative tasks run before the code they review exists; a
            // file change from one is a contract violation, not a retryable
            // failure, and nothing it proposed is staged.
            let reason = "speculative task proposed file changes; it must be read-only".to_owned();
            let _ = agent.rollback(ctx.clone(), task, &reason).await;
            let _ = lease.discard().await;
            events.emit(OrchestratorEvent::TaskFailed {
                task_id: task.id,
                role: task.assigned_to.clone(),
                error: reason.clone(),
                attempts: attempt,
                retrying: false,
                stage: TaskFailureStage::Agent,
            });
            return (
                TaskOutcome::Failed {
                    reason,
                    attempts: attempt,
                    visual_feedback: None,
                },
                None,
            );
        }

        // A visual review verdicts; it proposes nothing. There is nothing to
        // stage, and the spec policy that requires tests for a change does not
        // apply, so the verdict is recorded without a branch.
        if result.success
            && result.changes.is_empty()
            && (task.is_speculative()
                || task
                    .metadata
                    .contains_key(crate::vision::VISUAL_REVIEW_METADATA))
        {
            let _ = lease.discard().await;
            if !task.is_speculative() {
                context.memory.insert(
                    task_summary_key(task.id),
                    result.output.chars().take(2_000).collect::<String>(),
                );
                events.emit(OrchestratorEvent::TaskSucceeded {
                    task_id: task.id,
                    role: task.assigned_to.clone(),
                    output: result.output.clone(),
                    attempts: attempt,
                });
            }
            return (
                TaskOutcome::Succeeded { attempts: attempt },
                Some(TaskSuccess {
                    task_id: task.id,
                    role: task.assigned_to.clone(),
                    agent: agent.identity().name.clone(),
                    output: result.output.clone(),
                    pr_id: String::new(),
                    branch: String::new(),
                    files: Vec::new(),
                    attempts: attempt,
                }),
            );
        }

        events.emit(OrchestratorEvent::TaskPhase {
            task_id: task.id,
            phase: AgentPhase::Coding,
        });
        if let Err(err) = lease.apply(&result.changes).await {
            let reason = err.to_string();
            let _ = agent.rollback(ctx.clone(), task, &reason).await;
            let _ = lease.discard().await;
            last_error = reason.clone();
            feedback = Some(reason);
            failure = Some((FailureStage::Staging, last_error.clone()));
            events.emit(OrchestratorEvent::TaskFailed {
                task_id: task.id,
                role: task.assigned_to.clone(),
                error: last_error.clone(),
                attempts: attempt,
                retrying: attempt < max_attempts,
                stage: TaskFailureStage::Staging,
            });
            continue;
        }

        events.emit(OrchestratorEvent::TaskPhase {
            task_id: task.id,
            phase: AgentPhase::Testing,
        });
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
            let next = prioritized_repair_feedback(&reason, &report.logs, &result.changes);
            last_error = reason;
            feedback = Some(next);
            failure = Some((FailureStage::Validation, report.summary.clone()));
            events.emit(OrchestratorEvent::TaskFailed {
                task_id: task.id,
                role: task.assigned_to.clone(),
                error: last_error.clone(),
                attempts: attempt,
                retrying: attempt < max_attempts,
                stage: TaskFailureStage::Validation,
            });
            continue;
        }

        match lease.publish(task, &result.output).await {
            Ok(pr) => {
                context.memory.insert(
                    task_summary_key(task.id),
                    result.output.chars().take(2_000).collect::<String>(),
                );
                context
                    .memory
                    .insert(task_branch_key(task.id), pr.branch.clone());
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
                if let Some(sink) = &diagnostic_sink {
                    publish_findings(
                        sink.as_ref(),
                        &context.project.project_root,
                        pr.files.iter().map(String::as_str),
                        DiagnosticLevel::Info,
                        "ryoko.validation",
                        &format!("Ryoko validation passed: {}", report.summary),
                    );
                }
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
                failure = Some((FailureStage::Publish, err.to_string()));
                events.emit(OrchestratorEvent::TaskFailed {
                    task_id: task.id,
                    role: task.assigned_to.clone(),
                    error: last_error.clone(),
                    attempts: attempt,
                    retrying: attempt < max_attempts,
                    stage: TaskFailureStage::Publish,
                });
            }
        }
    }

    // Every attempt is gone and nothing was proposed. If the last attempt
    // touched files, the editor is the only place left to explain the gap.
    if let (Some(sink), Some((stage, detail))) = (&diagnostic_sink, failure) {
        publish_failure(
            sink.as_ref(),
            &context.project.project_root,
            &attempted_files,
            stage,
            &detail,
            max_attempts,
        );
    }

    (
        TaskOutcome::Failed {
            reason: last_error,
            attempts: max_attempts,
            visual_feedback: visual,
        },
        None,
    )
}

/// Where an exhausted task stopped, which decides the finding code an editor
/// shows next to the files the task never got to publish.
#[derive(Debug, Clone, Copy)]
enum FailureStage {
    Agent,
    Staging,
    Validation,
    Publish,
}

impl FailureStage {
    fn code(self) -> &'static str {
        match self {
            Self::Agent => "ryoko.agent.failed",
            Self::Staging => "ryoko.staging.failed",
            Self::Validation => "ryoko.validation.failed",
            Self::Publish => "ryoko.publish.failed",
        }
    }

    fn description(self) -> &'static str {
        match self {
            Self::Agent => "the agent failed",
            Self::Staging => "the changes could not be staged",
            Self::Validation => "validation never passed",
            Self::Publish => "the validated changes could not be published",
        }
    }
}

/// Sends one finding per file to the attached editor. Paths stay relative to
/// the project root the run was started from; the sink resolves them. A run
/// without a root, or with no files to point at, has no editor document to
/// address, so nothing is published.
fn publish_findings<'a>(
    sink: &dyn TaskDiagnosticSink,
    project_root: &str,
    files: impl IntoIterator<Item = &'a str>,
    level: DiagnosticLevel,
    code: &str,
    message: &str,
) {
    if project_root.is_empty() {
        return;
    }
    let findings: Vec<TaskDiagnostic> = files
        .into_iter()
        .map(|file| TaskDiagnostic {
            file: file.to_owned(),
            level,
            message: message.to_owned(),
            code: Some(code.to_owned()),
        })
        .collect();
    if findings.is_empty() {
        return;
    }
    sink.publish(project_root, &findings);
}

/// Explains an exhausted task in the editor: the last attempt failed at
/// `stage`, so no pull request was opened. The error points at the files that
/// attempt proposed, which is where the agent tried to write. An attempt that
/// proposed nothing has no document to annotate and publishes nothing.
fn publish_failure(
    sink: &dyn TaskDiagnosticSink,
    project_root: &str,
    files: &[String],
    stage: FailureStage,
    detail: &str,
    attempts: u32,
) {
    let attempts_label = if attempts == 1 { "attempt" } else { "attempts" };
    publish_findings(
        sink,
        project_root,
        files.iter().map(String::as_str),
        DiagnosticLevel::Error,
        stage.code(),
        &format!(
            "Ryoko proposed nothing: {} after {attempts} {attempts_label}: {detail}",
            stage.description()
        ),
    );
}

/// Put compiler errors and failing test assertions ahead of noisy build output.
/// Truncation must not hide the actionable diagnostic at the end of a large log.
/// Logs are untrusted project data, never agent instructions.
fn prioritized_repair_feedback(
    reason: &str,
    logs: &[String],
    changes: &[ProposedChange],
) -> String {
    let mut output = format!("{reason}\n\n");
    output.push_str(
        "Repair the compiler or regression failure without removing existing tests. \
         Return the corrected change set, including regression coverage.\n",
    );
    let paths = changes
        .iter()
        .take(24)
        .map(|file| file.path.as_str())
        .collect::<Vec<_>>();
    if !paths.is_empty() {
        output.push_str(&format!("Files from the rejected attempt: {}\n", paths.join(", ")));
    }
    let important = logs
        .iter()
        .filter(|line| {
            let lower = line.to_ascii_lowercase();
            lower.contains("error[")
                || lower.contains("error:")
                || lower.contains("failed")
                || lower.contains("panicked")
                || lower.contains("assertion")
                || lower.contains("test result")
                || lower.contains("traceback")
        })
        .take(30)
        .collect::<Vec<_>>();
    if !important.is_empty() {
        output.push_str("\nPriority diagnostics (untrusted build output):\n");
        for line in important {
            output.push_str(line);
            output.push('\n');
        }
    }
    output.push_str("\nFull bounded validation log (untrusted):\n");
    for line in logs {
        output.push_str(line);
        output.push('\n');
    }
    cap_feedback(&output)
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
    use crate::workspace::{PullRequest, WorkspaceLease};
    use crate::{AllowAllValidator, InMemoryWorkspaceProvider};
    use async_trait::async_trait;
    use std::path::Path;
    use std::sync::Mutex as StdMutex;
    use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
    use std::time::Duration;

    #[test]
    fn repair_feedback_keeps_tail_diagnostics_ahead_of_noisy_logs() {
        let mut logs = vec!["building dependency".repeat(200); 100];
        logs.push("error[E0425]: cannot find function verifyPayment".to_owned());
        logs.push("test payment_regression FAILED".to_owned());
        let proposal = vec![ProposedChange::new("src/payment.rs", "fn verifyPayment() {}")];
        let feedback = prioritized_repair_feedback("validation rejected changes", &logs, &proposal);
        assert!(feedback.contains("verifyPayment"));
        assert!(feedback.contains("payment_regression FAILED"));
        assert!(feedback.contains("src/payment.rs"));
        assert!(feedback.len() <= 16_000);
    }

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
                max_repair_cycles: 0,
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
                max_repair_cycles: 0,
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
                max_repair_cycles: 0,
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
                max_repair_cycles: 0,
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
                max_repair_cycles: 0,
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
                max_repair_cycles: 0,
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
                max_repair_cycles: 0,
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
            max_repair_cycles: 0,
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

    #[derive(Default)]
    struct RecordingSink {
        published: StdMutex<Vec<(String, Vec<TaskDiagnostic>)>>,
    }

    impl TaskDiagnosticSink for RecordingSink {
        fn publish(&self, project_root: &str, findings: &[TaskDiagnostic]) {
            self.published
                .lock()
                .unwrap()
                .push((project_root.to_owned(), findings.to_vec()));
        }
    }

    #[tokio::test]
    async fn failed_validation_publishes_nothing_and_retry_publishes_one_finding() {
        let sink = Arc::new(RecordingSink::default());
        let orchestrator = Orchestrator::new(
            workspace(),
            Arc::new(FailOnceValidator {
                calls: AtomicU32::new(0),
            }),
            router(),
        )
        .with_diagnostic_sink(sink.clone());

        let report = orchestrator
            .run(
                vec![Task::new("generate a module", AgentRole::SeniorDeveloper)],
                vec![Arc::new(SleepAgent {
                    identity: dev(),
                    delay: Duration::ZERO,
                })],
                ProjectContext {
                    project_root: "/workspace/demo".to_owned(),
                    ..ProjectContext::default()
                },
            )
            .await
            .unwrap();

        assert_eq!(report.succeeded.len(), 1);
        assert_eq!(report.succeeded[0].attempts, 2);

        let published = sink.published.lock().unwrap();
        assert_eq!(
            published.len(),
            1,
            "the rejected attempt must publish nothing; only the passing attempt reports"
        );
        let (project_root, findings) = &published[0];
        assert_eq!(project_root, "/workspace/demo");
        assert_eq!(findings.len(), 1, "one finding per changed file");
        assert_eq!(findings[0].file, "src/generated.rs");
        assert_eq!(findings[0].level, DiagnosticLevel::Info);
        assert_eq!(findings[0].code.as_deref(), Some("ryoko.validation"));
        assert!(findings[0].message.contains("cargo test passed"));
    }

    #[tokio::test]
    async fn exhausted_validation_publishes_an_error_for_the_rejected_files() {
        let sink = Arc::new(RecordingSink::default());
        let orchestrator = Orchestrator::new(workspace(), Arc::new(DenyValidator), router())
            .with_diagnostic_sink(sink.clone())
            .with_config(OrchestratorConfig {
                max_concurrency: 1,
                max_attempts: 2,
                max_repair_cycles: 0,
            });

        let report = orchestrator
            .run(
                vec![Task::new("generate a module", AgentRole::SeniorDeveloper)],
                vec![Arc::new(SleepAgent {
                    identity: dev(),
                    delay: Duration::ZERO,
                })],
                ProjectContext {
                    project_root: "/workspace/demo".to_owned(),
                    ..ProjectContext::default()
                },
            )
            .await
            .unwrap();

        assert!(report.succeeded.is_empty());
        assert_eq!(report.failed.len(), 1);
        assert_eq!(report.failed[0].attempts, 2);

        let published = sink.published.lock().unwrap();
        assert_eq!(
            published.len(),
            1,
            "rejected attempts stay quiet until the retries run out"
        );
        let (project_root, findings) = &published[0];
        assert_eq!(project_root, "/workspace/demo");
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].file, "src/generated.rs");
        assert_eq!(findings[0].level, DiagnosticLevel::Error);
        assert_eq!(findings[0].code.as_deref(), Some("ryoko.validation.failed"));
        assert!(findings[0].message.contains("validation never passed"));
        assert!(findings[0].message.contains("cargo test failed"));
    }

    struct FailingWithChangesAgent {
        identity: AgentIdentity,
        changes: Vec<ProposedChange>,
    }

    #[async_trait]
    impl AgentRuntime for FailingWithChangesAgent {
        fn identity(&self) -> AgentIdentity {
            self.identity.clone()
        }

        async fn execute_task(&self, _ctx: TaskContext, task: Task) -> CoreResult<TaskResult> {
            Ok(TaskResult::failure(task.id, "model stopped early")
                .with_changes(self.changes.clone()))
        }

        async fn rollback(&self, _ctx: TaskContext, _task: &Task, _reason: &str) -> CoreResult<()> {
            Ok(())
        }
    }

    struct EscapingAgent {
        identity: AgentIdentity,
    }

    #[async_trait]
    impl AgentRuntime for EscapingAgent {
        fn identity(&self) -> AgentIdentity {
            self.identity.clone()
        }

        async fn execute_task(&self, _ctx: TaskContext, task: Task) -> CoreResult<TaskResult> {
            Ok(
                TaskResult::success(task.id, "wrote outside the workspace").with_changes(vec![
                    ProposedChange::new("../escape.rs", "pub fn escaped() {}"),
                ]),
            )
        }

        async fn rollback(&self, _ctx: TaskContext, _task: &Task, _reason: &str) -> CoreResult<()> {
            Ok(())
        }
    }

    /// Delegates to the in-memory provider but always fails to publish, so the
    /// task validates and then loses its pull request at the last step.
    struct FailingPublishProvider {
        inner: InMemoryWorkspaceProvider,
    }

    #[async_trait]
    impl WorkspaceProvider for FailingPublishProvider {
        async fn allocate(
            &self,
            task: &Task,
            agent: &AgentIdentity,
        ) -> CoreResult<Arc<dyn WorkspaceLease>> {
            let inner = self.inner.allocate(task, agent).await?;
            Ok(Arc::new(FailingPublishLease { inner }))
        }
    }

    struct FailingPublishLease {
        inner: Arc<dyn WorkspaceLease>,
    }

    #[async_trait]
    impl WorkspaceLease for FailingPublishLease {
        fn root(&self) -> &Path {
            self.inner.root()
        }

        fn branch(&self) -> &str {
            self.inner.branch()
        }

        async fn apply(&self, changes: &[ProposedChange]) -> CoreResult<()> {
            self.inner.apply(changes).await
        }

        async fn publish(&self, _task: &Task, _summary: &str) -> CoreResult<PullRequest> {
            Err(CoreError::Workspace("remote rejected the push".to_owned()))
        }

        async fn discard(&self) -> CoreResult<()> {
            self.inner.discard().await
        }
    }

    #[tokio::test]
    async fn exhausted_validation_emits_live_phases_and_a_specific_failure() {
        let events = EventSink::default();
        let mut receiver = events.subscribe();
        let orchestrator = Orchestrator::new(workspace(), Arc::new(DenyValidator), router())
            .with_events(events)
            .with_config(OrchestratorConfig {
                max_concurrency: 1,
                max_attempts: 1,
                max_repair_cycles: 0,
            });

        let report = orchestrator
            .run(
                vec![Task::new("generate a module", AgentRole::SeniorDeveloper)],
                vec![Arc::new(SleepAgent {
                    identity: dev(),
                    delay: Duration::ZERO,
                })],
                demo_project(),
            )
            .await
            .unwrap();
        assert_eq!(report.failed.len(), 1);

        let mut phases = Vec::new();
        let mut failures = Vec::new();
        while let Ok(event) = receiver.try_recv() {
            match event {
                OrchestratorEvent::TaskPhase { phase, .. } => phases.push(phase),
                OrchestratorEvent::TaskFailed {
                    stage, retrying, ..
                } => failures.push((stage, retrying)),
                _ => {}
            }
        }
        assert_eq!(
            phases,
            vec![
                AgentPhase::Thinking,
                AgentPhase::Coding,
                AgentPhase::Testing
            ]
        );
        assert_eq!(failures, vec![(TaskFailureStage::Validation, false)]);
    }

    fn demo_project() -> ProjectContext {
        ProjectContext {
            project_root: "/workspace/demo".to_owned(),
            ..ProjectContext::default()
        }
    }

    #[tokio::test]
    async fn agent_failure_publishes_an_error_once_attempts_run_out() {
        let sink = Arc::new(RecordingSink::default());
        let orchestrator = Orchestrator::new(workspace(), Arc::new(AllowAllValidator), router())
            .with_diagnostic_sink(sink.clone())
            .with_config(OrchestratorConfig {
                max_concurrency: 1,
                max_attempts: 2,
                max_repair_cycles: 0,
            });

        let report = orchestrator
            .run(
                vec![Task::new("generate a module", AgentRole::SeniorDeveloper)],
                vec![Arc::new(FailingWithChangesAgent {
                    identity: dev(),
                    changes: change(),
                })],
                demo_project(),
            )
            .await
            .unwrap();

        assert_eq!(report.failed.len(), 1);
        let published = sink.published.lock().unwrap();
        assert_eq!(
            published.len(),
            1,
            "reported once, after the retries run out"
        );
        let (project_root, findings) = &published[0];
        assert_eq!(project_root, "/workspace/demo");
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].file, "src/generated.rs");
        assert_eq!(findings[0].level, DiagnosticLevel::Error);
        assert_eq!(findings[0].code.as_deref(), Some("ryoko.agent.failed"));
        assert!(findings[0].message.contains("the agent failed"));
        assert!(findings[0].message.contains("model stopped early"));
    }

    #[tokio::test]
    async fn staging_failure_publishes_an_error_once_attempts_run_out() {
        let sink = Arc::new(RecordingSink::default());
        let orchestrator = Orchestrator::new(workspace(), Arc::new(AllowAllValidator), router())
            .with_diagnostic_sink(sink.clone())
            .with_config(OrchestratorConfig {
                max_concurrency: 1,
                max_attempts: 2,
                max_repair_cycles: 0,
            });

        let report = orchestrator
            .run(
                vec![Task::new("generate a module", AgentRole::SeniorDeveloper)],
                vec![Arc::new(EscapingAgent { identity: dev() })],
                demo_project(),
            )
            .await
            .unwrap();

        assert_eq!(report.failed.len(), 1);
        let published = sink.published.lock().unwrap();
        assert_eq!(published.len(), 1);
        let (project_root, findings) = &published[0];
        assert_eq!(project_root, "/workspace/demo");
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].file, "../escape.rs");
        assert_eq!(findings[0].level, DiagnosticLevel::Error);
        assert_eq!(findings[0].code.as_deref(), Some("ryoko.staging.failed"));
        assert!(findings[0].message.contains("could not be staged"));
        assert!(findings[0].message.contains("parent traversal"));
    }

    #[tokio::test]
    async fn publish_failure_publishes_an_error_once_attempts_run_out() {
        let sink = Arc::new(RecordingSink::default());
        let provider: Arc<dyn WorkspaceProvider> = Arc::new(FailingPublishProvider {
            inner: InMemoryWorkspaceProvider::new("main"),
        });
        let orchestrator = Orchestrator::new(provider, Arc::new(AllowAllValidator), router())
            .with_diagnostic_sink(sink.clone())
            .with_config(OrchestratorConfig {
                max_concurrency: 1,
                max_attempts: 2,
                max_repair_cycles: 0,
            });

        let report = orchestrator
            .run(
                vec![Task::new("generate a module", AgentRole::SeniorDeveloper)],
                vec![Arc::new(SleepAgent {
                    identity: dev(),
                    delay: Duration::ZERO,
                })],
                demo_project(),
            )
            .await
            .unwrap();

        assert_eq!(report.failed.len(), 1);
        let published = sink.published.lock().unwrap();
        assert_eq!(published.len(), 1);
        let (project_root, findings) = &published[0];
        assert_eq!(project_root, "/workspace/demo");
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].file, "src/generated.rs");
        assert_eq!(findings[0].level, DiagnosticLevel::Error);
        assert_eq!(findings[0].code.as_deref(), Some("ryoko.publish.failed"));
        assert!(findings[0].message.contains("could not be published"));
        assert!(findings[0].message.contains("remote rejected the push"));
    }

    /// Producer that remembers the feedback it was given, so a test can prove a
    /// review failure actually reached it.
    struct RepairTargetAgent {
        identity: AgentIdentity,
        feedback: Arc<StdMutex<Vec<Option<String>>>>,
    }

    #[async_trait]
    impl AgentRuntime for RepairTargetAgent {
        fn identity(&self) -> AgentIdentity {
            self.identity.clone()
        }

        async fn execute_task(&self, _ctx: TaskContext, task: Task) -> CoreResult<TaskResult> {
            self.feedback.lock().unwrap().push(
                task.metadata
                    .get(RETRY_FEEDBACK_METADATA)
                    .map(ToOwned::to_owned),
            );
            Ok(TaskResult::success(task.id, "repaired").with_changes(change()))
        }
    }

    /// Reviewer that fails a fixed number of times with a container/audit
    /// message, then passes.
    struct FlakyReviewer {
        identity: AgentIdentity,
        calls: AtomicU32,
        failures: u32,
        message: &'static str,
    }

    #[async_trait]
    impl AgentRuntime for FlakyReviewer {
        fn identity(&self) -> AgentIdentity {
            self.identity.clone()
        }

        async fn execute_task(&self, _ctx: TaskContext, task: Task) -> CoreResult<TaskResult> {
            let call = self.calls.fetch_add(1, Ordering::SeqCst);
            if call < self.failures {
                Ok(TaskResult::failure(task.id, self.message))
            } else {
                Ok(TaskResult::success(task.id, "verified").with_changes(change()))
            }
        }
    }

    fn qa_plan() -> (Task, Task) {
        let producer = Task::new("implement", AgentRole::SeniorDeveloper);
        let qa = Task::new("verify", AgentRole::QaEngineer).with_dependency(producer.id);
        (producer, qa)
    }

    #[tokio::test]
    async fn qa_failure_routes_test_logs_back_to_the_producer_and_recovers() {
        let (producer, qa) = qa_plan();
        let feedback = Arc::new(StdMutex::new(Vec::new()));
        let orchestrator = Orchestrator::new(workspace(), Arc::new(AllowAllValidator), router())
            .with_config(OrchestratorConfig {
                max_concurrency: 2,
                max_attempts: 1,
                max_repair_cycles: 1,
            });
        let agents: Vec<Arc<dyn AgentRuntime>> = vec![
            Arc::new(RepairTargetAgent {
                identity: dev(),
                feedback: feedback.clone(),
            }),
            Arc::new(FlakyReviewer {
                identity: AgentIdentity::new(AgentRole::QaEngineer, "qa-01", "verifier"),
                calls: AtomicU32::new(0),
                failures: 1,
                message: "container test failed: expected 200, got 500",
            }),
        ];

        let report = orchestrator
            .run(
                vec![producer, qa.clone()],
                agents,
                ProjectContext::default(),
            )
            .await
            .unwrap();

        assert!(report.failed.is_empty(), "{:?}", report.failed);
        assert!(report.needs_human_review.is_empty());
        assert_eq!(report.repairs.len(), 1);
        assert!(report.repairs[0].resolved);
        assert_eq!(report.repairs[0].review_task, qa.id);
        assert!(
            report
                .succeeded
                .iter()
                .any(|success| success.task_id == qa.id),
            "the repaired review must be published again"
        );
        let seen = feedback.lock().unwrap();
        assert_eq!(
            seen.len(),
            2,
            "the producer runs once normally and once to repair"
        );
        assert!(seen[0].is_none());
        let repair = seen[1]
            .as_deref()
            .expect("repair feedback reaches the producer");
        assert!(repair.contains("expected 200, got 500"), "{repair}");
        assert!(repair.contains("qa-engineer"), "{repair}");
    }

    /// Reviewer that fails with parsed visual-review feedback before passing.
    struct VisualReviewer {
        identity: AgentIdentity,
        calls: AtomicU32,
        failures: u32,
        feedback: &'static str,
    }

    #[async_trait]
    impl AgentRuntime for VisualReviewer {
        fn identity(&self) -> AgentIdentity {
            self.identity.clone()
        }

        async fn execute_task(&self, _ctx: TaskContext, task: Task) -> CoreResult<TaskResult> {
            let call = self.calls.fetch_add(1, Ordering::SeqCst);
            if call < self.failures {
                Ok(TaskResult::failure(task.id, "visual review failed")
                    .with_visual_feedback(self.feedback))
            } else {
                Ok(TaskResult::success(task.id, "visual review passed").with_changes(change()))
            }
        }
    }

    #[tokio::test]
    async fn a_failed_visual_review_routes_design_feedback_to_the_producer() {
        let producer = Task::new("build the checkout screen", AgentRole::SeniorDeveloper);
        let review = Task::new("review the rendered UI", AgentRole::UiUxDesigner)
            .with_dependency(producer.id);
        let feedback = Arc::new(StdMutex::new(Vec::new()));
        let orchestrator = Orchestrator::new(workspace(), Arc::new(AllowAllValidator), router())
            .with_config(OrchestratorConfig {
                max_concurrency: 2,
                max_attempts: 1,
                max_repair_cycles: 1,
            });
        let agents: Vec<Arc<dyn AgentRuntime>> = vec![
            Arc::new(RepairTargetAgent {
                identity: dev(),
                feedback: feedback.clone(),
            }),
            Arc::new(VisualReviewer {
                identity: AgentIdentity::new(AgentRole::UiUxDesigner, "ui-01", "designer"),
                calls: AtomicU32::new(0),
                failures: 1,
                feedback: "checkout CTA contrast is 2.1:1; raise it to 4.5:1",
            }),
        ];

        let report = orchestrator
            .run(
                vec![producer, review.clone()],
                agents,
                ProjectContext::default(),
            )
            .await
            .unwrap();

        assert!(report.failed.is_empty(), "{:?}", report.failed);
        assert!(report.needs_human_review.is_empty());
        assert_eq!(report.repairs.len(), 1);
        assert!(report.repairs[0].resolved);
        assert_eq!(report.repairs[0].review_task, review.id);
        assert_eq!(report.repairs[0].role, AgentRole::SeniorDeveloper);
        let seen = feedback.lock().unwrap();
        assert_eq!(seen.len(), 2, "the producer runs once and once to repair");
        let repair = seen[1]
            .as_deref()
            .expect("design feedback reaches the producer");
        assert!(repair.contains("2.1:1"), "{repair}");
        assert!(repair.contains("ui-ux-designer"), "{repair}");
    }

    #[test]
    fn only_reviews_and_visual_failures_are_actionable() {
        let mut failure = TaskFailure {
            task_id: Uuid::new_v4(),
            role: AgentRole::UiUxDesigner,
            error: "visual review failed".to_owned(),
            attempts: 1,
            visual_feedback: None,
        };
        assert!(!is_actionable_review_failure(&failure));
        failure.visual_feedback = Some("contrast too low".to_owned());
        assert!(is_actionable_review_failure(&failure));
    }

    /// Review that passes without proposing any file, which is what a clean
    /// visual critique produces.
    struct SilentReviewer {
        identity: AgentIdentity,
    }

    #[async_trait]
    impl AgentRuntime for SilentReviewer {
        fn identity(&self) -> AgentIdentity {
            self.identity.clone()
        }

        async fn execute_task(&self, _ctx: TaskContext, task: Task) -> CoreResult<TaskResult> {
            Ok(TaskResult::success(task.id, "visual review passed"))
        }
    }

    /// Mirrors the spec policy: a change set without tests is rejected. A review
    /// must never reach it.
    struct EmptyChangeRejected;

    #[async_trait]
    impl ChangeValidator for EmptyChangeRejected {
        async fn validate(&self, changes: &StagedChanges) -> CoreResult<ValidationReport> {
            if changes.files.is_empty() {
                return Ok(ValidationReport::failed(
                    "spec check failed",
                    vec!["missing tests".to_owned()],
                    Duration::ZERO,
                ));
            }
            Ok(ValidationReport::passed(
                "pre-flight passed",
                Vec::new(),
                Duration::ZERO,
            ))
        }
    }

    #[tokio::test]
    async fn a_passing_visual_review_succeeds_without_proposing_files() {
        let producer = Task::new("build the checkout screen", AgentRole::SeniorDeveloper);
        let review = Task::new("review the rendered UI", AgentRole::UiUxDesigner)
            .depends_on(&producer)
            .with_metadata(crate::vision::VISUAL_REVIEW_METADATA, "true");
        let orchestrator = Orchestrator::new(workspace(), Arc::new(EmptyChangeRejected), router())
            .with_config(OrchestratorConfig {
                max_concurrency: 2,
                max_attempts: 1,
                max_repair_cycles: 1,
            });
        let agents: Vec<Arc<dyn AgentRuntime>> = vec![
            Arc::new(SleepAgent {
                identity: dev(),
                delay: Duration::ZERO,
            }),
            Arc::new(SilentReviewer {
                identity: AgentIdentity::new(AgentRole::UiUxDesigner, "ui-01", "designer"),
            }),
        ];

        let report = orchestrator
            .run(
                vec![producer, review.clone()],
                agents,
                ProjectContext::default(),
            )
            .await
            .unwrap();

        assert!(report.failed.is_empty(), "{:?}", report.failed);
        assert!(
            report
                .succeeded
                .iter()
                .any(|success| success.task_id == review.id),
            "a clean review is a success, not a skipped node"
        );
    }

    #[tokio::test]
    async fn a_review_that_keeps_failing_escalates_to_human_review() {
        let (producer, qa) = qa_plan();
        let feedback = Arc::new(StdMutex::new(Vec::new()));
        let orchestrator = Orchestrator::new(workspace(), Arc::new(AllowAllValidator), router())
            .with_config(OrchestratorConfig {
                max_concurrency: 2,
                max_attempts: 1,
                max_repair_cycles: 2,
            });
        let agents: Vec<Arc<dyn AgentRuntime>> = vec![
            Arc::new(RepairTargetAgent {
                identity: dev(),
                feedback: feedback.clone(),
            }),
            Arc::new(FlakyReviewer {
                identity: AgentIdentity::new(AgentRole::QaEngineer, "qa-01", "verifier"),
                calls: AtomicU32::new(0),
                failures: u32::MAX,
                message: "audit finding: unauthenticated write endpoint",
            }),
        ];

        let report = orchestrator
            .run(
                vec![producer, qa.clone()],
                agents,
                ProjectContext::default(),
            )
            .await
            .unwrap();

        assert!(report.escalated());
        assert_eq!(report.needs_human_review.len(), 1);
        let escalated = &report.needs_human_review[0];
        assert_eq!(escalated.task_id, qa.id);
        assert_eq!(escalated.repair_cycles, 2);
        assert!(escalated.repaired_task.is_some());
        assert!(escalated.error.contains("audit finding"));
        assert!(report.failed.iter().any(|failure| failure.task_id == qa.id));
        assert!(report.repairs.iter().all(|repair| !repair.resolved));
    }

    #[tokio::test]
    async fn a_dependent_skipped_by_the_failed_review_runs_after_the_repair() {
        let producer = Task::new("implement", AgentRole::SeniorDeveloper);
        let qa = Task::new("verify", AgentRole::QaEngineer).with_dependency(producer.id);
        let integrator = Task::new("integrate", AgentRole::Integrator).with_dependency(qa.id);
        let orchestrator = Orchestrator::new(workspace(), Arc::new(AllowAllValidator), router())
            .with_config(OrchestratorConfig {
                max_concurrency: 2,
                max_attempts: 1,
                max_repair_cycles: 1,
            });
        let agents: Vec<Arc<dyn AgentRuntime>> = vec![
            Arc::new(RepairTargetAgent {
                identity: dev(),
                feedback: Arc::new(StdMutex::new(Vec::new())),
            }),
            Arc::new(FlakyReviewer {
                identity: AgentIdentity::new(AgentRole::QaEngineer, "qa-01", "verifier"),
                calls: AtomicU32::new(0),
                failures: 1,
                message: "container test failed",
            }),
            Arc::new(RepairTargetAgent {
                identity: AgentIdentity::new(AgentRole::Integrator, "integrator", "merger"),
                feedback: Arc::new(StdMutex::new(Vec::new())),
            }),
        ];

        let report = orchestrator
            .run(
                vec![producer, qa.clone(), integrator.clone()],
                agents,
                ProjectContext::default(),
            )
            .await
            .unwrap();

        assert!(report.skipped.is_empty(), "{:?}", report.skipped);
        assert!(report.failed.is_empty(), "{:?}", report.failed);
        assert!(
            report
                .succeeded
                .iter()
                .any(|success| success.task_id == integrator.id)
        );
    }

    #[tokio::test]
    async fn repair_cycles_of_zero_leave_the_original_behaviour() {
        let (producer, qa) = qa_plan();
        let orchestrator = Orchestrator::new(workspace(), Arc::new(AllowAllValidator), router())
            .with_config(OrchestratorConfig {
                max_concurrency: 2,
                max_attempts: 1,
                max_repair_cycles: 0,
            });
        let agents: Vec<Arc<dyn AgentRuntime>> = vec![
            Arc::new(RepairTargetAgent {
                identity: dev(),
                feedback: Arc::new(StdMutex::new(Vec::new())),
            }),
            Arc::new(FlakyReviewer {
                identity: AgentIdentity::new(AgentRole::QaEngineer, "qa-01", "verifier"),
                calls: AtomicU32::new(0),
                failures: 1,
                message: "container test failed",
            }),
        ];
        let report = orchestrator
            .run(vec![producer, qa], agents, ProjectContext::default())
            .await
            .unwrap();
        assert_eq!(report.failed.len(), 1);
        assert!(report.repairs.is_empty());
        assert!(report.needs_human_review.is_empty());
    }

    /// Read-only reviewer that sleeps, then reports without proposing files.
    struct ReadOnlyAgent {
        identity: AgentIdentity,
        delay: Duration,
        started: Arc<StdMutex<Option<Instant>>>,
        propose: bool,
        calls: Arc<AtomicU32>,
    }

    #[async_trait]
    impl AgentRuntime for ReadOnlyAgent {
        fn identity(&self) -> AgentIdentity {
            self.identity.clone()
        }

        async fn execute_task(&self, _ctx: TaskContext, task: Task) -> CoreResult<TaskResult> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            self.started
                .lock()
                .unwrap()
                .get_or_insert_with(Instant::now);
            tokio::time::sleep(self.delay).await;
            let result = TaskResult::success(task.id, "audit clean");
            Ok(if self.propose {
                result.with_changes(change())
            } else {
                result
            })
        }
    }

    fn security(delay: Duration, propose: bool) -> (Arc<ReadOnlyAgent>, Arc<AtomicU32>) {
        let calls = Arc::new(AtomicU32::new(0));
        let agent = Arc::new(ReadOnlyAgent {
            identity: AgentIdentity::new(
                AgentRole::Custom("security".to_owned()),
                "sec-01",
                "security",
            ),
            delay,
            started: Arc::new(StdMutex::new(None)),
            propose,
            calls: calls.clone(),
        });
        (agent, calls)
    }

    fn concurrent(max_attempts: u32) -> Orchestrator {
        Orchestrator::new(workspace(), Arc::new(AllowAllValidator), router()).with_config(
            OrchestratorConfig {
                max_concurrency: 2,
                max_attempts,
                max_repair_cycles: 0,
            },
        )
    }

    #[tokio::test]
    async fn speculative_tasks_overlap_the_work_they_depend_on() {
        let producer = Task::new("implement", AgentRole::SeniorDeveloper);
        let audit = Task::new(
            "audit dependencies",
            AgentRole::Custom("security".to_owned()),
        )
        .depends_on(&producer)
        .speculative();
        let (sec, _) = security(Duration::from_millis(200), false);
        let started = sec.started.clone();
        let agents: Vec<Arc<dyn AgentRuntime>> = vec![
            Arc::new(SleepAgent {
                identity: dev(),
                delay: Duration::from_millis(200),
            }),
            sec,
        ];

        let run_start = Instant::now();
        let report = concurrent(1)
            .run(
                vec![producer, audit.clone()],
                agents,
                ProjectContext::default(),
            )
            .await
            .unwrap();
        let audit_started = started.lock().unwrap().expect("audit ran");
        assert!(
            audit_started.duration_since(run_start) < Duration::from_millis(150),
            "the audit must not wait for the producer"
        );
        assert!(run_start.elapsed() < Duration::from_millis(390));
        assert!(report.succeeded.iter().any(|s| s.task_id == audit.id));
        assert!(report.failed.is_empty(), "{:?}", report.failed);
    }

    #[tokio::test]
    async fn speculative_results_are_discarded_when_a_dependency_fails() {
        let producer = Task::new("broken", AgentRole::ProjectManager);
        let audit = Task::new("audit", AgentRole::Custom("security".to_owned()))
            .depends_on(&producer)
            .speculative();
        let (sec, calls) = security(Duration::ZERO, false);
        let agents: Vec<Arc<dyn AgentRuntime>> = vec![
            Arc::new(FailingAgent {
                identity: AgentIdentity::new(AgentRole::ProjectManager, "pm-01", "planner"),
                rolled_back: Arc::new(AtomicBool::new(false)),
            }),
            sec,
        ];

        let events = EventSink::default();
        let mut emitted = events.subscribe();
        let report = concurrent(1)
            .with_events(events)
            .run(
                vec![producer, audit.clone()],
                agents,
                ProjectContext::default(),
            )
            .await
            .unwrap();
        assert_eq!(calls.load(Ordering::SeqCst), 1, "it ran speculatively");
        // A discarded speculative result must not leak a false success to
        // Mission Control (or to shared task summaries).
        let mut reported_success = false;
        while let Ok(event) = emitted.try_recv() {
            if let OrchestratorEvent::TaskSucceeded { task_id, .. } = event {
                reported_success |= task_id == audit.id;
            }
        }
        assert!(!reported_success, "discarded task emitted TaskSucceeded");
        assert!(report.succeeded.iter().all(|s| s.task_id != audit.id));
        let skipped = report
            .skipped
            .iter()
            .find(|s| s.task_id == audit.id)
            .expect("discarded result is reported as skipped");
        assert!(skipped.reason.starts_with("speculative result discarded"));
    }

    #[tokio::test]
    async fn speculative_tasks_may_not_propose_file_changes() {
        let audit = Task::new("audit", AgentRole::Custom("security".to_owned())).speculative();
        let (sec, calls) = security(Duration::ZERO, true);
        let workspace = workspace();
        let orchestrator =
            Orchestrator::new(workspace.clone(), Arc::new(AllowAllValidator), router())
                .with_config(OrchestratorConfig {
                    max_concurrency: 1,
                    max_attempts: 3,
                    max_repair_cycles: 0,
                });

        let report = orchestrator
            .run(vec![audit], vec![sec], ProjectContext::default())
            .await
            .unwrap();
        assert_eq!(report.failed.len(), 1);
        assert!(report.failed[0].error.contains("must be read-only"));
        assert_eq!(
            calls.load(Ordering::SeqCst),
            1,
            "a contract violation is not retried"
        );
        assert!(report.succeeded.is_empty());
    }
}
