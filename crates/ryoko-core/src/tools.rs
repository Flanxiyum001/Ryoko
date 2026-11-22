//! Tool execution engine traits for Ryoko core.
//!
//! This module defines the core traits and types for the secure tool execution engine.
//! Implementations are provided by `ryoko-tools`.

use crate::model::Task;
use crate::validation::ChangeValidator;
use crate::workspace::WorkspaceLease;
use ryoko_models::ModelRouter;
use ryoko_vfs::DocumentStore;
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use std::sync::Arc;
use std::time::Duration;

/// Result of a tool execution, designed for consistent consumption by model adapters.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolExecutionResult {
    pub tool: String,
    pub success: bool,
    pub output: ToolOutput,
    pub error: Option<String>,
    pub metadata: serde_json::Value,
}

impl ToolExecutionResult {
    pub fn success(tool: String, output: ToolOutput) -> Self {
        Self {
            tool,
            success: true,
            output,
            error: None,
            metadata: serde_json::Value::Null,
        }
    }

    pub fn failure(tool: String, error: String) -> Self {
        Self {
            tool,
            success: false,
            output: ToolOutput::empty(),
            error: Some(error),
            metadata: serde_json::Value::Null,
        }
    }

    pub fn with_metadata(mut self, metadata: serde_json::Value) -> Self {
        self.metadata = metadata;
        self
    }
}

/// Output from a tool execution.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ToolOutput {
    pub text: String,
    pub data: serde_json::Value,
    pub files: Vec<FileOutput>,
    pub truncated: bool,
    pub duration_ms: u64,
}

impl ToolOutput {
    pub fn empty() -> Self {
        Self::default()
    }

    pub fn text(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            ..Default::default()
        }
    }

    pub fn data(data: serde_json::Value) -> Self {
        Self {
            data,
            ..Default::default()
        }
    }

    pub fn files(files: Vec<FileOutput>) -> Self {
        Self {
            files,
            ..Default::default()
        }
    }

    pub fn merge(mut self, other: Self) -> Self {
        self.text.push_str(&other.text);
        if self.data.is_null() {
            self.data = other.data;
        } else if !other.data.is_null() {
            if let (serde_json::Value::Object(a), serde_json::Value::Object(b)) =
                (&mut self.data, other.data)
            {
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
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileOutput {
    pub path: String,
    pub content: String,
    pub operation: FileOperation,
}

/// File operation type.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FileOperation {
    Create,
    Read,
    Update,
    Delete,
}

/// A single capability that a tool may require.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Capability {
    Read(ReadScope),
    Write(WriteScope),
    Execute,
    Network(NetworkScope),
    Mcp,
    GitRead,
    GitWrite,
    TestExecute,
}

/// Scope for read operations.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReadScope {
    TaskFiles,
    Workspace,
    System,
}

/// Scope for write operations.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WriteScope {
    CreateOnly,
    Modify,
    Delete,
    Full,
}

/// Scope for network operations.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NetworkScope {
    Loopback,
    Internal,
    External,
}

/// A set of capabilities with efficient membership testing.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CapabilitySet {
    capabilities: BTreeSet<Capability>,
}

impl CapabilitySet {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn from(capabilities: impl IntoIterator<Item = Capability>) -> Self {
        Self {
            capabilities: capabilities.into_iter().collect(),
        }
    }

    pub fn all() -> Self {
        Self {
            capabilities: [
                Capability::Read(ReadScope::Workspace),
                Capability::Write(WriteScope::Full),
                Capability::Execute,
                Capability::Network(NetworkScope::External),
                Capability::Mcp,
                Capability::GitRead,
                Capability::GitWrite,
                Capability::TestExecute,
            ]
            .into_iter()
            .collect(),
        }
    }

    pub fn read_only() -> Self {
        Self {
            capabilities: [Capability::Read(ReadScope::TaskFiles), Capability::GitRead]
                .into_iter()
                .collect(),
        }
    }

    pub fn code_generation() -> Self {
        Self {
            capabilities: [
                Capability::Read(ReadScope::Workspace),
                Capability::Write(WriteScope::Modify),
                Capability::Execute,
                Capability::GitRead,
                Capability::TestExecute,
            ]
            .into_iter()
            .collect(),
        }
    }

    pub fn contains(&self, capability: &Capability) -> bool {
        self.capabilities.contains(capability)
    }

    pub fn contains_all(&self, required: &CapabilitySet) -> bool {
        required.capabilities.is_subset(&self.capabilities)
    }

    pub fn insert(&mut self, capability: Capability) {
        self.capabilities.insert(capability);
    }

    pub fn remove(&mut self, capability: &Capability) -> bool {
        self.capabilities.remove(capability)
    }

    pub fn iter(&self) -> impl Iterator<Item = &Capability> {
        self.capabilities.iter()
    }

    pub fn len(&self) -> usize {
        self.capabilities.len()
    }

    pub fn is_empty(&self) -> bool {
        self.capabilities.is_empty()
    }
}

impl From<Vec<Capability>> for CapabilitySet {
    fn from(capabilities: Vec<Capability>) -> Self {
        Self::from(capabilities)
    }
}

/// Permission scope combines capabilities with the approval requirement.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PermissionScope {
    pub capabilities: CapabilitySet,
    pub requires_approval: bool,
}

impl PermissionScope {
    pub fn new(capabilities: CapabilitySet, requires_approval: bool) -> Self {
        Self {
            capabilities,
            requires_approval,
        }
    }

    pub fn with_approval(capabilities: CapabilitySet) -> Self {
        Self::new(capabilities, true)
    }

    pub fn pre_approved(capabilities: CapabilitySet) -> Self {
        Self::new(capabilities, false)
    }
}

/// How long an approval grant applies.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ApprovalScope {
    Once,
    ForRun,
    Deny,
}

/// Decision made by an approval gate.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolApproval {
    pub tool: String,
    pub scope: ApprovalScope,
    pub allowed: bool,
    pub reused_grant: bool,
}

/// Trait for an approval gate that asks a human operator for permission.
#[async_trait::async_trait]
pub trait ApprovalGate: Send + Sync {
    async fn approve(&self, tool: &str, arguments: &serde_json::Value) -> ApprovalScope;
}

/// Session-based approval gate that remembers grants for the run.
pub struct SessionApprovalGate {
    inner: Arc<dyn ApprovalGate>,
    granted: tokio::sync::Mutex<BTreeSet<String>>,
    denied: tokio::sync::Mutex<BTreeSet<String>>,
    decisions: tokio::sync::Mutex<Vec<ToolApproval>>,
}

impl SessionApprovalGate {
    pub fn new(inner: Arc<dyn ApprovalGate>) -> Self {
        Self {
            inner,
            granted: tokio::sync::Mutex::new(BTreeSet::new()),
            denied: tokio::sync::Mutex::new(BTreeSet::new()),
            decisions: tokio::sync::Mutex::new(Vec::new()),
        }
    }

    pub async fn grants(&self) -> Vec<String> {
        self.granted.lock().await.iter().cloned().collect()
    }

    pub async fn decisions(&self) -> Vec<ToolApproval> {
        self.decisions.lock().await.clone()
    }

    pub async fn revoke_grants(&self) {
        self.granted.lock().await.clear();
    }
}

#[async_trait::async_trait]
impl ApprovalGate for SessionApprovalGate {
    async fn approve(&self, tool: &str, arguments: &serde_json::Value) -> ApprovalScope {
        if tool.trim().is_empty() {
            let decision = ToolApproval {
                tool: String::new(),
                scope: ApprovalScope::Deny,
                allowed: false,
                reused_grant: false,
            };
            self.decisions.lock().await.push(decision);
            return ApprovalScope::Deny;
        }

        if self.denied.lock().await.contains(tool) {
            let decision = ToolApproval {
                tool: tool.to_owned(),
                scope: ApprovalScope::Deny,
                allowed: false,
                reused_grant: true,
            };
            self.decisions.lock().await.push(decision);
            return ApprovalScope::Deny;
        }

        if self.granted.lock().await.contains(tool) {
            let decision = ToolApproval {
                tool: tool.to_owned(),
                scope: ApprovalScope::ForRun,
                allowed: true,
                reused_grant: true,
            };
            self.decisions.lock().await.push(decision);
            return ApprovalScope::ForRun;
        }

        let scope = self.inner.approve(tool, arguments).await;
        let allowed = scope != ApprovalScope::Deny;

        if allowed && scope == ApprovalScope::ForRun {
            self.granted.lock().await.insert(tool.to_owned());
        }
        if !allowed {
            self.denied.lock().await.insert(tool.to_owned());
        }

        let decision = ToolApproval {
            tool: tool.to_owned(),
            scope,
            allowed,
            reused_grant: false,
        };
        self.decisions.lock().await.push(decision);

        scope
    }
}

/// Terminal-based approval asker for CLI.
pub struct TerminalApprovalAsker {
    interactive: bool,
    pre_approved: BTreeSet<String>,
}

impl TerminalApprovalAsker {
    pub fn new() -> Self {
        Self {
            interactive: true,
            pre_approved: BTreeSet::new(),
        }
    }

    pub fn non_interactive_with(pre_approved: impl IntoIterator<Item = String>) -> Self {
        Self {
            interactive: false,
            pre_approved: pre_approved.into_iter().collect(),
        }
    }
}

fn is_non_interactive() -> bool {
    std::env::var("RYOKO_NONINTERACTIVE")
        .map(|v| matches!(v.trim(), "1" | "true" | "yes"))
        .unwrap_or(false)
}

fn scope_from_answer(answer: &str) -> ApprovalScope {
    match answer.trim().to_ascii_lowercase().as_str() {
        "o" | "once" | "y" | "yes" => ApprovalScope::Once,
        "r" | "run" => ApprovalScope::ForRun,
        _ => ApprovalScope::Deny,
    }
}

#[async_trait::async_trait]
impl ApprovalGate for TerminalApprovalAsker {
    async fn approve(&self, tool: &str, arguments: &serde_json::Value) -> ApprovalScope {
        if !self.interactive || is_non_interactive() {
            if self.pre_approved.contains(tool) {
                return ApprovalScope::ForRun;
            }
            return ApprovalScope::Deny;
        }

        eprintln!("Tool approval required: {}", tool);
        eprintln!(
            "Arguments: {}",
            serde_json::to_string_pretty(arguments).unwrap_or_default()
        );
        eprint!("Allow [o]nce, for this [r]un, or [d]eny (default)? ");

        use std::io::{self, Write};
        io::stdout().flush().ok();

        let mut answer = String::new();
        let result = tokio::time::timeout(
            std::time::Duration::from_secs(90),
            tokio::io::AsyncBufReadExt::read_line(
                &mut tokio::io::BufReader::new(tokio::io::stdin()),
                &mut answer,
            ),
        )
        .await;

        match result {
            Ok(Ok(_)) => scope_from_answer(&answer),
            _ => ApprovalScope::Deny,
        }
    }
}

/// Limits for tool execution.
#[derive(Debug, Clone)]
pub struct ToolLimits {
    pub timeout: Duration,
    pub max_output_bytes: usize,
    pub max_calls_per_task: u64,
    pub max_concurrency: usize,
    pub allow_network: bool,
    pub allow_writes: bool,
    pub allow_exec: bool,
}

impl Default for ToolLimits {
    fn default() -> Self {
        Self {
            timeout: Duration::from_secs(30),
            max_output_bytes: 1024 * 1024,
            max_calls_per_task: 100,
            max_concurrency: 4,
            allow_network: false,
            allow_writes: true,
            allow_exec: false,
        }
    }
}

impl ToolLimits {
    pub fn read_only() -> Self {
        Self {
            allow_writes: false,
            allow_exec: false,
            ..Default::default()
        }
    }

    pub fn code_generation() -> Self {
        Self {
            timeout: Duration::from_secs(120),
            max_output_bytes: 4 * 1024 * 1024,
            max_calls_per_task: 50,
            allow_exec: true,
            ..Default::default()
        }
    }

    pub fn test_execution() -> Self {
        Self {
            timeout: Duration::from_secs(300),
            max_output_bytes: 16 * 1024 * 1024,
            max_calls_per_task: 10,
            allow_exec: true,
            ..Default::default()
        }
    }
}

/// Budget for tool calls, tracked per task and globally.
#[derive(Debug)]
pub struct ToolBudget {
    total_calls: std::sync::atomic::AtomicU64,
    calls_made: std::sync::atomic::AtomicU64,
    per_tool_limits: Arc<tokio::sync::RwLock<std::collections::HashMap<String, u64>>>,
    per_tool_calls: Arc<tokio::sync::RwLock<std::collections::HashMap<String, u64>>>,
}

impl Clone for ToolBudget {
    fn clone(&self) -> Self {
        Self {
            total_calls: std::sync::atomic::AtomicU64::new(
                self.total_calls.load(std::sync::atomic::Ordering::Relaxed),
            ),
            calls_made: std::sync::atomic::AtomicU64::new(
                self.calls_made.load(std::sync::atomic::Ordering::Relaxed),
            ),
            per_tool_limits: Arc::clone(&self.per_tool_limits),
            per_tool_calls: Arc::clone(&self.per_tool_calls),
        }
    }
}

impl Default for ToolBudget {
    fn default() -> Self {
        Self::unlimited()
    }
}

impl ToolBudget {
    pub fn unlimited() -> Self {
        Self {
            total_calls: std::sync::atomic::AtomicU64::new(u64::MAX),
            calls_made: std::sync::atomic::AtomicU64::new(0),
            per_tool_limits: Arc::new(tokio::sync::RwLock::new(std::collections::HashMap::new())),
            per_tool_calls: Arc::new(tokio::sync::RwLock::new(std::collections::HashMap::new())),
        }
    }

    pub fn new(total_calls: u64) -> Self {
        Self {
            total_calls: std::sync::atomic::AtomicU64::new(total_calls),
            calls_made: std::sync::atomic::AtomicU64::new(0),
            per_tool_limits: Arc::new(tokio::sync::RwLock::new(std::collections::HashMap::new())),
            per_tool_calls: Arc::new(tokio::sync::RwLock::new(std::collections::HashMap::new())),
        }
    }

    pub async fn set_tool_limit(&self, tool: &str, limit: u64) {
        self.per_tool_limits
            .write()
            .await
            .insert(tool.to_owned(), limit);
    }

    pub async fn check_and_reserve(&self, tool: &str, calls: u64) -> crate::CoreResult<()> {
        use std::sync::atomic::Ordering;

        let current = self.calls_made.fetch_add(calls, Ordering::SeqCst);
        if current + calls > self.total_calls.load(Ordering::SeqCst) {
            self.calls_made.fetch_sub(calls, Ordering::SeqCst);
            return Err(crate::error::CoreError::ToolBudgetExceeded(format!(
                "total tool call budget exhausted ({}/{})",
                current + calls,
                self.total_calls.load(Ordering::SeqCst)
            )));
        }

        let mut per_tool = self.per_tool_calls.write().await;
        let tool_current = per_tool.entry(tool.to_owned()).or_insert(0);
        if let Some(limit) = self.per_tool_limits.read().await.get(tool) {
            if *tool_current + calls > *limit {
                *tool_current -= calls;
                return Err(crate::error::CoreError::ToolBudgetExceeded(format!(
                    "tool '{}' call budget exhausted ({}/{})",
                    tool,
                    *tool_current + calls,
                    limit
                )));
            }
        }
        *tool_current += calls;

        Ok(())
    }

    pub fn remaining_total(&self) -> u64 {
        use std::sync::atomic::Ordering;
        self.total_calls
            .load(Ordering::Relaxed)
            .saturating_sub(self.calls_made.load(Ordering::Relaxed))
    }

    pub async fn remaining_for_tool(&self, tool: &str) -> Option<u64> {
        let limits = self.per_tool_limits.read().await;
        let calls = self.per_tool_calls.read().await;
        limits
            .get(tool)
            .map(|limit| limit.saturating_sub(*calls.get(tool).unwrap_or(&0)))
    }

    pub async fn calls_for_tool(&self, tool: &str) -> u64 {
        self.per_tool_calls
            .read()
            .await
            .get(tool)
            .copied()
            .unwrap_or(0)
    }
}

/// Output size limits configuration.
#[derive(Debug, Clone)]
pub struct OutputLimits {
    pub max_text_bytes: usize,
    pub max_data_bytes: usize,
    pub max_files: usize,
    pub max_file_bytes: usize,
}

impl Default for OutputLimits {
    fn default() -> Self {
        Self {
            max_text_bytes: 64 * 1024,
            max_data_bytes: 256 * 1024,
            max_files: 100,
            max_file_bytes: 4 * 1024 * 1024,
        }
    }
}

/// Context available to tools during execution.
#[derive(Clone)]
pub struct ToolContext {
    pub task: Task,
    pub workspace: Option<Arc<dyn WorkspaceLease>>,
    pub models: Arc<ModelRouter>,
    pub validator: Arc<dyn ChangeValidator>,
    pub vfs: Arc<DocumentStore>,
    pub memory: Arc<crate::SharedMemory>,
    pub approval_gate: Option<Arc<dyn ApprovalGate>>,
    pub capabilities: CapabilitySet,
    pub budget: ToolBudget,
    pub limits: ToolLimits,
    pub events: crate::EventSink,
    pub mcp: Option<Arc<dyn crate::mcp::McpToolService>>,
}

impl ToolContext {
    pub fn new(
        task: Task,
        workspace: Option<Arc<dyn WorkspaceLease>>,
        models: Arc<ModelRouter>,
        validator: Arc<dyn ChangeValidator>,
        memory: Arc<crate::SharedMemory>,
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
            events: crate::EventSink::default(),
            mcp: None,
        }
    }

    pub fn with_approval_gate(mut self, gate: Option<Arc<dyn ApprovalGate>>) -> Self {
        self.approval_gate = gate;
        self
    }

    pub fn with_budget(mut self, budget: ToolBudget) -> Self {
        self.budget = budget;
        self
    }

    pub fn with_limits(mut self, limits: ToolLimits) -> Self {
        self.limits = limits;
        self
    }

    pub fn with_events(mut self, events: crate::EventSink) -> Self {
        self.events = events;
        self
    }

    pub fn with_mcp(mut self, mcp: Option<Arc<dyn crate::mcp::McpToolService>>) -> Self {
        self.mcp = mcp;
        self
    }

    pub fn has_capability(&self, capability: &Capability) -> bool {
        self.capabilities.contains(capability)
    }

    pub fn has_all_capabilities(&self, required: &CapabilitySet) -> bool {
        self.capabilities.contains_all(required)
    }

    pub fn workspace_root(&self) -> Option<std::path::PathBuf> {
        self.workspace.as_ref().map(|w| w.root().to_path_buf())
    }

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

    pub fn emit_progress(&self, message: impl Into<String>) {
        self.events
            .emit(crate::event::OrchestratorEvent::TaskProgress {
                task_id: self.task.id,
                message: message.into(),
            });
    }
}

/// Trait for a tool that can be executed within a task's workspace.
#[async_trait::async_trait]
pub trait Tool: Send + Sync {
    fn name(&self) -> &str;
    fn description(&self) -> &str;
    fn input_schema(&self) -> serde_json::Value;
    fn required_capabilities(&self) -> CapabilitySet;
    fn is_mutating(&self) -> bool;

    async fn execute(
        &self,
        context: &ToolContext,
        input: serde_json::Value,
    ) -> crate::CoreResult<ToolOutput>;
}

/// Collection of all available tools for a task.
#[derive(Default)]
pub struct ToolRegistry {
    tools: Vec<Box<dyn Tool>>,
}

impl ToolRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn register(&mut self, tool: Box<dyn Tool>) {
        self.tools.push(tool);
    }

    pub fn get(&self, name: &str) -> Option<&dyn Tool> {
        self.tools
            .iter()
            .find(|t| t.name() == name)
            .map(|t| t.as_ref())
    }

    pub fn list(&self) -> Vec<&dyn Tool> {
        self.tools.iter().map(|t| t.as_ref()).collect()
    }
}

/// Tool executor that manages tool execution with limits and budgets.
pub struct ToolExecutor {
    registry: ToolRegistry,
    global_budget: ToolBudget,
    global_limits: ToolLimits,
    running_tasks:
        Arc<tokio::sync::Mutex<std::collections::HashMap<uuid::Uuid, TaskExecutionState>>>,
    semaphore: Arc<tokio::sync::Semaphore>,
}

impl ToolExecutor {
    pub fn new(
        registry: ToolRegistry,
        global_budget: ToolBudget,
        global_limits: ToolLimits,
        max_concurrency: usize,
    ) -> Self {
        Self {
            registry,
            global_budget,
            global_limits,
            running_tasks: Arc::new(tokio::sync::Mutex::new(std::collections::HashMap::new())),
            semaphore: Arc::new(tokio::sync::Semaphore::new(max_concurrency.max(1))),
        }
    }

    pub async fn execute(
        &self,
        context: &ToolContext,
        tool_name: &str,
        input: serde_json::Value,
    ) -> crate::CoreResult<ToolOutput> {
        let tool = self
            .registry
            .get(tool_name)
            .ok_or_else(|| crate::error::CoreError::ToolNotFound(tool_name.to_owned()))?;

        if !context.has_all_capabilities(&tool.required_capabilities()) {
            let missing = tool
                .required_capabilities()
                .capabilities
                .iter()
                .filter(|c| !context.capabilities.contains(c))
                .next()
                .cloned()
                .unwrap_or(Capability::Read(ReadScope::Workspace));
            return Err(crate::error::CoreError::ToolPermissionDenied {
                tool: tool_name.to_owned(),
                capability: format!("{:?}", missing),
            });
        }

        if tool.is_mutating() {
            let scope = context.request_approval(tool_name, &input).await;
            if scope == ApprovalScope::Deny {
                return Err(crate::error::CoreError::ToolApprovalDenied(format!(
                    "Operator denied {}",
                    tool_name
                )));
            }
        }

        self.global_budget.check_and_reserve(tool_name, 1).await?;
        context.budget.check_and_reserve(tool_name, 1).await?;

        let _permit = self
            .semaphore
            .clone()
            .acquire_owned()
            .await
            .map_err(|_| crate::error::CoreError::Internal("semaphore closed".into()))?;

        let task_id = context.task.id;
        {
            let mut running = self.running_tasks.lock().await;
            running.insert(
                task_id,
                TaskExecutionState {
                    tool: tool_name.to_owned(),
                    started_at: std::time::Instant::now(),
                },
            );
        }

        let timeout = context.limits.timeout.max(self.global_limits.timeout);
        let execution_future = tool.execute(context, input);
        let result = match tokio::time::timeout(timeout, execution_future).await {
            Ok(result) => result,
            Err(_) => {
                return Err(crate::error::CoreError::ToolTimeout {
                    tool: tool_name.to_owned(),
                    seconds: timeout.as_secs(),
                });
            }
        };

        self.running_tasks.lock().await.remove(&task_id);

        if let Ok(output) = &result {
            let output_size = serde_json::to_vec(output).map(|v| v.len()).unwrap_or(0);
            if output_size > context.limits.max_output_bytes {
                return Err(crate::error::CoreError::ToolOutputLimitExceeded {
                    tool: tool_name.to_owned(),
                    bytes: output_size,
                    limit: context.limits.max_output_bytes,
                });
            }
        }

        result
    }

    pub async fn cancel(&self, task_id: uuid::Uuid) {
        self.running_tasks.lock().await.remove(&task_id);
    }

    pub fn registry(&self) -> &ToolRegistry {
        &self.registry
    }
}

struct TaskExecutionState {
    tool: String,
    started_at: std::time::Instant,
}

/// MCP tool adapter configuration.
pub struct McpToolConfig {
    pub service: Arc<dyn crate::mcp::McpToolService>,
    pub namespace: String,
}

impl Clone for McpToolConfig {
    fn clone(&self) -> Self {
        Self {
            service: Arc::clone(&self.service),
            namespace: self.namespace.clone(),
        }
    }
}

impl std::fmt::Debug for McpToolConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("McpToolConfig")
            .field("namespace", &self.namespace)
            .finish_non_exhaustive()
    }
}

impl McpToolConfig {
    pub fn new(service: Arc<dyn crate::mcp::McpToolService>, namespace: impl Into<String>) -> Self {
        Self {
            service,
            namespace: namespace.into(),
        }
    }
}
