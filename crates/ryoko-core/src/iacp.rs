//! Inter-Agent Communication Protocol (IACP) v1.0.
//!
//! Agents exchange typed, auditable envelopes instead of freeform text. The
//! [`IacpRouter`] is the only path through which an envelope can move: it
//! verifies the schema, enforces role/tool/path policies, reserves file locks
//! and token budget, journals the decision, and only then publishes the message
//! to subscribers (local WebSocket / Tauri event bridge).
//!
//! Transport is deliberately separated from policy. [`IacpService`] exposes a
//! JSON-RPC 2.0 ingress (`iacp.route`, `iacp.audit`, `iacp.budget`) that a
//! WebSocket adapter can call for every frame.

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::sync::{Arc, Mutex};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio::sync::broadcast;
use uuid::Uuid;

use crate::workspace::validate_relative_path;

pub const IACP_VERSION: &str = "1.0";
pub const JSONRPC_VERSION: &str = "2.0";

// JSON-RPC 2.0 error codes, plus Ryoko's policy range.
pub const RPC_PARSE_ERROR: i64 = -32700;
pub const RPC_INVALID_REQUEST: i64 = -32600;
pub const RPC_METHOD_NOT_FOUND: i64 = -32601;
pub const RPC_INVALID_PARAMS: i64 = -32602;
pub const RPC_POLICY_VIOLATION: i64 = -32010;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentIdentity {
    /// PascalCase, kebab-case or snake_case role name, e.g. `SeniorDeveloper`.
    pub role: String,
    pub agent_id: String,
}

impl AgentIdentity {
    pub fn new(role: impl Into<String>, agent_id: impl Into<String>) -> Self {
        Self {
            role: role.into(),
            agent_id: agent_id.into(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct IacpEnvelope {
    pub iacp_version: String,
    pub message_id: Uuid,
    pub timestamp: DateTime<Utc>,
    pub correlation_id: Uuid,
    pub sender: AgentIdentity,
    pub recipient: AgentIdentity,
    /// Serializes as top-level `event_type` and `payload` fields.
    #[serde(flatten)]
    pub payload: IacpPayload,
}

impl IacpEnvelope {
    pub fn new(sender: AgentIdentity, recipient: AgentIdentity, payload: IacpPayload) -> Self {
        Self {
            iacp_version: IACP_VERSION.to_owned(),
            message_id: Uuid::new_v4(),
            timestamp: Utc::now(),
            correlation_id: Uuid::new_v4(),
            sender,
            recipient,
            payload,
        }
    }

    pub fn with_correlation_id(mut self, correlation_id: Uuid) -> Self {
        self.correlation_id = correlation_id;
        self
    }

    pub fn event_type(&self) -> &'static str {
        self.payload.event_type()
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(
    tag = "event_type",
    content = "payload",
    rename_all = "SCREAMING_SNAKE_CASE"
)]
pub enum IacpPayload {
    TaskDelegated {
        task_id: Uuid,
        title: String,
        priority: TaskPriority,
        scope: TaskScope,
        requirements: Vec<String>,
        allocated_token_budget: u64,
    },
    CodeReviewRequested {
        proposal_id: Uuid,
        source_branch: String,
        target_branch: String,
        changed_files: Vec<FileChangeMetadata>,
        compilation_status: CompilationStatus,
        sandbox_test_results: SandboxTestResults,
    },
    CodeReviewFeedback {
        proposal_id: Uuid,
        verdict: ReviewVerdict,
        global_summary: String,
        annotations: Vec<ReviewAnnotation>,
    },
    ContextQueryRequest {
        query_id: Uuid,
        query_type: String,
        parameters: BTreeMap<String, Value>,
    },
}

impl IacpPayload {
    pub fn event_type(&self) -> &'static str {
        match self {
            Self::TaskDelegated { .. } => "TASK_DELEGATED",
            Self::CodeReviewRequested { .. } => "CODE_REVIEW_REQUESTED",
            Self::CodeReviewFeedback { .. } => "CODE_REVIEW_FEEDBACK",
            Self::ContextQueryRequest { .. } => "CONTEXT_QUERY_REQUEST",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum TaskPriority {
    Critical,
    High,
    Medium,
    Low,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskScope {
    pub target_directories: Vec<String>,
    pub allowed_tools: Vec<String>,
    pub file_locks: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum FileChangeStatus {
    Added,
    Modified,
    Deleted,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileChangeMetadata {
    pub path: String,
    pub status: FileChangeStatus,
    pub lines_added: usize,
    pub lines_removed: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum CompilationStatus {
    Success,
    Failed,
    NotRun,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SandboxTestResults {
    pub tests_run: u32,
    pub tests_failed: u32,
    pub execution_time_ms: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum ReviewVerdict {
    Approved,
    ChangesRequested,
    Rejected,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum ReviewSeverity {
    Info,
    Warning,
    Critical,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReviewAnnotation {
    pub file_path: String,
    pub line_number: usize,
    pub severity: ReviewSeverity,
    pub issue_type: String,
    pub comment: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub suggested_fix: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum IacpError {
    #[error("unsupported IACP version `{0}`; expected `1.0`")]
    UnsupportedVersion(String),

    #[error("invalid envelope: {0}")]
    InvalidEnvelope(String),

    #[error("invalid `{event_type}` payload: {reason}")]
    InvalidPayload {
        event_type: &'static str,
        reason: String,
    },

    #[error("unauthorized route: `{sender}` may not send `{event_type}` to `{recipient}`")]
    UnauthorizedRoute {
        sender: String,
        recipient: String,
        event_type: &'static str,
    },

    #[error("policy violation: {0}")]
    PolicyViolation(String),

    #[error("file lock conflict: `{path}` is currently held by agent `{owner}`")]
    FileLockConflict { path: String, owner: String },

    #[error(
        "token budget exceeded for agent `{agent_id}`: requested {requested}, remaining {remaining}"
    )]
    TokenBudgetExceeded {
        agent_id: String,
        requested: u64,
        remaining: u64,
    },
}

impl IacpError {
    pub fn rpc_code(&self) -> i64 {
        match self {
            Self::InvalidEnvelope(_) | Self::InvalidPayload { .. } => RPC_INVALID_PARAMS,
            _ => RPC_POLICY_VIOLATION,
        }
    }

    pub fn kind(&self) -> &'static str {
        match self {
            Self::UnsupportedVersion(_) => "unsupported_version",
            Self::InvalidEnvelope(_) => "invalid_envelope",
            Self::InvalidPayload { .. } => "invalid_payload",
            Self::UnauthorizedRoute { .. } => "unauthorized_route",
            Self::PolicyViolation(_) => "policy_violation",
            Self::FileLockConflict { .. } => "file_lock_conflict",
            Self::TokenBudgetExceeded { .. } => "token_budget_exceeded",
        }
    }
}

#[derive(Debug, Clone)]
pub struct IacpPolicy {
    /// Explicit tool allowlist. Anything else is rejected before an LLM or
    /// terminal bridge ever sees the delegation.
    pub allowed_tools: BTreeSet<String>,
    /// Ceiling for a single `allocated_token_budget` grant.
    pub max_token_allocation: u64,
    pub max_audit_entries: usize,
}

impl Default for IacpPolicy {
    fn default() -> Self {
        Self {
            allowed_tools: [
                "read_file",
                "write_file",
                "apply_patch",
                "list_files",
                "search",
                "grep",
                "cargo_check",
                "cargo_test",
                "cargo_build",
                "run_tests",
            ]
            .into_iter()
            .map(str::to_owned)
            .collect(),
            max_token_allocation: 1_000_000,
            max_audit_entries: 10_000,
        }
    }
}

impl IacpPolicy {
    pub fn new(allowed_tools: impl IntoIterator<Item = impl Into<String>>) -> Self {
        Self {
            allowed_tools: allowed_tools.into_iter().map(Into::into).collect(),
            ..Self::default()
        }
    }

    pub fn with_max_token_allocation(mut self, max: u64) -> Self {
        self.max_token_allocation = max;
        self
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AuditEntry {
    pub sequence: u64,
    pub recorded_at: DateTime<Utc>,
    pub envelope: IacpEnvelope,
    pub accepted: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rejection: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RoutedMessage {
    pub message_id: Uuid,
    pub event_type: String,
    pub accepted: bool,
    pub sender: AgentIdentity,
    pub recipient: AgentIdentity,
    pub audit_sequence: u64,
    pub locked_files: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BudgetStatus {
    pub agent_id: String,
    pub granted_tokens: u64,
    pub used_tokens: u64,
    pub remaining_tokens: u64,
}

#[derive(Debug, Clone)]
struct LockEntry {
    owner: String,
    task_id: Uuid,
}

#[derive(Debug, Default)]
struct RouterState {
    sequence: u64,
    audit: VecDeque<AuditEntry>,
    grants: BTreeMap<String, u64>,
    usage: BTreeMap<String, u64>,
    locks: BTreeMap<String, LockEntry>,
}

struct RouterInner {
    policy: IacpPolicy,
    state: Mutex<RouterState>,
    bus: broadcast::Sender<IacpEnvelope>,
}

/// Policy-enforcing message router and audit journal. Cheap to clone; clones
/// share the same state and subscriber bus.
#[derive(Clone)]
pub struct IacpRouter {
    inner: Arc<RouterInner>,
}

impl std::fmt::Debug for IacpRouter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("IacpRouter")
            .field("policy", &self.inner.policy)
            .finish_non_exhaustive()
    }
}

impl IacpRouter {
    pub fn new(policy: IacpPolicy) -> Self {
        let (bus, _) = broadcast::channel(1_024);
        Self {
            inner: Arc::new(RouterInner {
                policy,
                state: Mutex::new(RouterState::default()),
                bus,
            }),
        }
    }

    pub fn policy(&self) -> &IacpPolicy {
        &self.inner.policy
    }

    /// Subscribers receive only accepted envelopes, in order.
    pub fn subscribe(&self) -> broadcast::Receiver<IacpEnvelope> {
        self.inner.bus.subscribe()
    }

    /// Validates, enforces policy and journals an envelope. The envelope is
    /// published to subscribers only when this returns `Ok`.
    pub fn route(&self, envelope: IacpEnvelope) -> Result<RoutedMessage, IacpError> {
        let event_type = envelope.event_type().to_owned();
        let outcome = self.accept(&envelope);
        let locked_files = if outcome.is_ok() {
            match &envelope.payload {
                IacpPayload::TaskDelegated { scope, .. } => scope.file_locks.clone(),
                _ => Vec::new(),
            }
        } else {
            Vec::new()
        };
        let audit_sequence = self.journal(&envelope, &outcome);

        match outcome {
            Ok(()) => {
                let routed = RoutedMessage {
                    message_id: envelope.message_id,
                    event_type,
                    accepted: true,
                    sender: envelope.sender.clone(),
                    recipient: envelope.recipient.clone(),
                    audit_sequence,
                    locked_files,
                };
                let _ = self.inner.bus.send(envelope);
                Ok(routed)
            }
            Err(err) => Err(err),
        }
    }

    fn accept(&self, envelope: &IacpEnvelope) -> Result<(), IacpError> {
        self.validate_envelope(envelope)?;
        self.validate_payload(&envelope.payload)?;
        self.authorize(envelope)?;

        let mut state = self.lock_state();
        if let IacpPayload::TaskDelegated {
            task_id,
            scope,
            allocated_token_budget,
            ..
        } = &envelope.payload
        {
            // Check every lock before mutating so a conflict cannot leave the
            // envelope half-applied.
            let mut normalized = Vec::with_capacity(scope.file_locks.len());
            for path in &scope.file_locks {
                let clean = validate_relative_path(path)
                    .map_err(|err| IacpError::InvalidPayload {
                        event_type: "TASK_DELEGATED",
                        reason: err.to_string(),
                    })?
                    .to_string_lossy()
                    .replace('\\', "/");
                if let Some(entry) = state.locks.get(&clean)
                    && entry.owner != envelope.recipient.agent_id
                {
                    return Err(IacpError::FileLockConflict {
                        path: clean,
                        owner: entry.owner.clone(),
                    });
                }
                normalized.push(clean);
            }

            for path in normalized {
                state.locks.insert(
                    path,
                    LockEntry {
                        owner: envelope.recipient.agent_id.clone(),
                        task_id: *task_id,
                    },
                );
            }

            let grant = state
                .grants
                .entry(envelope.recipient.agent_id.clone())
                .or_insert(0);
            *grant = grant.saturating_add(*allocated_token_budget);
        }

        Ok(())
    }

    fn validate_envelope(&self, envelope: &IacpEnvelope) -> Result<(), IacpError> {
        if envelope.iacp_version != IACP_VERSION {
            return Err(IacpError::UnsupportedVersion(envelope.iacp_version.clone()));
        }
        if envelope.message_id.is_nil() {
            return Err(IacpError::InvalidEnvelope(
                "message_id must not be nil".to_owned(),
            ));
        }
        if envelope.correlation_id.is_nil() {
            return Err(IacpError::InvalidEnvelope(
                "correlation_id must not be nil".to_owned(),
            ));
        }
        if envelope.sender.agent_id.trim().is_empty()
            || envelope.recipient.agent_id.trim().is_empty()
        {
            return Err(IacpError::InvalidEnvelope(
                "sender.agent_id and recipient.agent_id are required".to_owned(),
            ));
        }
        if envelope.sender.role.trim().is_empty() || envelope.recipient.role.trim().is_empty() {
            return Err(IacpError::InvalidEnvelope(
                "sender.role and recipient.role are required".to_owned(),
            ));
        }
        if envelope.sender.agent_id == envelope.recipient.agent_id {
            return Err(IacpError::InvalidEnvelope(
                "sender and recipient must be different agents".to_owned(),
            ));
        }
        Ok(())
    }

    fn validate_payload(&self, payload: &IacpPayload) -> Result<(), IacpError> {
        let payload_error = |reason: String| IacpError::InvalidPayload {
            event_type: payload.event_type(),
            reason,
        };

        match payload {
            IacpPayload::TaskDelegated {
                task_id,
                title,
                scope,
                requirements,
                allocated_token_budget,
                ..
            } => {
                if task_id.is_nil() {
                    return Err(payload_error("task_id must not be nil".to_owned()));
                }
                if title.trim().is_empty() {
                    return Err(payload_error("title must not be empty".to_owned()));
                }
                if scope.target_directories.is_empty() {
                    return Err(payload_error(
                        "scope.target_directories must contain at least one path".to_owned(),
                    ));
                }
                for directory in &scope.target_directories {
                    validate_relative_path(directory)
                        .map_err(|err| payload_error(err.to_string()))?;
                }
                if scope.allowed_tools.is_empty() {
                    return Err(payload_error(
                        "scope.allowed_tools must not be empty".to_owned(),
                    ));
                }
                for tool in &scope.allowed_tools {
                    if !self.inner.policy.allowed_tools.contains(tool) {
                        return Err(IacpError::PolicyViolation(format!(
                            "tool `{tool}` is not on the orchestrator allowlist"
                        )));
                    }
                }
                for path in &scope.file_locks {
                    validate_relative_path(path).map_err(|err| payload_error(err.to_string()))?;
                }
                if requirements.is_empty() {
                    return Err(payload_error(
                        "at least one definition-of-done requirement is required".to_owned(),
                    ));
                }
                if let Some(blank) = requirements.iter().find(|req| req.trim().is_empty()) {
                    return Err(payload_error(format!(
                        "requirement must not be blank (found `{blank}`)"
                    )));
                }
                if *allocated_token_budget == 0 {
                    return Err(payload_error(
                        "allocated_token_budget must be greater than zero".to_owned(),
                    ));
                }
                if *allocated_token_budget > self.inner.policy.max_token_allocation {
                    return Err(IacpError::PolicyViolation(format!(
                        "allocated_token_budget {} exceeds the per-task ceiling of {}",
                        allocated_token_budget, self.inner.policy.max_token_allocation
                    )));
                }
            }
            IacpPayload::CodeReviewRequested {
                proposal_id,
                source_branch,
                target_branch,
                changed_files,
                sandbox_test_results,
                ..
            } => {
                if proposal_id.is_nil() {
                    return Err(payload_error("proposal_id must not be nil".to_owned()));
                }
                validate_branch(source_branch).map_err(payload_error)?;
                validate_branch(target_branch).map_err(payload_error)?;
                if source_branch == target_branch {
                    return Err(payload_error(
                        "source_branch and target_branch must differ".to_owned(),
                    ));
                }
                if changed_files.is_empty() {
                    return Err(payload_error(
                        "changed_files must contain at least one file".to_owned(),
                    ));
                }
                let mut seen = BTreeSet::new();
                for file in changed_files {
                    validate_relative_path(&file.path)
                        .map_err(|err| payload_error(err.to_string()))?;
                    if !seen.insert(&file.path) {
                        return Err(payload_error(format!(
                            "duplicate changed file `{}`",
                            file.path
                        )));
                    }
                }
                if sandbox_test_results.tests_failed > sandbox_test_results.tests_run {
                    return Err(payload_error(
                        "tests_failed cannot exceed tests_run".to_owned(),
                    ));
                }
            }
            IacpPayload::CodeReviewFeedback {
                proposal_id,
                global_summary,
                annotations,
                ..
            } => {
                if proposal_id.is_nil() {
                    return Err(payload_error("proposal_id must not be nil".to_owned()));
                }
                if global_summary.trim().is_empty() {
                    return Err(payload_error("global_summary must not be empty".to_owned()));
                }
                for annotation in annotations {
                    validate_relative_path(&annotation.file_path)
                        .map_err(|err| payload_error(err.to_string()))?;
                    if annotation.line_number == 0 {
                        return Err(payload_error(format!(
                            "annotation for `{}` must target line >= 1",
                            annotation.file_path
                        )));
                    }
                    if annotation.comment.trim().is_empty() {
                        return Err(payload_error(format!(
                            "annotation for `{}` must include a comment",
                            annotation.file_path
                        )));
                    }
                    if annotation.issue_type.trim().is_empty() {
                        return Err(payload_error(format!(
                            "annotation for `{}` must include an issue_type",
                            annotation.file_path
                        )));
                    }
                }
            }
            IacpPayload::ContextQueryRequest {
                query_id,
                query_type,
                parameters,
            } => {
                if query_id.is_nil() {
                    return Err(payload_error("query_id must not be nil".to_owned()));
                }
                if query_type.trim().is_empty() {
                    return Err(payload_error("query_type must not be empty".to_owned()));
                }
                if parameters.is_empty() {
                    return Err(payload_error("parameters must not be empty".to_owned()));
                }
                if let Some(entity) = parameters.get("entity_name")
                    && entity.as_str().is_none_or(|name| name.trim().is_empty())
                {
                    return Err(payload_error(
                        "parameters.entity_name must be a non-empty string".to_owned(),
                    ));
                }
            }
        }
        Ok(())
    }

    fn authorize(&self, envelope: &IacpEnvelope) -> Result<(), IacpError> {
        let sender = normalize_role(&envelope.sender.role);
        let recipient = normalize_role(&envelope.recipient.role);
        let allowed = match &envelope.payload {
            IacpPayload::TaskDelegated { .. } => {
                matches!(sender.as_str(), "projectmanager" | "techlead")
            }
            IacpPayload::CodeReviewRequested { .. } => {
                !matches!(sender.as_str(), "projectmanager" | "codereviewer")
                    && matches!(recipient.as_str(), "codereviewer" | "qaengineer")
            }
            IacpPayload::CodeReviewFeedback { .. } => {
                matches!(sender.as_str(), "codereviewer" | "qaengineer")
            }
            IacpPayload::ContextQueryRequest { .. } => true,
        };

        if allowed {
            Ok(())
        } else {
            Err(IacpError::UnauthorizedRoute {
                sender: envelope.sender.role.clone(),
                recipient: envelope.recipient.role.clone(),
                event_type: envelope.event_type(),
            })
        }
    }

    fn lock_state(&self) -> std::sync::MutexGuard<'_, RouterState> {
        self.inner
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    fn journal(&self, envelope: &IacpEnvelope, outcome: &Result<(), IacpError>) -> u64 {
        let mut state = self.lock_state();
        state.sequence += 1;
        let sequence = state.sequence;
        state.audit.push_back(AuditEntry {
            sequence,
            recorded_at: Utc::now(),
            envelope: envelope.clone(),
            accepted: outcome.is_ok(),
            rejection: outcome.as_ref().err().map(ToString::to_string),
        });
        while state.audit.len() > self.inner.policy.max_audit_entries {
            state.audit.pop_front();
        }
        sequence
    }

    pub fn audit_log(&self) -> Vec<AuditEntry> {
        self.lock_state().audit.iter().cloned().collect()
    }

    pub fn audit_len(&self) -> usize {
        self.lock_state().audit.len()
    }

    /// Charges tokens against the budget granted to `agent_id` by
    /// `TASK_DELEGATED`. Call this from the model gateway before dispatch.
    pub fn record_token_usage(&self, agent_id: &str, tokens: u64) -> Result<u64, IacpError> {
        let mut state = self.lock_state();
        let granted = state.grants.get(agent_id).copied().unwrap_or(0);
        let used = state.usage.get(agent_id).copied().unwrap_or(0);
        let remaining = granted.saturating_sub(used);

        if tokens > remaining {
            return Err(IacpError::TokenBudgetExceeded {
                agent_id: agent_id.to_owned(),
                requested: tokens,
                remaining,
            });
        }

        let new_used = used + tokens;
        state.usage.insert(agent_id.to_owned(), new_used);
        Ok(granted.saturating_sub(new_used))
    }

    pub fn budget(&self, agent_id: &str) -> BudgetStatus {
        let state = self.lock_state();
        let granted = state.grants.get(agent_id).copied().unwrap_or(0);
        let used = state.usage.get(agent_id).copied().unwrap_or(0);
        BudgetStatus {
            agent_id: agent_id.to_owned(),
            granted_tokens: granted,
            used_tokens: used,
            remaining_tokens: granted.saturating_sub(used),
        }
    }

    /// Releases every file lock held by an agent (normally at task completion).
    pub fn release_task_locks(&self, task_id: Uuid) -> Vec<String> {
        let mut state = self.lock_state();
        let released: Vec<String> = state
            .locks
            .iter()
            .filter(|(_, entry)| entry.task_id == task_id)
            .map(|(path, _)| path.clone())
            .collect();
        for path in &released {
            state.locks.remove(path);
        }
        released
    }

    pub fn release_agent_locks(&self, agent_id: &str) -> Vec<String> {
        let mut state = self.lock_state();
        let released: Vec<String> = state
            .locks
            .iter()
            .filter(|(_, entry)| entry.owner == agent_id)
            .map(|(path, _)| path.clone())
            .collect();
        for path in &released {
            state.locks.remove(path);
        }
        released
    }

    pub fn locks(&self) -> BTreeMap<String, String> {
        self.lock_state()
            .locks
            .iter()
            .map(|(path, entry)| (path.clone(), entry.owner.clone()))
            .collect()
    }
}

fn normalize_role(role: &str) -> String {
    role.chars()
        .filter(|character| character.is_ascii_alphanumeric())
        .map(|character| character.to_ascii_lowercase())
        .collect()
}

fn validate_branch(branch: &str) -> Result<(), String> {
    if branch.trim().is_empty() {
        return Err("branch name must not be empty".to_owned());
    }
    if branch.starts_with('-') || branch.starts_with('/') || branch.ends_with('/') {
        return Err(format!("invalid branch name `{branch}`"));
    }
    if branch.contains("..") || branch.contains('\\') {
        return Err(format!("invalid branch name `{branch}`"));
    }
    if branch
        .chars()
        .any(|character| character.is_whitespace() || character.is_control())
    {
        return Err(format!(
            "branch name must not contain whitespace: `{branch}`"
        ));
    }
    Ok(())
}

/// JSON-RPC 2.0 boundary for a local WebSocket transport.
#[derive(Debug, Clone)]
pub struct IacpService {
    router: IacpRouter,
}

impl IacpService {
    pub fn new(policy: IacpPolicy) -> Self {
        Self {
            router: IacpRouter::new(policy),
        }
    }

    pub fn with_router(router: IacpRouter) -> Self {
        Self { router }
    }

    pub fn router(&self) -> &IacpRouter {
        &self.router
    }

    pub fn route(&self, envelope: IacpEnvelope) -> Result<RoutedMessage, IacpError> {
        self.router.route(envelope)
    }

    pub fn handle_json_rpc(&self, raw: &str) -> String {
        let response = match serde_json::from_str::<Value>(raw) {
            Ok(request) => self.handle_value(&request),
            Err(err) => rpc_error(
                Value::Null,
                RPC_PARSE_ERROR,
                format!("parse error: {err}"),
                "parse_error",
            ),
        };
        serde_json::to_string(&response).unwrap_or_else(|err| {
            format!(r#"{{"jsonrpc":"2.0","id":null,"error":{{"code":-32603,"message":"{err}"}}}}"#)
        })
    }

    pub fn handle_value(&self, request: &Value) -> Value {
        let id = request.get("id").cloned().unwrap_or(Value::Null);

        if !request.is_object()
            || request.get("jsonrpc").and_then(Value::as_str) != Some(JSONRPC_VERSION)
        {
            return rpc_error(
                id,
                RPC_INVALID_REQUEST,
                "invalid JSON-RPC 2.0 request".to_owned(),
                "invalid_request",
            );
        }

        let Some(method) = request.get("method").and_then(Value::as_str) else {
            return rpc_error(
                id,
                RPC_INVALID_REQUEST,
                "method must be a string".to_owned(),
                "invalid_request",
            );
        };

        match method {
            "iacp.route" => {
                let Some(params) = request.get("params") else {
                    return rpc_error(
                        id,
                        RPC_INVALID_PARAMS,
                        "params must contain an IACP envelope".to_owned(),
                        "invalid_params",
                    );
                };
                let envelope: IacpEnvelope = match serde_json::from_value(params.clone()) {
                    Ok(envelope) => envelope,
                    Err(err) => {
                        return rpc_error(
                            id,
                            RPC_INVALID_PARAMS,
                            format!("invalid IACP envelope: {err}"),
                            "invalid_params",
                        );
                    }
                };

                match self.router.route(envelope) {
                    Ok(routed) => rpc_result(id, json!(routed)),
                    Err(err) => rpc_error(id, err.rpc_code(), err.to_string(), err.kind()),
                }
            }
            "iacp.audit" => rpc_result(id, json!(self.router.audit_log())),
            "iacp.budget" => {
                let agent_id = request
                    .get("params")
                    .and_then(|params| params.get("agent_id"))
                    .and_then(Value::as_str);
                let Some(agent_id) = agent_id else {
                    return rpc_error(
                        id,
                        RPC_INVALID_PARAMS,
                        "params.agent_id is required".to_owned(),
                        "invalid_params",
                    );
                };
                rpc_result(id, json!(self.router.budget(agent_id)))
            }
            other => rpc_error(
                id,
                RPC_METHOD_NOT_FOUND,
                format!("unknown method `{other}`"),
                "method_not_found",
            ),
        }
    }
}

fn rpc_result(id: Value, result: Value) -> Value {
    json!({"jsonrpc": JSONRPC_VERSION, "id": id, "result": result})
}

fn rpc_error(id: Value, code: i64, message: String, kind: &str) -> Value {
    json!({
        "jsonrpc": JSONRPC_VERSION,
        "id": id,
        "error": {"code": code, "message": message, "data": {"kind": kind}}
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pm() -> AgentIdentity {
        AgentIdentity::new("ProjectManager", "pm-agent-01")
    }

    fn dev() -> AgentIdentity {
        AgentIdentity::new("SeniorDeveloper", "dev-agent-01")
    }

    fn reviewer() -> AgentIdentity {
        AgentIdentity::new("CodeReviewer", "review-agent-01")
    }

    fn delegated(token_budget: u64, locks: &[&str]) -> IacpPayload {
        IacpPayload::TaskDelegated {
            task_id: Uuid::new_v4(),
            title: "Implement high-speed vector indexing".to_owned(),
            priority: TaskPriority::High,
            scope: TaskScope {
                target_directories: vec!["src/indexing".to_owned()],
                allowed_tools: vec!["read_file".to_owned(), "cargo_test".to_owned()],
                file_locks: locks.iter().map(|path| (*path).to_owned()).collect(),
            },
            requirements: vec!["Unit tests must achieve 85% coverage".to_owned()],
            allocated_token_budget: token_budget,
        }
    }

    fn review_requested() -> IacpPayload {
        IacpPayload::CodeReviewRequested {
            proposal_id: Uuid::new_v4(),
            source_branch: "ryoko/agent-dev-01/vector-indexing".to_owned(),
            target_branch: "main".to_owned(),
            changed_files: vec![FileChangeMetadata {
                path: "src/indexing/vector.rs".to_owned(),
                status: FileChangeStatus::Added,
                lines_added: 242,
                lines_removed: 0,
            }],
            compilation_status: CompilationStatus::Success,
            sandbox_test_results: SandboxTestResults {
                tests_run: 14,
                tests_failed: 0,
                execution_time_ms: 124,
            },
        }
    }

    fn feedback() -> IacpPayload {
        IacpPayload::CodeReviewFeedback {
            proposal_id: Uuid::new_v4(),
            verdict: ReviewVerdict::ChangesRequested,
            global_summary: "Sound maths, unsafe alignment on line 42.".to_owned(),
            annotations: vec![ReviewAnnotation {
                file_path: "src/indexing/vector.rs".to_owned(),
                line_number: 42,
                severity: ReviewSeverity::Critical,
                issue_type: "MemorySafety".to_owned(),
                comment: "Wraps an unaligned pointer.".to_owned(),
                suggested_fix: Some("let aligned = raw.align_to::<u8>();".to_owned()),
            }],
        }
    }

    #[test]
    fn envelope_round_trips_with_flat_event_type_and_payload() {
        let envelope = IacpEnvelope::new(pm(), dev(), delegated(500_000, &["src/lib.rs"]));
        let json = serde_json::to_value(&envelope).unwrap();

        assert_eq!(json["iacp_version"], "1.0");
        assert!(json["timestamp"].as_str().unwrap().ends_with('Z'));
        assert_eq!(json["event_type"], "TASK_DELEGATED");
        assert_eq!(
            json["payload"]["title"],
            "Implement high-speed vector indexing"
        );
        assert_eq!(json["payload"]["priority"], "HIGH");
        assert_eq!(json["payload"]["scope"]["file_locks"][0], "src/lib.rs");
        assert!(json["payload"].get("event_type").is_none());

        let parsed: IacpEnvelope = serde_json::from_value(json).unwrap();
        assert_eq!(parsed, envelope);
    }

    #[test]
    fn parses_documented_payload_d() {
        let raw = json!({
            "iacp_version": "1.0",
            "message_id": "e2a5f4b8-c3d9-4b2e-a1f7-6c8d9e0f1a2b",
            "timestamp": "2026-11-07T05:52:00Z",
            "correlation_id": "9b1deb4d-3b7d-4bad-9bdd-2b0d7b3dcb6d",
            "sender": {"role": "UiUxDesigner", "agent_id": "designer-01"},
            "recipient": {"role": "SeniorDeveloper", "agent_id": "dev-agent-01"},
            "event_type": "CONTEXT_QUERY_REQUEST",
            "payload": {
                "query_id": "f47ac10b-58cc-4372-a567-0e02b2c3d479",
                "query_type": "SCHEMA_DEFINITION",
                "parameters": {
                    "entity_name": "UserSession",
                    "preferred_format": "TypeScript_Interface"
                }
            }
        });

        let envelope: IacpEnvelope = serde_json::from_value(raw).unwrap();
        assert_eq!(envelope.event_type(), "CONTEXT_QUERY_REQUEST");
        assert_eq!(envelope.sender.role, "UiUxDesigner");
    }

    #[test]
    fn rejects_unknown_event_types_and_versions() {
        let unknown = json!({
            "iacp_version": "1.0",
            "message_id": Uuid::new_v4(),
            "timestamp": "2026-11-07T05:52:00Z",
            "correlation_id": Uuid::new_v4(),
            "sender": {"role": "ProjectManager", "agent_id": "pm"},
            "recipient": {"role": "SeniorDeveloper", "agent_id": "dev"},
            "event_type": "SHELL_EXEC_REQUEST",
            "payload": {}
        });
        assert!(serde_json::from_value::<IacpEnvelope>(unknown).is_err());

        let router = IacpRouter::new(IacpPolicy::default());
        let mut envelope = IacpEnvelope::new(pm(), dev(), delegated(100, &[]));
        envelope.iacp_version = "2.0".to_owned();
        assert!(matches!(
            router.route(envelope),
            Err(IacpError::UnsupportedVersion(_))
        ));
    }

    #[test]
    fn routes_delegation_and_grants_budget() {
        let router = IacpRouter::new(IacpPolicy::default());
        let routed = router
            .route(IacpEnvelope::new(
                pm(),
                dev(),
                delegated(500_000, &["src/lib.rs"]),
            ))
            .unwrap();

        assert_eq!(routed.event_type, "TASK_DELEGATED");
        assert_eq!(routed.locked_files, vec!["src/lib.rs"]);
        assert_eq!(router.budget("dev-agent-01").remaining_tokens, 500_000);
        assert_eq!(
            router.locks().get("src/lib.rs"),
            Some(&"dev-agent-01".to_owned())
        );
        assert_eq!(router.audit_len(), 1);
        assert!(router.audit_log()[0].accepted);
    }

    #[test]
    fn token_budget_is_enforced_before_dispatch() {
        let router = IacpRouter::new(IacpPolicy::default());
        router
            .route(IacpEnvelope::new(pm(), dev(), delegated(100, &[])))
            .unwrap();

        assert_eq!(router.record_token_usage("dev-agent-01", 60).unwrap(), 40);
        assert!(matches!(
            router.record_token_usage("dev-agent-01", 41),
            Err(IacpError::TokenBudgetExceeded {
                requested: 41,
                remaining: 40,
                ..
            })
        ));
        let budget = router.budget("dev-agent-01");
        assert_eq!(budget.used_tokens, 60);
        assert_eq!(budget.remaining_tokens, 40);
    }

    #[test]
    fn rejects_unauthorized_delegation_and_unknown_tools() {
        let router = IacpRouter::new(IacpPolicy::default());
        let designer = AgentIdentity::new("UI_Designer", "designer-01");
        let database = AgentIdentity::new("DatabaseEngineer", "db-01");

        assert!(matches!(
            router.route(IacpEnvelope::new(designer, database, delegated(100, &[]))),
            Err(IacpError::UnauthorizedRoute { .. })
        ));

        let mut payload = delegated(100, &[]);
        if let IacpPayload::TaskDelegated { scope, .. } = &mut payload {
            scope.allowed_tools.push("rm_rf".to_owned());
        }
        assert!(matches!(
            router.route(IacpEnvelope::new(pm(), dev(), payload)),
            Err(IacpError::PolicyViolation(_))
        ));
    }

    #[test]
    fn enforces_file_locks_and_release() {
        let router = IacpRouter::new(IacpPolicy::default());
        router
            .route(IacpEnvelope::new(
                pm(),
                dev(),
                delegated(100, &["src/lib.rs"]),
            ))
            .unwrap();

        let second = IacpEnvelope::new(
            pm(),
            AgentIdentity::new("UiUxDesigner", "designer-01"),
            delegated(100, &["src/lib.rs"]),
        );
        assert!(matches!(
            router.route(second),
            Err(IacpError::FileLockConflict { path, .. }) if path == "src/lib.rs"
        ));

        assert_eq!(
            router.release_agent_locks("dev-agent-01"),
            vec!["src/lib.rs"]
        );
        router
            .route(IacpEnvelope::new(
                pm(),
                AgentIdentity::new("UiUxDesigner", "designer-01"),
                delegated(100, &["src/lib.rs"]),
            ))
            .unwrap();
    }

    #[test]
    fn review_flow_and_context_queries_are_type_checked() {
        let router = IacpRouter::new(IacpPolicy::default());
        router
            .route(IacpEnvelope::new(dev(), reviewer(), review_requested()))
            .unwrap();
        router
            .route(IacpEnvelope::new(reviewer(), dev(), feedback()))
            .unwrap();

        let mut bad = feedback();
        if let IacpPayload::CodeReviewFeedback { annotations, .. } = &mut bad {
            annotations[0].line_number = 0;
        }
        assert!(matches!(
            router.route(IacpEnvelope::new(reviewer(), dev(), bad)),
            Err(IacpError::InvalidPayload { .. })
        ));

        let designer = AgentIdentity::new("UiUxDesigner", "designer-01");
        let query = IacpPayload::ContextQueryRequest {
            query_id: Uuid::new_v4(),
            query_type: "SCHEMA_DEFINITION".to_owned(),
            parameters: BTreeMap::from([("entity_name".to_owned(), json!("UserSession"))]),
        };
        router
            .route(IacpEnvelope::new(designer, dev(), query))
            .unwrap();
    }

    #[test]
    fn rejects_traversal_paths_in_delegations() {
        let router = IacpRouter::new(IacpPolicy::default());
        let mut payload = delegated(100, &["../escape"]);
        if let IacpPayload::TaskDelegated { scope, .. } = &mut payload {
            scope.target_directories = vec!["src/../../etc".to_owned()];
        }
        assert!(matches!(
            router.route(IacpEnvelope::new(pm(), dev(), payload)),
            Err(IacpError::InvalidPayload { .. })
        ));
    }

    #[test]
    fn audit_journals_rejections_in_sequence() {
        let router = IacpRouter::new(IacpPolicy::default());
        let _ = router.route(IacpEnvelope::new(
            AgentIdentity::new("UI_Designer", "designer-01"),
            dev(),
            delegated(100, &[]),
        ));
        let audit = router.audit_log();
        assert_eq!(audit.len(), 1);
        assert!(!audit[0].accepted);
        assert!(
            audit[0]
                .rejection
                .as_deref()
                .unwrap()
                .contains("unauthorized")
        );
    }

    #[tokio::test]
    async fn accepted_envelopes_are_published_to_subscribers() {
        let router = IacpRouter::new(IacpPolicy::default());
        let mut rx = router.subscribe();
        router
            .route(IacpEnvelope::new(pm(), dev(), delegated(100, &[])))
            .unwrap();
        let received = rx.recv().await.unwrap();
        assert_eq!(received.event_type(), "TASK_DELEGATED");
    }

    #[test]
    fn json_rpc_routes_and_reports_policy_errors() {
        let service = IacpService::new(IacpPolicy::default());
        let envelope = IacpEnvelope::new(pm(), dev(), delegated(100, &[]));
        let request = json!({
            "jsonrpc": "2.0",
            "id": 7,
            "method": "iacp.route",
            "params": envelope
        });

        let response = service.handle_value(&request);
        assert_eq!(response["id"], 7);
        assert_eq!(response["result"]["accepted"], true);

        let bad = json!({
            "jsonrpc": "2.0",
            "id": 8,
            "method": "iacp.route",
            "params": IacpEnvelope::new(
                AgentIdentity::new("UI_Designer", "designer-01"),
                dev(),
                delegated(100, &[])
            )
        });
        let response = service.handle_value(&bad);
        assert_eq!(response["error"]["code"], RPC_POLICY_VIOLATION);
        assert_eq!(response["error"]["data"]["kind"], "unauthorized_route");

        let unknown = json!({"jsonrpc": "2.0", "id": 9, "method": "iacp.nope"});
        assert_eq!(
            service.handle_value(&unknown)["error"]["code"],
            RPC_METHOD_NOT_FOUND
        );

        let parse_error = service.handle_json_rpc("{not json");
        assert!(parse_error.contains("-32700"));
    }

    #[test]
    fn json_rpc_audit_and_budget_methods_work() {
        let service = IacpService::new(IacpPolicy::default());
        service
            .route(IacpEnvelope::new(pm(), dev(), delegated(100, &[])))
            .unwrap();

        let audit = service.handle_value(&json!({
            "jsonrpc": "2.0", "id": 1, "method": "iacp.audit"
        }));
        assert_eq!(audit["result"].as_array().unwrap().len(), 1);

        let budget = service.handle_value(&json!({
            "jsonrpc": "2.0", "id": 2, "method": "iacp.budget",
            "params": {"agent_id": "dev-agent-01"}
        }));
        assert_eq!(budget["result"]["remaining_tokens"], 100);
    }
}
