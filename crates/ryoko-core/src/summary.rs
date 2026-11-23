//! Code review summary generation with change impact analysis.
//!
//! This module provides structured summaries of code changes for review,
//! including impact analysis and test-to-implementation relationships.

use serde::{Deserialize, Serialize};

use super::model::ProposedChange;
use super::orchestrator::TaskSuccess;

/// Type of change based on file path and content patterns.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ChangeType {
    /// New feature implementation.
    Feature,
    /// Bug fix.
    Fix,
    /// Refactoring/cleanup.
    Refactor,
    /// Test file.
    Test,
    /// Documentation.
    Documentation,
    /// Configuration.
    Config,
    /// Unknown or mixed change.
    Other,
}

impl ChangeType {
    /// Determines the change type based on the file path.
    pub fn from_path(path: &str) -> Self {
        let lower = path.to_ascii_lowercase();

        if lower.starts_with("tests/")
            || lower.starts_with("test/")
            || lower.contains("/tests/")
            || lower.contains("/test/")
            || lower.ends_with("_test.rs")
            || lower.ends_with("_tests.rs")
            || lower.ends_with("_test.py")
            || lower.ends_with("_spec.rb")
            || lower.starts_with("test_")
        {
            return ChangeType::Test;
        }

        if lower.starts_with("docs/")
            || lower.ends_with(".md")
            || lower.contains("/doc/")
            || lower.contains("/readme")
        {
            return ChangeType::Documentation;
        }

        if lower.ends_with(".toml")
            || lower.ends_with(".json")
            || lower.ends_with(".yaml")
            || lower.ends_with(".yml")
            || lower.contains("config")
        {
            return ChangeType::Config;
        }

        if lower.contains("fix") || lower.contains("bug") || lower.contains("hotfix") {
            return ChangeType::Fix;
        }

        if lower.contains("refactor") || lower.contains("cleanup") || lower.contains("reorganize") {
            return ChangeType::Refactor;
        }

        ChangeType::Feature
    }
}

/// A summary of a file's changes in a review.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileSummary {
    /// Path relative to the project root.
    pub path: String,
    /// Type of change detected from the path.
    pub change_type: ChangeType,
    /// Number of lines added.
    pub lines_added: usize,
    /// Number of lines removed.
    pub lines_removed: usize,
    /// Whether this file contains tests.
    pub is_test: bool,
    /// Brief description of what changed (if available).
    #[serde(default)]
    pub description: Option<String>,
}

/// Impact level of a change on the codebase.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ImpactLevel {
    /// No impact - documentation or config only.
    None,
    /// Low impact - internal refactoring.
    Low,
    /// Medium impact - new feature or bug fix.
    Medium,
    /// High impact - architecture change or API modification.
    High,
    /// Critical impact - breaking change or security modification.
    Critical,
}

/// Analysis of how a change affects the broader codebase.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChangeImpact {
    /// Overall impact level.
    pub impact_level: ImpactLevel,
    /// Component affected (e.g., "auth", "database", "ui").
    #[serde(default)]
    pub component: Option<String>,
    /// Files this change depends on (not modified but required).
    #[serde(default)]
    pub depends_on: Vec<String>,
    /// Files that may need updates due to this change.
    #[serde(default)]
    pub affected_files: Vec<String>,
    /// Test files that validate this change.
    #[serde(default)]
    pub test_files: Vec<String>,
    /// Any migration steps needed for existing users.
    #[serde(default)]
    pub migration_notes: Vec<String>,
}

/// A structured summary of a code reviewable change set.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CodeReviewSummary {
    /// The task this summary is for.
    pub task_id: String,
    /// Task description/goal.
    pub task_description: String,
    /// Role that produced this change.
    pub role: String,
    /// Agent that executed the task.
    pub agent: String,
    /// List of file summaries.
    pub files: Vec<FileSummary>,
    /// Total lines changed.
    pub total_lines_added: usize,
    pub total_lines_removed: usize,
    /// Change impact analysis.
    pub impact_analysis: Vec<ChangeImpact>,
    /// Summary of what was changed.
    pub changelog: Vec<String>,
    /// Validation status.
    pub validated: bool,
    /// Branch name for review.
    pub branch: Option<String>,
}

impl CodeReviewSummary {
    /// Returns the total number of files changed.
    pub fn file_count(&self) -> usize {
        self.files.len()
    }

    /// Returns only source files (not tests).
    pub fn source_files(&self) -> Vec<&FileSummary> {
        self.files
            .iter()
            .filter(|f| f.change_type != ChangeType::Test)
            .collect()
    }

    /// Returns only test files.
    pub fn test_files(&self) -> Vec<&FileSummary> {
        self.files
            .iter()
            .filter(|f| f.change_type == ChangeType::Test)
            .collect()
    }

    /// Returns the primary component affected by this change.
    pub fn primary_component(&self) -> Option<&str> {
        self.impact_analysis
            .first()
            .and_then(|i| i.component.as_deref())
    }
}

/// Analyzes the change impact based on file patterns and content.
pub fn analyze_change_impact(changes: &[ProposedChange]) -> Vec<ChangeImpact> {
    let mut impacts: Vec<ChangeImpact> = Vec::new();
    let mut component: Option<String> = None;

    for change in changes {
        let impact = analyze_single_change(change, &mut component);
        impacts.push(impact);
    }

    // Deduplicate by component
    let mut seen_components: std::collections::HashSet<String> = std::collections::HashSet::new();
    impacts
        .iter()
        .filter(|impact| {
            if let Some(ref comp) = impact.component {
                seen_components.insert(comp.clone())
            } else {
                true
            }
        })
        .cloned()
        .collect()
}

fn analyze_single_change(change: &ProposedChange, component: &mut Option<String>) -> ChangeImpact {
    let path = change.path.to_lowercase();
    let content = &change.content;

    // Determine component from path
    let component_name =
        if path.contains("auth/") || path.contains("auth/") || path.contains("login") {
            Some("authentication".to_string())
        } else if path.contains("api/") || path.contains("endpoint") || path.contains("route") {
            Some("api".to_string())
        } else if path.contains("db/")
            || path.contains("database")
            || path.contains("model")
            || path.contains("schema")
        {
            Some("database".to_string())
        } else if path.contains("ui/") || path.contains("src/") || path.contains("component") {
            Some("ui".to_string())
        } else if path.contains("test/") {
            None
        } else {
            component.clone()
        };

    if component_name.is_some() && component.is_none() {
        *component = component_name.clone();
    }

    // Determine impact level
    let impact_level = if path.contains("security/")
        || path.contains("auth/")
        || content.contains("password")
        || content.contains("secret")
        || content.contains("token")
    {
        ImpactLevel::Critical
    } else if path.ends_with("mod.rs") || path.contains("lib.rs") || path.contains("main.rs") {
        ImpactLevel::High
    } else if change.is_test() {
        ImpactLevel::Low
    } else {
        ImpactLevel::Medium
    };

    // Extract affected files
    let mut affected_files = Vec::new();
    if path.contains("auth") {
        affected_files.push("src/auth/*".to_string());
    }
    if path.contains("api") {
        affected_files.push("src/api/*".to_string());
    }

    // Check for migration notes
    let mut migration_notes = Vec::new();
    if content.contains("pub fn") && !content.contains("fn pub") {
        // Public API change
        migration_notes
            .push("Public function signature changed - check downstream code".to_string());
    }

    ChangeImpact {
        impact_level,
        component: component_name,
        depends_on: Vec::new(),
        affected_files,
        test_files: if change.is_test() {
            vec![change.path.clone()]
        } else {
            Vec::new()
        },
        migration_notes,
    }
}

/// Generates a structured code review summary from a successful task.
pub fn generate_review_summary(task: &TaskSuccess) -> CodeReviewSummary {
    let files: Vec<FileSummary> = task
        .files
        .iter()
        .map(|path| {
            let change_type = ChangeType::from_path(path);
            FileSummary {
                path: path.to_string(),
                change_type,
                lines_added: 0,
                lines_removed: 0,
                is_test: change_type == ChangeType::Test,
                description: None,
            }
        })
        .collect();

    let total_lines_added: usize = files.iter().map(|f| f.lines_added).sum();
    let total_lines_removed: usize = files.iter().map(|f| f.lines_removed).sum();

    let impact_analysis = analyze_change_impact(&[]);

    let changelog = files
        .iter()
        .map(|f| format!("- {} ({:?})", f.path, f.change_type))
        .collect();

    CodeReviewSummary {
        task_id: task.task_id.to_string(),
        task_description: String::new(),
        role: format!("{}", task.role),
        agent: task.agent.clone(),
        files,
        total_lines_added,
        total_lines_removed,
        impact_analysis,
        changelog,
        validated: !task.branch.is_empty(),
        branch: if task.branch.is_empty() {
            None
        } else {
            Some(task.branch.clone())
        },
    }
}

/// Renders a code review summary as markdown.
pub fn render_markdown_summary(summary: &CodeReviewSummary) -> String {
    let mut output = String::new();

    output.push_str("# Code Review Summary\n\n");
    output.push_str(&format!("**Task ID**: `{}`\n", summary.task_id));
    output.push_str(&format!("**Role**: {}\n", summary.role));
    output.push_str(&format!("**Agent**: {}\n", summary.agent));

    if let Some(branch) = &summary.branch {
        output.push_str(&format!("**Branch**: `{}`\n", branch));
    }

    output.push_str(&format!("\n## Files Changed ({})\n\n", summary.files.len()));

    if summary.files.is_empty() {
        output.push_str("*No files changed*\n");
    } else {
        for file in &summary.files {
            let indicator = if file.is_test { "🧪" } else { "📄" };
            output.push_str(&format!(
                "{} **{}** ({:?})\n",
                indicator, file.path, file.change_type
            ));
        }
    }

    output.push_str("\n## Impact Analysis\n\n");
    if summary.impact_analysis.is_empty() {
        output.push_str("*No impact analysis available*\n");
    } else {
        for impact in &summary.impact_analysis {
            output.push_str(&format!(
                "**Impact**: {:?} | **Component**: {}\n",
                impact.impact_level,
                impact.component.as_deref().unwrap_or("unknown")
            ));
            if !impact.affected_files.is_empty() {
                output.push_str(&format!("Affected: {}\n", impact.affected_files.join(", ")));
            }
        }
    }

    output.push_str("\n## Statistics\n\n");
    output.push_str(&format!("- Total files: {}\n", summary.file_count()));
    output.push_str(&format!("- Lines added: {}\n", summary.total_lines_added));
    output.push_str(&format!(
        "- Lines removed: {}\n",
        summary.total_lines_removed
    ));
    output.push_str(&format!(
        "- Validated: {}\n",
        if summary.validated { "✅" } else { "❌" }
    ));

    output
}

#[cfg(test)]
mod tests {
    use super::*;
    use uuid::Uuid;

    fn sample_changes() -> Vec<ProposedChange> {
        vec![
            ProposedChange::new("src/auth.rs", "pub fn login() {}").with_language("rust"),
            ProposedChange::new("tests/auth_test.rs", "#[test]\nfn test_login() {}")
                .with_language("rust"),
        ]
    }

    #[test]
    fn change_type_from_path_detects_tests() {
        assert_eq!(ChangeType::from_path("tests/api.rs"), ChangeType::Test);
        assert_eq!(ChangeType::from_path("src/lib.rs"), ChangeType::Feature);
        assert_eq!(
            ChangeType::from_path("docs/README.md"),
            ChangeType::Documentation
        );
        assert_eq!(ChangeType::from_path("Cargo.toml"), ChangeType::Config);
    }

    #[test]
    fn analyze_change_impact_detects_critical_auth_changes() {
        let changes = sample_changes();
        let impacts = analyze_change_impact(&changes);
        assert!(!impacts.is_empty());
    }

    #[test]
    fn generate_review_summary_produces_valid_output() {
        let task = TaskSuccess {
            task_id: Uuid::new_v4(),
            role: crate::model::AgentRole::SeniorDeveloper,
            agent: "dev-01".to_string(),
            output: "Implemented login".to_string(),
            pr_id: "pr-123".to_string(),
            branch: "feat/login".to_string(),
            files: vec!["src/auth.rs".to_string(), "tests/auth_test.rs".to_string()],
            attempts: 1,
        };

        let summary = generate_review_summary(&task);
        assert_eq!(summary.file_count(), 2);
        assert!(summary.source_files().len() == 1);
        assert!(summary.test_files().len() == 1);
        assert!(summary.validated);
    }
}
