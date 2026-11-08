//! Built-in agents and the structured proposal contract.
//!
//! Model-backed agents never return prose that another component has to parse
//! heuristically. They return one JSON object containing complete file bodies;
//! the orchestrator validates the paths and writes them only into the task's
//! isolated worktree.

use async_trait::async_trait;
use ryoko_models::{ChatRequest, ModelError};
use serde::{Deserialize, Serialize};

use crate::agent::{AgentRuntime, TaskContext};
use crate::error::CoreResult;
use crate::model::{AgentIdentity, ProposedChange, Task, TaskResult};
use crate::workspace::validate_relative_path;

/// Metadata key used by the orchestrator to carry validation or agent errors
/// into the next attempt.
pub const RETRY_FEEDBACK_METADATA: &str = "ryoko.feedback";

/// Metadata key carrying a short project/context excerpt for the model.
pub const CONTEXT_METADATA: &str = "ryoko.context";

/// The exact response shape a code-generation agent must return.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StructuredProposal {
    #[serde(default)]
    pub summary: String,
    #[serde(default)]
    pub files: Vec<ProposedChange>,
}

impl StructuredProposal {
    pub fn new(summary: impl Into<String>, files: Vec<ProposedChange>) -> Self {
        Self {
            summary: summary.into(),
            files,
        }
    }
}

/// System prompt shared by every model-backed code-generation agent.
pub const CODEGEN_SYSTEM_PROMPT: &str = r#"You are a senior software engineer working inside an isolated Git worktree.

Return exactly one JSON object. Do not use Markdown fences, prose before the
object, or prose after it.

The object has this shape:
{
  "summary": "one sentence describing the change",
  "files": [
    {
      "path": "relative/path/to/file",
      "content": "complete file content",
      "language": "rust",
      "description": "what changed in this file"
    }
  ]
}

Rules:
- Paths must be relative to the project root. Never return absolute paths or `..`.
- Return every file you create or modify, with its complete content, not a diff.
- Include at least one test file or an inline test block. Source-only proposals
  are rejected before they reach human review.
- Keep the change targeted at the goal. Do not invent unrelated files.
- If you were given feedback from a failed attempt, fix that specific failure.
"#;

/// Agent that asks the configured model for a [`StructuredProposal`] and turns
/// it into a [`TaskResult`]. The orchestrator applies the files only after the
/// result is accepted, and only inside the task's workspace lease.
#[derive(Debug, Clone)]
pub struct ModelAgent {
    identity: AgentIdentity,
}

impl ModelAgent {
    pub fn new(identity: AgentIdentity) -> Self {
        Self { identity }
    }

    pub fn with_model(mut self, model: impl Into<String>) -> Self {
        self.identity.model = Some(model.into());
        self
    }

    fn request(&self, task: &Task, project_root: &str, active_branch: &str) -> ChatRequest {
        let model = self
            .identity
            .model
            .clone()
            .unwrap_or_else(|| "default".to_owned());
        let mut prompt = format!(
            "Goal:\n{}\n\nProject root: {project_root}\nBase branch: {active_branch}\n",
            task.description
        );

        if let Some(context) = task.metadata.get(CONTEXT_METADATA) {
            if !context.trim().is_empty() {
                prompt.push_str("\nProject context:\n");
                prompt.push_str(&truncate(context, 12_000));
                prompt.push('\n');
            }
        }

        if let Some(feedback) = task.metadata.get(RETRY_FEEDBACK_METADATA) {
            if !feedback.trim().is_empty() {
                prompt.push_str(
                    "\nThe previous attempt was rejected. Fix the cause and return the full \
                     corrected file set.\nFeedback:\n",
                );
                prompt.push_str(&truncate(feedback, 12_000));
                prompt.push('\n');
            }
        }

        ChatRequest::new(model)
            .with_system(CODEGEN_SYSTEM_PROMPT)
            .with_user(prompt)
    }
}

#[async_trait]
impl AgentRuntime for ModelAgent {
    fn identity(&self) -> AgentIdentity {
        self.identity.clone()
    }

    async fn execute_task(&self, ctx: TaskContext, task: Task) -> CoreResult<TaskResult> {
        if self
            .identity
            .model
            .as_deref()
            .unwrap_or("")
            .trim()
            .is_empty()
        {
            return Ok(TaskResult::failure(
                task.id,
                "agent has no model configured; set RYOKO_MODEL or --model",
            ));
        }

        let request = self.request(&task, &ctx.project.project_root, &ctx.project.active_branch);
        let completion = match ctx.models.complete(request).await {
            Ok(completion) => completion,
            Err(error) => {
                return Ok(TaskResult::failure(task.id, format_model_error(&error)));
            }
        };

        let proposal = match parse_structured_proposal(&completion.text) {
            Ok(proposal) => proposal,
            Err(error) => {
                return Ok(TaskResult::failure(
                    task.id,
                    format!("invalid structured proposal: {error}"),
                ));
            }
        };

        let summary = if proposal.summary.trim().is_empty() {
            "proposed code changes".to_owned()
        } else {
            proposal.summary
        };
        Ok(TaskResult::success(task.id, summary).with_changes(proposal.files))
    }
}

fn format_model_error(error: &ModelError) -> String {
    format!("model request failed: {error}")
}

/// Parses a model response into a proposal. Accepts a bare JSON object as well
/// as a fenced ```json block, because some providers add fences despite explicit
/// instructions.
pub fn parse_structured_proposal(text: &str) -> Result<StructuredProposal, String> {
    let candidate =
        extract_json(text).ok_or_else(|| "response contains no JSON object".to_owned())?;
    let proposal: StructuredProposal =
        serde_json::from_str(candidate).map_err(|error| error.to_string())?;
    if proposal.files.is_empty() {
        return Err("files must contain at least one file".to_owned());
    }
    for file in &proposal.files {
        validate_relative_path(&file.path).map_err(|error| error.to_string())?;
    }
    Ok(proposal)
}

fn extract_json(text: &str) -> Option<&str> {
    let trimmed = text.trim();
    if let Some(start) = trimmed.find("```") {
        let body = &trimmed[start + 3..];
        let body = body.strip_prefix("json").unwrap_or(body);
        let body = body.trim_start_matches(['\r', '\n']);
        if let Some(end) = body.find("```") {
            let fenced = body[..end].trim();
            if fenced.starts_with('{') && fenced.ends_with('}') {
                return Some(fenced);
            }
        }
    }

    let start = trimmed.find('{')?;
    let end = trimmed.rfind('}')?;
    (start < end).then_some(trimmed[start..=end].trim())
}

fn truncate(text: &str, max_bytes: usize) -> String {
    if text.len() <= max_bytes {
        return text.to_owned();
    }
    let mut end = max_bytes;
    while end > 0 && !text.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}...", &text[..end])
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::AllowAllValidator;
    use crate::agent::TaskContext;
    use crate::event::EventSink;
    use crate::memory::SharedMemory;
    use crate::model::{AgentRole, ProjectContext};
    use ryoko_models::{MockProvider, ScriptedTurn};
    use std::sync::Arc;

    fn proposal_json() -> String {
        serde_json::json!({
            "summary": "add health endpoint",
            "files": [
                {
                    "path": "src/health.rs",
                    "content": "pub fn health() -> bool { true }",
                    "language": "rust"
                },
                {
                    "path": "tests/health.rs",
                    "content": "#[test]\nfn healthy() { assert!(true); }"
                }
            ]
        })
        .to_string()
    }

    #[test]
    fn parses_bare_and_fenced_proposals() {
        let bare = parse_structured_proposal(&proposal_json()).unwrap();
        assert_eq!(bare.files.len(), 2);

        let fenced = format!("Here is the result:\n```json\n{}\n```\n", proposal_json());
        let parsed = parse_structured_proposal(&fenced).unwrap();
        assert_eq!(parsed.summary, "add health endpoint");
    }

    #[test]
    fn rejects_empty_files_and_unsafe_paths() {
        let empty = r#"{"summary":"nothing","files":[]}"#;
        assert!(parse_structured_proposal(empty).is_err());

        let unsafe_path = r#"{"summary":"bad","files":[{"path":"../escape.rs","content":""}]}"#;
        assert!(parse_structured_proposal(unsafe_path).is_err());
    }

    #[tokio::test]
    async fn returns_structured_changes_and_sends_feedback_to_the_model() {
        let mock = Arc::new(MockProvider::new("mock"));
        mock.push_turn(ScriptedTurn::text(proposal_json()));
        let mut router = ryoko_models::ModelRouter::new("mock");
        router.register(mock.clone());

        let agent = ModelAgent::new(AgentIdentity::new(
            AgentRole::SeniorDeveloper,
            "dev-01",
            "implementer",
        ))
        .with_model("mock/coder");
        let context = TaskContext {
            project: ProjectContext {
                project_root: "/tmp/project".to_owned(),
                active_branch: "main".to_owned(),
                shared_memory: Default::default(),
            },
            memory: SharedMemory::new(),
            models: Arc::new(router),
            validator: Arc::new(AllowAllValidator),
            workspace: None,
            events: EventSink::default(),
        };
        let task = Task::new("add health", AgentRole::SeniorDeveloper).with_metadata(
            RETRY_FEEDBACK_METADATA,
            "error[E0425]: cannot find function `health`",
        );

        let result = agent.execute_task(context, task).await.unwrap();
        assert!(result.success);
        assert_eq!(result.changes.len(), 2);
        assert_eq!(result.changes[1].path, "tests/health.rs");

        let requests = mock.requests();
        assert_eq!(requests.len(), 1);
        let user = requests[0]
            .messages
            .iter()
            .map(|message| message.text_content())
            .collect::<Vec<_>>()
            .join("\n");
        assert!(user.contains("error[E0425]"));
    }
}
