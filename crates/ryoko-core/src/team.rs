//! Opinionated, extensible team profiles for an application-building IDE.
//! These are role contracts, not grants of filesystem, shell or network access.
use crate::model::{AgentRole, Task};

/// Task metadata key naming the verification steps a task must run before it
/// may be validated. Values are a comma-separated list such as
/// `headless-browser,screenshot`.
pub const VERIFICATION_METADATA: &str = "ryoko.verify";

pub const ARCHITECT_ROLE: &str = "software-architect";
pub const BACKEND_ROLE: &str = "backend-engineer";
pub const SECURITY_ROLE: &str = "security-engineer";
pub const DEVOPS_ROLE: &str = "devops-engineer";

#[derive(Debug, Clone)]
pub struct TeamMember {
    pub role: AgentRole,
    pub title: &'static str,
    pub instructions: &'static str,
}

pub fn standard_team() -> Vec<TeamMember> {
    vec![
        TeamMember {
            role: AgentRole::ProjectManager,
            title: "Project manager",
            instructions: "Turn the user's goal into prioritized, testable acceptance criteria, explicit non-goals and a dependency plan. Never claim unverified work is complete.",
        },
        TeamMember {
            role: AgentRole::Custom("software-architect".into()),
            title: "Software architect",
            instructions: "Specify boundaries, data contracts, deployment topology, failure modes, extensibility and maintainability. Make trade-offs and assumptions explicit.",
        },
        TeamMember {
            role: AgentRole::UiUxDesigner,
            title: "Senior UI/UX designer",
            instructions: "Design accessible user journeys, screen hierarchy, interaction states, responsive layouts and coherent visual design. Include loading, error and empty states.",
        },
        TeamMember {
            role: AgentRole::Custom("backend-engineer".into()),
            title: "Senior backend engineer",
            instructions: "Define APIs, schemas, authentication, data consistency, migrations and integration contracts. Prefer secure defaults and idempotent operations.",
        },
        TeamMember {
            role: AgentRole::SeniorDeveloper,
            title: "Senior full-stack engineer",
            instructions: "Implement reviewable and maintainable changes against the agreed contracts. Explain changed files and verification; never invent test results.",
        },
        TeamMember {
            role: AgentRole::Custom("security-engineer".into()),
            title: "Security engineer",
            instructions: "Threat-model trust boundaries, untrusted inputs, secrets, prompt injection, filesystem confinement and dependency risks. Propose concrete mitigating tests.",
        },
        TeamMember {
            role: AgentRole::QaEngineer,
            title: "QA automation engineer",
            instructions: "Write acceptance, regression, accessibility, edge-case and failure-mode test plans. Distinguish proposed tests from executed results.",
        },
        TeamMember {
            role: AgentRole::Custom("devops-engineer".into()),
            title: "DevOps/SRE engineer",
            instructions: "Design reproducible builds, staged deployments, secrets handling, health checks, telemetry and rollback. Require explicit approval for live deployment.",
        },
        TeamMember {
            role: AgentRole::Integrator,
            title: "Merge integrator",
            instructions: "Combine validated sibling branches in a new worktree, resolve semantic conflicts conservatively with a model, preserve every accepted test and security fix, and publish one reviewable integration diff. Never merge to the target branch or discard a sibling branch.",
        },
    ]
}

/// Deterministic project decomposition. The caller still validates this DAG
/// before executing anything and requires independent approval for mutations.
pub fn application_plan(goal: &str) -> Vec<Task> {
    let pm = Task::new(
        format!("Scope and acceptance criteria for: {goal}"),
        AgentRole::ProjectManager,
    );
    let architect = Task::new(
        format!("Architecture and interfaces for: {goal}"),
        AgentRole::Custom("software-architect".into()),
    )
    .depends_on(&pm);
    let ux = Task::new(
        format!("Accessible screens and user flows for: {goal}"),
        AgentRole::UiUxDesigner,
    )
    .depends_on(&architect);
    let backend = Task::new(
        format!("Backend APIs, storage and auth for: {goal}"),
        AgentRole::Custom("backend-engineer".into()),
    )
    .depends_on(&architect);
    let implementation = Task::new(
        format!("Integrate frontend and backend for: {goal}"),
        AgentRole::SeniorDeveloper,
    )
    .depends_on(&ux)
    .depends_on(&backend);
    let security = Task::new(
        format!("Threat model and security review for: {goal}"),
        AgentRole::Custom("security-engineer".into()),
    )
    .depends_on(&implementation);
    let qa = Task::new(
        format!("Acceptance and regression verification for: {goal}"),
        AgentRole::QaEngineer,
    )
    .depends_on(&implementation);
    let operations = Task::new(
        format!("Staging, monitoring and rollback plan for: {goal}"),
        AgentRole::Custom("devops-engineer".into()),
    )
    .depends_on(&security)
    .depends_on(&qa);
    vec![
        pm,
        architect,
        ux,
        backend,
        implementation,
        security,
        qa,
        operations,
    ]
}

/// Which specialist roles a goal actually needs.
///
/// This is a routing hint, never a permission: it only decides which DAG nodes
/// the project manager activates. Detecting a keyword must not, and does not,
/// grant a role extra filesystem, network or deployment access.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct GoalSignals {
    pub needs_design: bool,
    pub needs_backend: bool,
    pub needs_operations: bool,
}

impl GoalSignals {
    /// Classifies a goal by conservative keyword families. Anything unrecognised
    /// defaults to a backend slice, because every deliverable needs a producer;
    /// design and operations stay off unless the goal names them.
    pub fn from_goal(goal: &str) -> Self {
        let lower = goal.to_ascii_lowercase();
        let has = |needles: &[&str]| needles.iter().any(|needle| lower.contains(needle));
        let needs_design = has(&[
            "ui",
            "ux",
            "design",
            "screen",
            "page",
            "layout",
            "responsive",
            "accessib",
            "css",
            "theme",
            "dashboard",
            "onboarding flow",
            "mockup",
            "component",
        ]);
        let needs_backend = has(&[
            "api",
            "endpoint",
            "backend",
            "server",
            "database",
            "schema",
            "migration",
            "auth",
            "storage",
            "queue",
            "persistence",
        ]) || !needs_design;
        let needs_operations = has(&[
            "deploy",
            "deployment",
            "ci",
            "cd",
            "pipeline",
            "infrastructure",
            "docker",
            "kubernetes",
            "terraform",
            "observability",
            "monitoring",
            "rollback",
            "staging",
            "release",
        ]);
        Self {
            needs_design,
            needs_backend,
            needs_operations,
        }
    }
}

/// A project-manager plan for one goal: the activated specialists plus the
/// ordered task DAG they will run.
#[derive(Debug, Clone)]
pub struct DynamicPlan {
    pub signals: GoalSignals,
    /// The project-manager task that scopes and decomposes the goal. Every
    /// other task descends from it, so decomposition always runs first.
    pub decomposition: Task,
    pub tasks: Vec<Task>,
}

impl DynamicPlan {
    pub fn activated_roles(&self) -> Vec<AgentRole> {
        self.tasks
            .iter()
            .map(|task| task.assigned_to.clone())
            .collect()
    }

    /// The plan as a reviewable document: the project-manager decomposition plus
    /// every activated specialist task.
    pub fn all_tasks(&self) -> Vec<Task> {
        let mut tasks = vec![self.decomposition.clone()];
        tasks.extend(self.tasks.iter().cloned());
        tasks
    }

    /// The executable DAG. Decomposition is deterministic, so the project
    /// manager's output here is the plan itself rather than a worktree branch:
    /// the remaining tasks are re-rooted by dropping the dependency on the
    /// decomposition node. Every other edge is preserved.
    pub fn execution_tasks(&self) -> Vec<Task> {
        self.tasks
            .iter()
            .map(|task| {
                let mut task = task.clone();
                task.dependencies.retain(|id| *id != self.decomposition.id);
                task
            })
            .collect()
    }
}

/// Dynamic DAG routing. The project manager decomposes the goal first, then
/// only the specialists the goal requires are activated:
///
/// - UI/UX (with headless-browser and screenshot verification) and DevOps are
///   added only when [`GoalSignals`] detects them;
/// - backend, security and QA always run, because a change without a producer
///   or a review is not reviewable;
/// - the integrator is appended only when QA and Security both published a
///   branch, which is the integrator's documented minimum.
pub fn dynamic_plan(goal: &str) -> DynamicPlan {
    let signals = GoalSignals::from_goal(goal);
    let decomposition = Task::new(
        format!(
            "SCOPE: Break this goal into prioritized sub-tasks with testable acceptance criteria, explicit non-goals and a dependency plan: {goal}"
        ),
        AgentRole::ProjectManager,
    );
    let mut tasks = Vec::new();
    // A goal with a screen in it needs browser evidence, and QA repeats the same
    // requirement so the check is not lost downstream.
    let verification = if signals.needs_design {
        "headless-browser,screenshot"
    } else {
        "unit"
    };

    let architect = Task::new(
        format!("ARCHITECT: Implement a narrow, testable architecture/interface slice for: {goal}. Produce working code and regression tests, not only advice."),
        AgentRole::Custom(ARCHITECT_ROLE.to_owned()),
    )
    .depends_on(&decomposition);
    tasks.push(architect.clone());

    // At least one producer always exists: `needs_backend` is true unless the
    // goal is interface-only, in which case design supplies it.
    let mut producers = Vec::new();
    if signals.needs_design {
        let design = Task::new(
            format!("UIUX: Implement accessible screens and interaction states for: {goal}. Include loading, error and empty states."),
            AgentRole::UiUxDesigner,
        )
        .depends_on(&architect)
        .with_metadata(VERIFICATION_METADATA, verification);
        producers.push(design.clone());
        tasks.push(design);
    }
    if signals.needs_backend {
        let backend = Task::new(
            format!("BACKEND: Implement backend behavior and regression tests for: {goal}. Extend the validated architect branch."),
            AgentRole::Custom(BACKEND_ROLE.to_owned()),
        )
        .depends_on(&architect);
        producers.push(backend.clone());
        tasks.push(backend);
    }

    let mut implementation = Task::new(
        format!("FULLSTACK: Implement the agreed contracts and regression tests for: {goal}."),
        AgentRole::SeniorDeveloper,
    )
    .with_metadata(VERIFICATION_METADATA, verification);
    for producer in &producers {
        implementation = implementation.depends_on(producer);
    }
    tasks.push(implementation.clone());

    let qa = Task::new(
        format!("QA: Implement executable regression tests for: {goal}. Distinguish proposed tests from executed results."),
        AgentRole::QaEngineer,
    )
    .depends_on(&implementation)
    .with_metadata(VERIFICATION_METADATA, verification);
    let security = Task::new(
        format!(
            "SECURITY: Implement security hardening and adversarial regression tests for: {goal}."
        ),
        AgentRole::Custom(SECURITY_ROLE.to_owned()),
    )
    .depends_on(&implementation);
    tasks.push(qa.clone());
    tasks.push(security.clone());

    if signals.needs_operations {
        tasks.push(
            Task::new(
                format!("DEVOPS: Provide a reproducible build, staged deployment, health checks and rollback plan for: {goal}. Never deploy without explicit human approval."),
                AgentRole::Custom(DEVOPS_ROLE.to_owned()),
            )
            .depends_on(&qa)
            .depends_on(&security),
        );
    }

    tasks.push(
        Task::new(
            format!("INTEGRATE: Merge the validated QA and Security branches for: {goal} in a fresh worktree and publish one unified reviewable diff."),
            AgentRole::Integrator,
        )
        .depends_on(&qa)
        .depends_on(&security),
    );

    DynamicPlan {
        signals,
        decomposition,
        tasks,
    }
}

/// Executable four-role DAG. Each child has one parent so it can inherit
/// the validated parent commit without implicitly merging unrelated branches.
pub fn engineering_execution_plan(goal: &str) -> Vec<Task> {
    let architect = Task::new(
        format!(
            "ARCHITECT: Implement a narrow, testable architecture/interface slice for: {goal}. Produce working code and regression tests, not only advice."
        ),
        AgentRole::Custom("software-architect".into()),
    );
    let backend = Task::new(
        format!("BACKEND: Implement backend behavior and regression tests for: {goal}. Extend the validated Architect branch."),
        AgentRole::Custom("backend-engineer".into()),
    )
    .depends_on(&architect);
    let qa = Task::new(
        format!("QA: Implement executable regression tests for: {goal}. Work on the validated Backend snapshot."),
        AgentRole::QaEngineer,
    )
    .depends_on(&backend);
    let security = Task::new(
        format!("SECURITY: Implement security hardening and adversarial regression tests for: {goal}. Work on the validated Backend snapshot."),
        AgentRole::Custom("security-engineer".into()),
    )
    .depends_on(&backend);
    let integrator = Task::new(
        format!("INTEGRATE: Merge the validated QA and Security branches for: {goal} in a fresh worktree. Resolve semantic conflicts without dropping either change, then run the full validation suite and publish one unified reviewable diff."),
        AgentRole::Integrator,
    )
    .depends_on(&qa)
    .depends_on(&security);
    vec![architect, backend, qa, security, integrator]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Orchestrator;
    use std::collections::HashSet;

    #[test]
    fn standard_team_has_distinct_roles_and_valid_task_plan() {
        let team = standard_team();
        assert_eq!(team.len(), 9);
        let roles: HashSet<_> = team.into_iter().map(|member| member.role).collect();
        assert_eq!(roles.len(), 9);
        let tasks = application_plan("create a ticketing app");
        assert_eq!(tasks.len(), 8);
        assert!(tasks.iter().all(|t| roles.contains(&t.assigned_to)));
        Orchestrator::validate_plan(&tasks).unwrap();
    }

    #[test]
    fn execution_plan_is_an_ordered_dag_with_parallel_reviewers() {
        let tasks = engineering_execution_plan("add an endpoint");
        Orchestrator::validate_plan(&tasks).unwrap();
        assert_eq!(tasks.len(), 5);
        assert!(tasks[0].dependencies.is_empty());
        assert_eq!(tasks[1].dependencies, vec![tasks[0].id]);
        assert_eq!(tasks[2].dependencies, vec![tasks[1].id]);
        assert_eq!(tasks[3].dependencies, vec![tasks[1].id]);
        assert_eq!(tasks[4].assigned_to, AgentRole::Integrator);
        assert_eq!(tasks[4].dependencies, vec![tasks[2].id, tasks[3].id]);
    }

    #[test]
    fn dynamic_plan_activates_design_only_for_interface_goals() {
        let interface = dynamic_plan("build an accessible settings page with a responsive layout");
        assert!(interface.signals.needs_design);
        assert!(!interface.signals.needs_operations);
        let roles = interface.activated_roles();
        assert!(roles.contains(&AgentRole::UiUxDesigner));
        assert!(
            !roles
                .iter()
                .any(|role| *role == AgentRole::Custom(DEVOPS_ROLE.into()))
        );
        // Design and QA must request browser evidence, not just unit tests.
        let design = interface
            .tasks
            .iter()
            .find(|task| task.assigned_to == AgentRole::UiUxDesigner)
            .unwrap();
        assert_eq!(
            design
                .metadata
                .get(VERIFICATION_METADATA)
                .map(String::as_str),
            Some("headless-browser,screenshot")
        );
        let qa = interface
            .tasks
            .iter()
            .find(|task| task.assigned_to == AgentRole::QaEngineer)
            .unwrap();
        assert!(
            qa.metadata
                .get(VERIFICATION_METADATA)
                .is_some_and(|value| value.contains("screenshot"))
        );
        Orchestrator::validate_plan(&interface.all_tasks()).unwrap();
    }

    #[test]
    fn dynamic_plan_activates_devops_only_for_delivery_goals() {
        let delivery = dynamic_plan("add a container deployment pipeline with monitoring");
        assert!(delivery.signals.needs_operations);
        assert!(!delivery.signals.needs_design);
        let roles = delivery.activated_roles();
        assert!(
            roles
                .iter()
                .any(|role| *role == AgentRole::Custom(DEVOPS_ROLE.into()))
        );
        assert!(!roles.contains(&AgentRole::UiUxDesigner));
        Orchestrator::validate_plan(&delivery.all_tasks()).unwrap();

        let plain = dynamic_plan("fix an off-by-one error in the parser");
        assert!(!plain.signals.needs_operations);
        assert!(!plain.signals.needs_design);
        assert!(
            !plain
                .activated_roles()
                .iter()
                .any(|role| *role == AgentRole::Custom(DEVOPS_ROLE.into()))
        );
    }

    #[test]
    fn dynamic_plan_always_starts_with_the_project_manager() {
        let plan = dynamic_plan("ship a small internal tool");
        let all = plan.all_tasks();
        assert_eq!(all[0].id, plan.decomposition.id);
        assert_eq!(all[0].assigned_to, AgentRole::ProjectManager);
        assert!(all[0].dependencies.is_empty());
        for task in plan.tasks.iter() {
            assert!(!task.dependencies.is_empty(), "orphaned task: {task:?}");
        }
        // Security and QA remain mandatory; the integrator needs both branches.
        let roles = plan.activated_roles();
        assert!(roles.contains(&AgentRole::QaEngineer));
        assert!(roles.contains(&AgentRole::Custom(SECURITY_ROLE.into())));
        let integrator = plan
            .tasks
            .iter()
            .find(|task| task.assigned_to == AgentRole::Integrator)
            .unwrap();
        assert_eq!(integrator.dependencies.len(), 2);
        Orchestrator::validate_plan(&plan.all_tasks()).unwrap();
    }

    #[test]
    fn execution_tasks_re_root_the_plan_without_the_project_manager() {
        let plan = dynamic_plan("build a responsive dashboard with charts");
        let tasks = plan.execution_tasks();
        Orchestrator::validate_plan(&tasks).unwrap();
        assert!(
            !tasks
                .iter()
                .any(|task| task.assigned_to == AgentRole::ProjectManager)
        );
        assert!(
            !tasks
                .iter()
                .any(|task| task.dependencies.contains(&plan.decomposition.id))
        );
        // Every remaining task still has the structure the plan described.
        assert_eq!(tasks.len(), plan.tasks.len());
        for (original, executed) in plan.tasks.iter().zip(tasks.iter()) {
            assert_eq!(original.id, executed.id);
            assert_eq!(original.assigned_to, executed.assigned_to);
            assert!(original.dependencies.len() >= executed.dependencies.len());
        }
    }

    #[test]
    fn operations_cannot_start_before_security_and_qa() {
        let tasks = application_plan("ship responsibly");
        let operations = tasks.last().unwrap();
        assert_eq!(operations.dependencies.len(), 2);
        for dependency in &operations.dependencies {
            let role = &tasks
                .iter()
                .find(|task| task.id == *dependency)
                .unwrap()
                .assigned_to;
            assert!(matches!(role, AgentRole::QaEngineer | AgentRole::Custom(_)));
        }
    }
}
