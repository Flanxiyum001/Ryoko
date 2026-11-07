//! Opinionated, extensible team profiles for an application-building IDE.
//! These are role contracts, not grants of filesystem, shell or network access.
use crate::model::{AgentRole, Task};

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
    ).depends_on(&pm);
    let ux = Task::new(
        format!("Accessible screens and user flows for: {goal}"),
        AgentRole::UiUxDesigner,
    ).depends_on(&architect);
    let backend = Task::new(
        format!("Backend APIs, storage and auth for: {goal}"),
        AgentRole::Custom("backend-engineer".into()),
    ).depends_on(&architect);
    let implementation = Task::new(
        format!("Integrate frontend and backend for: {goal}"),
        AgentRole::SeniorDeveloper,
    ).depends_on(&ux).depends_on(&backend);
    let security = Task::new(
        format!("Threat model and security review for: {goal}"),
        AgentRole::Custom("security-engineer".into()),
    ).depends_on(&implementation);
    let qa = Task::new(
        format!("Acceptance and regression verification for: {goal}"),
        AgentRole::QaEngineer,
    ).depends_on(&implementation);
    let operations = Task::new(
        format!("Staging, monitoring and rollback plan for: {goal}"),
        AgentRole::Custom("devops-engineer".into()),
    ).depends_on(&security).depends_on(&qa);
    vec![pm, architect, ux, backend, implementation, security, qa, operations]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Orchestrator;
    use std::collections::HashSet;

    #[test]
    fn standard_team_has_distinct_roles_and_valid_task_plan() {
        let team = standard_team();
        assert_eq!(team.len(), 8);
        let roles: HashSet<_> = team.into_iter().map(|member| member.role).collect();
        assert_eq!(roles.len(), 8);
        let tasks = application_plan("create a ticketing app");
        assert_eq!(tasks.len(), 8);
        assert!(tasks.iter().all(|t| roles.contains(&t.assigned_to)));
        Orchestrator::validate_plan(&tasks).unwrap();
    }

    #[test]
    fn operations_cannot_start_before_security_and_qa() {
        let tasks = application_plan("ship responsibly");
        let operations = tasks.last().unwrap();
        assert_eq!(operations.dependencies.len(), 2);
        for dependency in &operations.dependencies {
            let role = &tasks.iter().find(|task| task.id == *dependency).unwrap().assigned_to;
            assert!(matches!(role, AgentRole::QaEngineer | AgentRole::Custom(_)));
        }
    }
}
