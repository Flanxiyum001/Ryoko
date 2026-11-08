use std::path::PathBuf;
use std::time::Duration;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::error::CoreResult;
use crate::model::ProposedChange;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StagedChanges {
    pub task_id: Uuid,
    pub files: Vec<ProposedChange>,
    /// Root of the isolated worktree that contains these changes. Validators
    /// use it to run project-specific build and test commands without ever
    /// touching the main checkout.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub root: Option<PathBuf>,
}

impl StagedChanges {
    pub fn new(task_id: Uuid, files: Vec<ProposedChange>) -> Self {
        Self {
            task_id,
            files,
            root: None,
        }
    }

    pub fn with_root(mut self, root: impl Into<PathBuf>) -> Self {
        self.root = Some(root.into());
        self
    }

    pub fn is_empty(&self) -> bool {
        self.files.is_empty()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ValidationReport {
    pub passed: bool,
    pub summary: String,
    #[serde(default)]
    pub logs: Vec<String>,
    #[serde(default)]
    pub duration_ms: u64,
}

impl ValidationReport {
    pub fn passed(summary: impl Into<String>, logs: Vec<String>, duration: Duration) -> Self {
        Self {
            passed: true,
            summary: summary.into(),
            logs,
            duration_ms: duration.as_millis() as u64,
        }
    }

    pub fn failed(summary: impl Into<String>, logs: Vec<String>, duration: Duration) -> Self {
        Self {
            passed: false,
            summary: summary.into(),
            logs,
            duration_ms: duration.as_millis() as u64,
        }
    }
}

/// Executes a task's test suite against staged changes. Implementations must
/// run untrusted code in an isolated environment: [`ryoko-sandbox`] provides the
/// WASM-backed implementation used by the desktop app.
#[async_trait]
pub trait ChangeValidator: Send + Sync {
    async fn validate(&self, changes: &StagedChanges) -> CoreResult<ValidationReport>;
}

/// Development / dry-run validator. It accepts everything, so it must never be
/// the default in a production build path.
#[derive(Debug, Default, Clone, Copy)]
pub struct AllowAllValidator;

#[async_trait]
impl ChangeValidator for AllowAllValidator {
    async fn validate(&self, changes: &StagedChanges) -> CoreResult<ValidationReport> {
        Ok(ValidationReport {
            passed: true,
            summary: format!(
                "guardrails disabled: accepted {} file(s) without execution",
                changes.files.len()
            ),
            logs: vec!["AllowAllValidator is active; changes were not executed".to_owned()],
            duration_ms: 0,
        })
    }
}
