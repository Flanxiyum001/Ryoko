use std::collections::{BTreeMap, HashMap};
use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// The role an agent plays in a project. Custom roles keep the model open for
/// Luau-defined agents.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentRole {
    ProjectManager,
    SeniorDeveloper,
    UiUxDesigner,
    QaEngineer,
    Custom(String),
}

impl AgentRole {
    pub fn label(&self) -> &str {
        match self {
            Self::ProjectManager => "project-manager",
            Self::SeniorDeveloper => "senior-developer",
            Self::UiUxDesigner => "ui-ux-designer",
            Self::QaEngineer => "qa-engineer",
            Self::Custom(name) => name,
        }
    }
}

impl fmt::Display for AgentRole {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.label())
    }
}

impl FromStr for AgentRole {
    type Err = ();

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        let role = match value {
            "project-manager" | "project_manager" => Self::ProjectManager,
            "senior-developer" | "senior_developer" | "developer" => Self::SeniorDeveloper,
            "ui-ux-designer" | "ui_ux_designer" | "designer" => Self::UiUxDesigner,
            "qa-engineer" | "qa_engineer" | "qa" => Self::QaEngineer,
            "" => return Err(()),
            custom => Self::Custom(custom.to_owned()),
        };
        Ok(role)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentIdentity {
    pub role: AgentRole,
    pub name: String,
    pub description: String,
    /// Preferred `provider/model` pair, if the agent talks to a model.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(default)]
    pub capabilities: Vec<String>,
}

impl AgentIdentity {
    pub fn new(role: AgentRole, name: impl Into<String>, description: impl Into<String>) -> Self {
        Self {
            role,
            name: name.into(),
            description: description.into(),
            model: None,
            capabilities: Vec::new(),
        }
    }

    pub fn with_model(mut self, model: impl Into<String>) -> Self {
        self.model = Some(model.into());
        self
    }

    pub fn with_capability(mut self, capability: impl Into<String>) -> Self {
        self.capabilities.push(capability.into());
        self
    }
}

/// Project-wide state handed to an agent when a task starts. `shared_memory` is
/// a point-in-time snapshot; live writes go through
/// [`crate::SharedMemory`](crate::memory::SharedMemory) on the task context.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ProjectContext {
    pub project_root: String,
    pub active_branch: String,
    pub shared_memory: HashMap<String, String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Task {
    pub id: Uuid,
    pub description: String,
    pub assigned_to: AgentRole,
    pub dependencies: Vec<Uuid>,
    #[serde(default)]
    pub metadata: BTreeMap<String, String>,
}

impl Task {
    pub fn new(description: impl Into<String>, assigned_to: AgentRole) -> Self {
        Self {
            id: Uuid::new_v4(),
            description: description.into(),
            assigned_to,
            dependencies: Vec::new(),
            metadata: BTreeMap::new(),
        }
    }

    pub fn with_id(mut self, id: Uuid) -> Self {
        self.id = id;
        self
    }

    pub fn with_dependency(mut self, dependency: Uuid) -> Self {
        self.dependencies.push(dependency);
        self
    }

    pub fn depends_on(mut self, dependency: &Task) -> Self {
        self.dependencies.push(dependency.id);
        self
    }

    pub fn with_metadata(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.metadata.insert(key.into(), value.into());
        self
    }
}

/// A file change proposed by an agent. Changes stay inside the task's isolated
/// workspace until validation passes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProposedChange {
    /// Path relative to the project root. Absolute paths and `..` are rejected
    /// by workspace implementations.
    pub path: String,
    pub content: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub language: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

impl ProposedChange {
    pub fn new(path: impl Into<String>, content: impl Into<String>) -> Self {
        Self {
            path: path.into(),
            content: content.into(),
            language: None,
            description: None,
        }
    }

    pub fn with_description(mut self, description: impl Into<String>) -> Self {
        self.description = Some(description.into());
        self
    }

    pub fn with_language(mut self, language: impl Into<String>) -> Self {
        self.language = Some(language.into());
        self
    }
}

/// Result returned by [`AgentRuntime::execute_task`](crate::AgentRuntime::execute_task).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskResult {
    pub task_id: Uuid,
    pub success: bool,
    pub output: String,
    /// Paths of the changes carried in `changes`.
    pub changes_proposed: Vec<String>,
    #[serde(default)]
    pub changes: Vec<ProposedChange>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

impl TaskResult {
    pub fn success(task_id: Uuid, output: impl Into<String>) -> Self {
        Self {
            task_id,
            success: true,
            output: output.into(),
            changes_proposed: Vec::new(),
            changes: Vec::new(),
            error: None,
        }
    }

    pub fn with_changes(mut self, changes: Vec<ProposedChange>) -> Self {
        self.changes_proposed = changes.iter().map(|c| c.path.clone()).collect();
        self.changes = changes;
        self
    }

    pub fn failure(task_id: Uuid, error: impl Into<String>) -> Self {
        Self {
            task_id,
            success: false,
            output: String::new(),
            changes_proposed: Vec::new(),
            changes: Vec::new(),
            error: Some(error.into()),
        }
    }

    pub fn failed_with_output(
        task_id: Uuid,
        output: impl Into<String>,
        error: impl Into<String>,
    ) -> Self {
        Self {
            task_id,
            success: false,
            output: output.into(),
            changes_proposed: Vec::new(),
            changes: Vec::new(),
            error: Some(error.into()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn role_round_trips_through_strings() {
        for role in [
            AgentRole::ProjectManager,
            AgentRole::SeniorDeveloper,
            AgentRole::UiUxDesigner,
            AgentRole::QaEngineer,
            AgentRole::Custom("security-reviewer".to_owned()),
        ] {
            let parsed: AgentRole = role.label().parse().unwrap();
            assert_eq!(parsed, role);
        }
        assert!("".parse::<AgentRole>().is_err());
    }

    #[test]
    fn task_result_tracks_proposed_paths() {
        let task = Task::new("ship it", AgentRole::SeniorDeveloper);
        let result = TaskResult::success(task.id, "done").with_changes(vec![
            ProposedChange::new("src/main.rs", "fn main() {}"),
            ProposedChange::new("README.md", "# Ryoko"),
        ]);
        assert_eq!(result.changes_proposed, vec!["src/main.rs", "README.md"]);
        assert!(result.success);
    }
}
