//! Versioned plugin ABI payloads.

use ryoko_core::model::Task;
use ryoko_core::workspace::validate_relative_path;
use ryoko_core::{ProjectContext, ProposedChange};
use serde::{Deserialize, Serialize};

use crate::manifest::PluginError;

/// The only ABI schema this host accepts.
pub const PLUGIN_SCHEMA: u32 = 1;
pub const MAX_REQUEST_BYTES: usize = 256 * 1024;
pub const MAX_SOURCE_BYTES: usize = 512 * 1024;
pub const MAX_AGENT_FILES: usize = 64;
pub const MAX_AGENT_CONTENT_BYTES: usize = 1024 * 1024;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PluginRequest {
    pub schema: u32,
    pub operation: PluginOperation,
}

impl PluginRequest {
    fn new(operation: PluginOperation) -> Self {
        Self {
            schema: PLUGIN_SCHEMA,
            operation,
        }
    }

    pub fn agent(task: Task, project: ProjectContext, role: impl Into<String>) -> Self {
        Self::new(PluginOperation::Agent {
            task,
            project,
            role: role.into(),
        })
    }

    pub fn lint(path: impl Into<String>, source: impl Into<String>) -> Self {
        Self::new(PluginOperation::Lint {
            path: path.into(),
            source: source.into(),
        })
    }

    pub fn context(path: impl Into<String>, source: impl Into<String>) -> Self {
        Self::new(PluginOperation::Context {
            path: path.into(),
            source: source.into(),
        })
    }

    pub fn validate(&self) -> Result<(), PluginError> {
        if self.schema != PLUGIN_SCHEMA {
            return Err(PluginError::Protocol(format!(
                "unsupported request schema {}",
                self.schema
            )));
        }
        match &self.operation {
            PluginOperation::Agent {
                task,
                project,
                role,
            } => {
                if task.description.len() > 32 * 1024 {
                    return Err(PluginError::Protocol(
                        "task description exceeds 32 KiB".into(),
                    ));
                }
                if task.metadata.len() > 64 {
                    return Err(PluginError::Protocol("too much task metadata".into()));
                }
                if role.len() > 64 || role.chars().any(char::is_control) {
                    return Err(PluginError::Protocol("invalid agent role".into()));
                }
                if project.project_root.len() > 4096
                    || project.active_branch.len() > 256
                    || !project.shared_memory.is_empty()
                {
                    return Err(PluginError::Protocol(
                        "agent project context is not bounded".into(),
                    ));
                }
            }
            PluginOperation::Lint { path, source } | PluginOperation::Context { path, source } => {
                validate_relative_path(path)
                    .map_err(|error| PluginError::Protocol(error.to_string()))?;
                if source.len() > MAX_SOURCE_BYTES {
                    return Err(PluginError::Protocol(
                        "plugin source input exceeds 512 KiB".into(),
                    ));
                }
            }
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum PluginOperation {
    Agent {
        task: Task,
        project: ProjectContext,
        role: String,
    },
    Lint {
        path: String,
        source: String,
    },
    Context {
        path: String,
        source: String,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PluginResponse {
    pub schema: u32,
    pub result: PluginOutput,
}

impl PluginResponse {
    pub fn validate(&self) -> Result<(), PluginError> {
        if self.schema != PLUGIN_SCHEMA {
            return Err(PluginError::Protocol(format!(
                "unsupported response schema {}",
                self.schema
            )));
        }
        match &self.result {
            PluginOutput::Agent { summary, files } => {
                if summary.len() > 8 * 1024 {
                    return Err(PluginError::Protocol("agent summary too large".into()));
                }
                if files.is_empty() || files.len() > MAX_AGENT_FILES {
                    return Err(PluginError::Protocol(
                        "agent must return between 1 and 64 files".into(),
                    ));
                }
                let mut total = 0usize;
                let mut seen = std::collections::BTreeSet::new();
                for file in files {
                    validate_relative_path(&file.path)
                        .map_err(|error| PluginError::Protocol(error.to_string()))?;
                    if !seen.insert(file.path.as_str()) {
                        return Err(PluginError::Protocol(format!(
                            "duplicate agent file path: {}",
                            file.path
                        )));
                    }
                    total = total.saturating_add(file.content.len());
                }
                if total > MAX_AGENT_CONTENT_BYTES {
                    return Err(PluginError::Protocol(
                        "agent file content exceeds 1 MiB".into(),
                    ));
                }
            }
            PluginOutput::Lint { diagnostics } => {
                if diagnostics.len() > 256 {
                    return Err(PluginError::Protocol(
                        "plugin returned too many diagnostics".into(),
                    ));
                }
                if diagnostics.iter().any(|entry| entry.message.len() > 4096) {
                    return Err(PluginError::Protocol(
                        "lint diagnostic exceeds 4 KiB".into(),
                    ));
                }
            }
            PluginOutput::Context { outline, symbols } => {
                if outline.len() > 64 * 1024 {
                    return Err(PluginError::Protocol(
                        "context outline exceeds 64 KiB".into(),
                    ));
                }
                if symbols.len() > 512 || symbols.iter().any(|symbol| symbol.len() > 512) {
                    return Err(PluginError::Protocol(
                        "context symbol limit exceeded".into(),
                    ));
                }
            }
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum PluginOutput {
    Agent {
        #[serde(default)]
        summary: String,
        files: Vec<ProposedChange>,
    },
    Lint {
        #[serde(default)]
        diagnostics: Vec<LintDiagnostic>,
    },
    Context {
        #[serde(default)]
        outline: String,
        #[serde(default)]
        symbols: Vec<String>,
    },
}

pub type PluginResult = PluginOutput;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LintDiagnostic {
    /// One-based line. Zero is rejected by consumers that require a position.
    pub line: usize,
    pub column: usize,
    pub severity: LintSeverity,
    pub message: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LintSeverity {
    Error,
    Warning,
    Info,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PluginContext {
    pub outline: String,
    pub symbols: Vec<String>,
}

/// Bounded agent-facing request metadata. Plugin requests never contain host
/// credentials, environment variables or model provider keys.
pub type PluginTask = Task;
