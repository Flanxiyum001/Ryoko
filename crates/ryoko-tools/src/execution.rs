//! Tool execution context and executor.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use ryoko_core::model::Task;
use ryoko_core::validation::ChangeValidator;
use ryoko_core::workspace::WorkspaceLease;
use ryoko_models::ModelRouter;
use ryoko_vfs::DocumentStore;
use tokio::sync::Semaphore;

use crate::approval::{ApprovalGate, ApprovalScope, SessionApprovalGate};
use crate::capabilities::{CapabilitySet, PermissionScope};
use crate::error::{ToolError, ToolResult};
use crate::limits::{ToolBudget, ToolLimits};

/// Context available to tools during execution.
#[derive(Clone)]
pub struct ToolContext {
    /// The current task being executed.
    pub task: Task,
    /// The isolated workspace lease for this task.
    pub workspace: Option<Arc<dyn WorkspaceLease>>,
    /// The model router for LLM calls.
    pub models: Arc<ModelRouter>,
    /// The change validator for pre-flight checks.
    pub validator: Arc<dyn ChangeValidator>,
    /// The virtual document store.
    pub vfs: Arc<DocumentStore>,
    /// The shared memory blackboard.
    pub memory: Arc<ryoko_core::SharedMemory>,
    /// The approval gate for sensitive operations.
    pub approval_gate: Option<Arc<dyn ApprovalGate>>,
    /// The capability set granted to this task.
    pub capabilities: CapabilitySet,
    /// The tool budget for this task.
    pub budget: ToolBudget,
    /// The execution limits.
    pub limits: ToolLimits,
    /// Event sink for progress reporting.
    pub events: ryoko_core::EventSink,
    /// Optional MCP tool service.
    pub mcp: Option<Arc<dyn ryoko_core::mcp::McpToolService>>,
}

impl ToolContext {
    /// Create a new tool context.
    pub fn new(
        task: Task,
        workspace: Option<Arc<dyn WorkspaceLease>>,
        models: Arc<ModelRouter>,
        validator: Arc<dyn ChangeValidator>,
        memory: Arc<ryoko_core::SharedMemory>,
        capabilities: CapabilitySet,
    ) -> Self {
        Self {
            task,
            workspace,
            models,
            validator,
            vfs: Arc::new(DocumentStore::new()),
            memory,
            approval_gate: None,
            capabilities,
            budget: ToolBudget::default(),
            limits: ToolLimits::default(),
            events: ryoko_core::EventSink::default(),
            mcp: None,
        }
    }

    /// Set the approval gate.
    pub fn with_approval_gate(mut self, gate: Option<Arc<dyn ApprovalGate>>) -> Self {
        self.approval_gate = gate;
        self
    }

    /// Set the tool budget.
    pub fn with_budget(mut self, budget: ToolBudget) -> Self {
        self.budget = budget;
        self
    }

    /// Set the execution limits.
    pub fn with_limits(mut self, limits: ToolLimits) -> Self {
        self.limits = limits;
        self
    }

    /// Set the event sink.
    pub fn with_events(mut self, events: ryoko_core::EventSink) -> Self {
        self.events = events;
        self
    }

    /// Set the MCP tool service.
    pub fn with_mcp(mut self, mcp: Option<Arc<dyn ryoko_core::mcp::McpToolService>>) -> Self {
        self.mcp = mcp;
        self
    }

    /// Check if the context has a capability.
    pub fn has_capability(&self, capability: &crate::capabilities::Capability) -> bool {
        self.capabilities.contains(capability)
    }

    /// Check if the context has all required capabilities.
    pub fn has_all_capabilities(&self, required: &CapabilitySet) -> bool {
        self.capabilities.contains_all(required)
    }

    /// Get the workspace root path.
    pub fn workspace_root(&self) -> Option<std::path::PathBuf> {
        self.workspace.as_ref().map(|w| w.root().to_path_buf())
    }

    /// Request approval for a tool call.
    pub async fn request_approval(
        &self,
        tool: &str,
        arguments: &serde_json::Value,
    ) -> ApprovalScope {
        if let Some(gate) = &self.approval_gate {
            gate.approve(tool, arguments).await
        } else {
            ApprovalScope::Deny
        }
    }

    /// Emit a progress event.
    pub fn emit_progress(&self, message: impl Into<String>) {
        self.events.emit(ryoko_core::event::OrchestratorEvent::TaskProgress {
            task_id: self.task.id,
            message: message.into(),
        });
    }
}

/// Output from a tool execution.
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct ToolOutput {
    /// Text output from the tool.
    pub text: String,
    /// Structured data output (JSON).
    pub data: serde_json::Value,
    /// Files created or modified.
    pub files: Vec<FileOutput>,
    /// Whether the output was truncated due to size limits.
    pub truncated: bool,
    /// Execution time in milliseconds.
    pub duration_ms: u64,
}

impl ToolOutput {
    /// Create an empty output.
    pub fn empty() -> Self {
        Self::default()
    }

    /// Create a text-only output.
    pub fn text(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            ..Default::default()
        }
    }

    /// Create a data-only output.
    pub fn data(data: serde_json::Value) -> Self {
        Self {
            data,
            ..Default::default()
        }
    }

    /// Create a file output.
    pub fn files(files: Vec<FileOutput>) -> Self {
        Self {
            files,
            ..Default::default()
        }
    }

    /// Combine with another output.
    pub fn merge(mut self, other: Self) -> Self {
        self.text.push_str(&other.text);
        if self.data.is_null() {
            self.data = other.data;
        } else if !other.data.is_null() {
            // Merge JSON objects
            if let (serde_json::Value::Object(a), serde_json::Value::Object(b)) = (&mut self.data, other.data) {
                a.extend(b);
            }
        }
        self.files.extend(other.files);
        self.truncated = self.truncated || other.truncated;
        self.duration_ms = self.duration_ms.saturating_add(other.duration_ms);
        self
    }
}

/// File output from a tool.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct FileOutput {
    pub path: String,
    pub content: String,
    pub operation: FileOperation,
}

/// File operation type.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FileOperation {
    Create,
    Read,
    Update,
    Delete,
}

/// Tool executor that manages tool execution with limits and budgets.
pub struct ToolExecutor {
    registry: crate::ToolRegistry,
    global_budget: ToolBudget,
    global_limits: ToolLimits,
    running_tasks: Arc<Mutex<HashMap<uuid::Uuid, TaskExecutionState>>>,
    semaphore: Arc<Semaphore>,
}

impl ToolExecutor {
    /// Create a new tool executor.
    pub fn new(
        registry: crate::ToolRegistry,
        global_budget: ToolBudget,
        global_limits: ToolLimits,
        max_concurrency: usize,
    ) -> Self {
        Self {
            registry,
            global_budget,
            global_limits,
            running_tasks: Arc::new(Mutex::new(HashMap::new())),
            semaphore: Arc::new(Semaphore::new(max_concurrency.max(1))),
        }
    }

    /// Execute a tool with the given context and input.
    pub async fn execute(
        &self,
        context: &ToolContext,
        tool_name: &str,
        input: serde_json::Value,
    ) -> ToolResult<ToolOutput> {
        // Check if tool exists
        let tool = self.registry.get(tool_name)
            .ok_or_else(|| ToolError::ToolNotFound(tool_name.to_owned()))?;

        // Check capabilities
        if !context.has_all_capabilities(&tool.required_capabilities()) {
            let missing = tool.required_capabilities().capabilities.iter()
                .filter(|c| !context.capabilities.contains(c))
                .next()
                .cloned()
                .unwrap_or(crate::capabilities::Capability::Read(crate::capabilities::ReadScope::Workspace));
            return Err(ToolError::PermissionDenied {
                tool: tool_name.to_owned(),
                capability: missing,
            });
        }

        // Check if mutating and requires approval
        if tool.is_mutating() {
            let scope = context.request_approval(tool_name, &input).await;
            if scope == ApprovalScope::Deny {
                return Err(ToolError::ApprovalDenied(format!("Operator denied {}", tool_name)));
            }
        }

        // Check budget
        self.global_budget.check_and_reserve(tool_name, 1).await?;
        context.budget.check_and_reserve(tool_name, 1).await?;

        // Acquire semaphore for concurrency control
        let _permit = self.semaphore.acquire_owned().await
            .map_err(|_| ToolError::Internal("semaphore closed".into()))?;

        // Track running task
        let task_id = context.task.id;
        {
            let mut running = self.running_tasks.lock().await;
            running.insert(task_id, TaskExecutionState {
                tool: tool_name.to_owned(),
                started_at: std::time::Instant::now(),
            });
        }

        // Execute with timeout
        let timeout = context.limits.timeout.max(self.global_limits.timeout);
        let execution_future = tool.execute(context, input);
        let result = match tokio::time::timeout(timeout, execution_future).await {
            Ok(result) => result,
            Err(_) => {
                return Err(ToolError::Timeout {
                    tool: tool_name.to_owned(),
                    seconds: timeout.as_secs(),
                });
            }
        };

        // Remove from running tasks
        self.running_tasks.lock().await.remove(&task_id);

        // Check output size
        if let Ok(output) = &result {
            let output_size = serde_json::to_vec(output).map(|v| v.len()).unwrap_or(0);
            if output_size > context.limits.max_output_bytes {
                return Err(ToolError::OutputLimitExceeded {
                    tool: tool_name.to_owned(),
                    bytes: output_size,
                    limit: context.limits.max_output_bytes,
                });
            }
        }

        result
    }

    /// Cancel a running tool execution.
    pub async fn cancel(&self, task_id: uuid::Uuid) {
        let mut running = self.running_tasks.lock().await;
        running.remove(&task_id);
    }

    /// Get the registry.
    pub fn registry(&self) -> &crate::ToolRegistry {
        &self.registry
    }
}

/// State of a running task execution.
struct TaskExecutionState {
    tool: String,
    started_at: std::time::Instant,
}

#[cfg(test)]
mod tests {
    use super::*;
    use ryoko_core::model::{AgentRole, Task};
    use ryoko_models::ModelRouter;

    #[tokio::test]
    async fn tool_executor_budget_check() {
        let registry = crate::ToolRegistry::new();
        let budget = ToolBudget::new(1); // Only 1 call allowed
        let executor = ToolExecutor::new(registry, budget, ToolLimits::default(), 1);

        let task = Task::new("test", AgentRole::SeniorDeveloper);
        let context = ToolContext::new(
            task,
            None,
            Arc::new(ModelRouter::new("mock")),
            Arc::new(ryoko_core::AllowAllValidator),
            Arc::new(ryoko_core::SharedMemory::new()),
            crate::capabilities::CapabilitySet::all(),
        );

        // First call should fail because no tools are registered
        let result = executor.execute(&context, "nonexistent", serde_json::json!({})).await;
        assert!(matches!(result, Err(ToolError::ToolNotFound(_))));
    }
}