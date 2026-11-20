//! Transport-neutral MCP JSON-RPC client for host-approved tool calls.
//!
//! The core owns protocol validation and a mandatory human approval boundary.
//! Transports and UI prompts are supplied by a trusted host (CLI/desktop).
//! Tool output is untrusted data, never interpreted as agent instructions.
use std::collections::{BTreeSet, HashSet, VecDeque};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use ryoko_models::{ToolDefinition, UsageLedger};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

const MCP_PROTOCOL_VERSION: &str = "2025-06-18";
const MAX_TOOLS: usize = 256;
const MAX_PAGES: usize = 16;
const MAX_REQUEST_BYTES: usize = 64 * 1024;
const MAX_ARGUMENT_BYTES: usize = 64 * 1024;
const MAX_RESULT_BYTES: usize = 1024 * 1024;

#[derive(Debug, thiserror::Error)]
pub enum McpError {
    #[error("MCP transport error: {0}")]
    Transport(String),
    #[error("invalid MCP response: {0}")]
    InvalidResponse(String),
    #[error("unsupported MCP protocol version: {0}")]
    UnsupportedVersion(String),
    #[error("MCP server error: {0}")]
    Remote(String),
    #[error("MCP tool call denied by the human approval gate")]
    PermissionDenied,
    #[error("MCP tool `{0}` is not on the host allowlist")]
    ToolNotAllowed(String),
    #[error("headless-approved MCP tool `{0}` is not on the host allowlist")]
    HeadlessApprovalNotAllowlisted(String),
    #[error("MCP tool-call budget exceeded: {0}")]
    BudgetExceeded(String),
    #[error("MCP client has not been initialized")]
    NotInitialized,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct McpTool {
    pub name: String,
    #[serde(default)]
    pub description: String,
    #[serde(rename = "inputSchema")]
    pub input_schema: Value,
}

#[async_trait]
pub trait McpTransport: Send + Sync {
    /// Sends one JSON-RPC request and returns its response. Implementations must
    /// bound message size and request duration, and must not execute server text.
    async fn request(&self, payload: &Value) -> Result<Value, McpError>;
    /// Sends a JSON-RPC notification (which has no response).
    async fn notify(&self, payload: &Value) -> Result<(), McpError>;
}

#[async_trait]
pub trait McpApprovalGate: Send + Sync {
    /// A trusted host displays the tool and arguments to a human. An agent
    /// must never supply its own gate or mint an approval token.
    async fn approve(&self, tool: &str, arguments: &Value) -> bool;
}

/// How long a host grant for one MCP tool applies. This is the scope a native
/// approval dialog offers: allow this call, allow it for the rest of the run, or
/// refuse it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum McpApprovalScope {
    Once,
    /// Applies to every later call of the same tool for the rest of the run.
    ForRun,
    Deny,
}

/// Asks a trusted host to choose a scope for one tool call. Implementations are
/// UI channels (terminal prompt, native dialog); they must never be reachable
/// from model output.
#[async_trait]
pub trait McpApprovalAsker: Send + Sync {
    async fn ask(&self, tool: &str, arguments: &Value) -> McpApprovalScope;
}

/// One recorded approval decision. Only the tool name and scope are kept: the
/// arguments may contain credentials or customer data and never enter the log.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct McpApprovalDecision {
    pub tool: String,
    pub scope: McpApprovalScope,
    pub allowed: bool,
    /// True when an earlier run-scoped grant answered this call without asking.
    pub reused_grant: bool,
}

/// [`McpApprovalGate`] with per-tool session scopes.
///
/// A `ForRun` answer is remembered for that tool only, so approving `list_issues`
/// once does not approve `delete_issue`. A `Deny` stops asking for the rest of
/// the session, because re-prompting a refused tool is a nuisance, not consent.
/// Everything else asks every time, and a blank tool name is refused without
/// asking.
pub struct SessionApprovalGate {
    asker: Arc<dyn McpApprovalAsker>,
    granted: Mutex<BTreeSet<String>>,
    denied: Mutex<BTreeSet<String>>,
    decisions: Mutex<VecDeque<McpApprovalDecision>>,
}

const MAX_SESSION_GRANTS: usize = 64;
const MAX_DECISIONS: usize = 256;

impl SessionApprovalGate {
    pub fn new(asker: Arc<dyn McpApprovalAsker>) -> Self {
        Self {
            asker,
            granted: Mutex::new(BTreeSet::new()),
            denied: Mutex::new(BTreeSet::new()),
            decisions: Mutex::new(VecDeque::new()),
        }
    }

    /// Tools currently allowed for the rest of the session, for display.
    pub fn grants(&self) -> Vec<String> {
        self.granted
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .iter()
            .cloned()
            .collect()
    }

    /// Decisions in the order they were made, oldest first.
    pub fn decisions(&self) -> Vec<McpApprovalDecision> {
        self.decisions
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .iter()
            .cloned()
            .collect()
    }

    /// Drops every session grant, for example when a new run starts. A refusal
    /// stays in force until the gate itself is replaced.
    pub fn revoke_grants(&self) {
        self.granted
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .clear();
    }

    fn record(&self, decision: McpApprovalDecision) {
        let mut log = self
            .decisions
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        if log.len() >= MAX_DECISIONS {
            log.pop_front();
        }
        log.push_back(decision);
    }
}

#[async_trait]
impl McpApprovalGate for SessionApprovalGate {
    async fn approve(&self, tool: &str, arguments: &Value) -> bool {
        if tool.trim().is_empty() {
            self.record(McpApprovalDecision {
                tool: String::new(),
                scope: McpApprovalScope::Deny,
                allowed: false,
                reused_grant: false,
            });
            return false;
        }
        if self
            .denied
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .contains(tool)
        {
            self.record(McpApprovalDecision {
                tool: tool.to_owned(),
                scope: McpApprovalScope::Deny,
                allowed: false,
                reused_grant: true,
            });
            return false;
        }
        if self
            .granted
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .contains(tool)
        {
            self.record(McpApprovalDecision {
                tool: tool.to_owned(),
                scope: McpApprovalScope::ForRun,
                allowed: true,
                reused_grant: true,
            });
            return true;
        }

        let scope = self.asker.ask(tool, arguments).await;
        let allowed = scope != McpApprovalScope::Deny;
        if allowed && scope == McpApprovalScope::ForRun {
            let mut granted = self
                .granted
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            // A recursive approval loop must not grow without bound; past the
            // cap the call still succeeds but is not remembered.
            if granted.len() < MAX_SESSION_GRANTS {
                granted.insert(tool.to_owned());
            }
        }
        if !allowed {
            self.denied
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .insert(tool.to_owned());
        }
        self.record(McpApprovalDecision {
            tool: tool.to_owned(),
            scope,
            allowed,
            reused_grant: false,
        });
        allowed
    }
}

/// JSON-RPC MCP client. Use a trusted transport and approval gate; do not
/// allow model-generated configuration to choose a server executable.
pub struct McpClient<T: McpTransport> {
    transport: T,
    approval: Arc<dyn McpApprovalGate>,
    next_id: AtomicU64,
    initialized: std::sync::atomic::AtomicBool,
    allowed_tools: Option<BTreeSet<String>>,
}

impl<T: McpTransport> McpClient<T> {
    pub fn new(transport: T, approval: Arc<dyn McpApprovalGate>) -> Self {
        Self {
            transport,
            approval,
            next_id: AtomicU64::new(1),
            initialized: std::sync::atomic::AtomicBool::new(false),
            allowed_tools: None,
        }
    }

    /// Restricts protocol-level calls to a host-owned allowlist. The CLI's
    /// operator-facing `mcp call` still checks the advertised catalogue; the
    /// agent toolbox always supplies this list.
    pub fn with_allowed_tools(
        mut self,
        tools: impl IntoIterator<Item = impl Into<String>>,
    ) -> Self {
        self.allowed_tools = Some(tools.into_iter().map(Into::into).collect());
        self
    }

    async fn request(&self, method: &str, params: Value) -> Result<Value, McpError> {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let payload = json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": method,
            "params": params
        });
        let encoded = serde_json::to_vec(&payload)
            .map_err(|error| McpError::InvalidResponse(error.to_string()))?;
        if encoded.len() > MAX_REQUEST_BYTES {
            return Err(McpError::InvalidResponse(
                "MCP request exceeds 64 KiB".into(),
            ));
        }
        let response = self.transport.request(&payload).await?;
        if response["jsonrpc"] != "2.0" || response["id"].as_u64() != Some(id) {
            return Err(McpError::InvalidResponse(
                "JSON-RPC version or response ID mismatch".into(),
            ));
        }
        if let Some(error) = response.get("error") {
            let message = error
                .get("message")
                .and_then(Value::as_str)
                .unwrap_or("unspecified server failure");
            return Err(McpError::Remote(message.chars().take(512).collect()));
        }
        let result = response
            .get("result")
            .ok_or_else(|| McpError::InvalidResponse("missing result".into()))?;
        let encoded = serde_json::to_vec(result)
            .map_err(|error| McpError::InvalidResponse(error.to_string()))?;
        if encoded.len() > MAX_RESULT_BYTES {
            return Err(McpError::InvalidResponse(
                "result exceeds 1 MiB limit".into(),
            ));
        }
        Ok(result.clone())
    }

    pub async fn initialize(&self) -> Result<(), McpError> {
        let response = self
            .request(
                "initialize",
                json!({
                    "protocolVersion": MCP_PROTOCOL_VERSION,
                    "capabilities": {},
                    "clientInfo": {"name": "ryoko", "version": env!("CARGO_PKG_VERSION")}
                }),
            )
            .await?;
        let version = response["protocolVersion"]
            .as_str()
            .ok_or_else(|| McpError::InvalidResponse("missing protocolVersion".into()))?;
        if !matches!(version, "2025-06-18" | "2025-03-26") {
            return Err(McpError::UnsupportedVersion(
                version.chars().take(80).collect(),
            ));
        }
        self.transport
            .notify(&json!({"jsonrpc":"2.0", "method":"notifications/initialized"}))
            .await?;
        self.initialized.store(true, Ordering::Release);
        Ok(())
    }

    fn require_initialized(&self) -> Result<(), McpError> {
        if !self.initialized.load(Ordering::Acquire) {
            return Err(McpError::NotInitialized);
        }
        Ok(())
    }

    pub async fn list_tools(&self) -> Result<Vec<McpTool>, McpError> {
        self.require_initialized()?;
        let mut result = Vec::new();
        let mut cursor: Option<String> = None;
        let mut seen = HashSet::new();
        for _ in 0..MAX_PAGES {
            let params = cursor.as_ref().map_or(json!({}), |c| json!({"cursor": c}));
            let response = self.request("tools/list", params).await?;
            let tools = response["tools"].as_array().ok_or_else(|| {
                McpError::InvalidResponse("tools/list missing tools array".into())
            })?;
            if result.len().saturating_add(tools.len()) > MAX_TOOLS {
                return Err(McpError::InvalidResponse(
                    "tool catalogue exceeds limit".into(),
                ));
            }
            for raw in tools {
                let tool: McpTool = serde_json::from_value(raw.clone())
                    .map_err(|error| McpError::InvalidResponse(error.to_string()))?;
                if tool.name.is_empty()
                    || tool.name.len() > 128
                    || tool.name.chars().any(char::is_control)
                    || !tool.input_schema.is_object()
                    || result.iter().any(|known: &McpTool| known.name == tool.name)
                {
                    return Err(McpError::InvalidResponse(
                        "invalid or duplicate MCP tool".into(),
                    ));
                }
                result.push(tool);
            }
            match response.get("nextCursor").and_then(Value::as_str) {
                Some(next) if !next.is_empty() => {
                    if !seen.insert(next.to_owned()) {
                        return Err(McpError::InvalidResponse(
                            "cyclic MCP pagination cursor".into(),
                        ));
                    }
                    cursor = Some(next.to_owned());
                }
                _ => return Ok(result),
            }
        }
        Err(McpError::InvalidResponse(
            "MCP pagination limit exceeded".into(),
        ))
    }

    pub async fn call_tool(&self, tool: &str, arguments: Value) -> Result<Value, McpError> {
        self.require_initialized()?;
        if tool.is_empty()
            || tool.len() > 128
            || tool.chars().any(char::is_control)
            || !arguments.is_object()
        {
            return Err(McpError::InvalidResponse(
                "invalid tool name or arguments".into(),
            ));
        }
        if let Some(allowed) = &self.allowed_tools
            && !allowed.contains(tool)
        {
            return Err(McpError::ToolNotAllowed(tool.chars().take(64).collect()));
        }
        let encoded = serde_json::to_vec(&arguments)
            .map_err(|error| McpError::InvalidResponse(error.to_string()))?;
        if encoded.len() > MAX_ARGUMENT_BYTES {
            return Err(McpError::InvalidResponse(
                "MCP arguments exceed 64 KiB".into(),
            ));
        }
        // Approval is never inferred from successful initialization, discovery,
        // model choice or a previously approved call.
        if !self.approval.approve(tool, &arguments).await {
            return Err(McpError::PermissionDenied);
        }
        self.request("tools/call", json!({"name": tool, "arguments": arguments}))
            .await
    }
}

/// Host-owned tool policy for an agent session. The server catalogue is never
/// trusted to expand this list, and a missing allowed tool fails closed at
/// connection time rather than silently disappearing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct McpToolPolicy {
    pub allowed_tools: BTreeSet<String>,
    pub max_calls: u64,
    pub max_argument_bytes: usize,
}

impl McpToolPolicy {
    pub fn new(allowed_tools: impl IntoIterator<Item = impl Into<String>>, max_calls: u64) -> Self {
        Self {
            allowed_tools: allowed_tools.into_iter().map(Into::into).collect(),
            max_calls,
            max_argument_bytes: 64 * 1024,
        }
    }

    pub fn with_max_argument_bytes(mut self, max: usize) -> Self {
        self.max_argument_bytes = max.max(1);
        self
    }
}

/// Tool surface handed to a model agent. Implementations must keep approval,
/// allowlisting and budget checks behind this boundary.
#[async_trait]
pub trait McpToolService: Send + Sync {
    fn definitions(&self) -> Vec<ToolDefinition>;
    /// `scope` is the owning DAG task, when the caller has one. It lets the
    /// shared run ledger enforce that task's independent caps as well as the
    /// overall run cap.
    async fn call(
        &self,
        tool: &str,
        arguments: Value,
        scope: Option<&str>,
    ) -> Result<Value, McpError>;
}

/// Connects one MCP client to a policy, optional shared run ledger and task
/// scope. The ledger check happens before every tool call, so a run that has
/// already exhausted its budget cannot keep spending on external tools.
/// Tool calls are additionally capped independently of model tokens.
pub struct McpToolbox<T: McpTransport> {
    client: McpClient<T>,
    tools: Vec<McpTool>,
    policy: McpToolPolicy,
    calls: AtomicU64,
    ledger: Option<UsageLedger>,
    scope: Option<String>,
}

impl<T: McpTransport> McpToolbox<T> {
    pub async fn connect(
        client: McpClient<T>,
        policy: McpToolPolicy,
        ledger: Option<UsageLedger>,
        scope: Option<String>,
    ) -> Result<Self, McpError> {
        if policy.allowed_tools.is_empty() {
            return Err(McpError::ToolNotAllowed("<empty allowlist>".into()));
        }
        if policy.max_calls == 0 {
            return Err(McpError::BudgetExceeded(
                "MCP call budget must be greater than zero".into(),
            ));
        }
        client.initialize().await?;
        let advertised = client.list_tools().await?;
        let mut tools = Vec::with_capacity(advertised.len());
        let mut definition_bytes = 0usize;
        for tool in advertised {
            if policy.allowed_tools.contains(&tool.name) {
                definition_bytes = definition_bytes.saturating_add(
                    serde_json::to_vec(&tool)
                        .map_err(|error| McpError::InvalidResponse(error.to_string()))?
                        .len(),
                );
                if definition_bytes > 256 * 1024 {
                    return Err(McpError::InvalidResponse(
                        "allowed MCP tool definitions exceed 256 KiB".into(),
                    ));
                }
                tools.push(tool);
            }
        }
        for allowed in &policy.allowed_tools {
            if !tools.iter().any(|tool| &tool.name == allowed) {
                return Err(McpError::ToolNotAllowed(format!(
                    "server did not advertise allowed tool `{allowed}`"
                )));
            }
        }
        Ok(Self {
            client,
            tools,
            policy,
            calls: AtomicU64::new(0),
            ledger,
            scope,
        })
    }

    pub fn catalog(&self) -> &[McpTool] {
        &self.tools
    }

    pub fn remaining_calls(&self) -> u64 {
        self.policy
            .max_calls
            .saturating_sub(self.calls.load(Ordering::Relaxed))
    }
}

#[async_trait]
impl<T: McpTransport> McpToolService for McpToolbox<T> {
    fn definitions(&self) -> Vec<ToolDefinition> {
        self.tools
            .iter()
            .map(|tool| {
                ToolDefinition::new(
                    tool.name.clone(),
                    tool.description.chars().take(1024).collect::<String>(),
                    tool.input_schema.clone(),
                )
            })
            .collect()
    }

    async fn call(
        &self,
        tool: &str,
        arguments: Value,
        scope: Option<&str>,
    ) -> Result<Value, McpError> {
        if !self.policy.allowed_tools.contains(tool) || !self.tools.iter().any(|t| t.name == tool) {
            return Err(McpError::ToolNotAllowed(tool.chars().take(64).collect()));
        }
        if !arguments.is_object() {
            return Err(McpError::InvalidResponse(
                "tool arguments must be a JSON object".into(),
            ));
        }
        let encoded = serde_json::to_vec(&arguments)
            .map_err(|error| McpError::InvalidResponse(error.to_string()))?;
        if encoded.len() > self.policy.max_argument_bytes {
            return Err(McpError::InvalidResponse(
                "tool arguments exceed the host limit".into(),
            ));
        }
        if let Some(ledger) = &self.ledger {
            ledger
                .check_budget_for(scope.or(self.scope.as_deref()))
                .map_err(|error| McpError::BudgetExceeded(error.to_string()))?;
        }
        let previous = self.calls.fetch_add(1, Ordering::SeqCst);
        if previous >= self.policy.max_calls {
            self.calls.fetch_sub(1, Ordering::SeqCst);
            return Err(McpError::BudgetExceeded(format!(
                "tool call limit of {} reached",
                self.policy.max_calls
            )));
        }
        self.client.call_tool(tool, arguments).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    struct FakeTransport {
        calls: Mutex<Vec<String>>,
    }

    #[async_trait]
    impl McpTransport for FakeTransport {
        async fn request(&self, payload: &Value) -> Result<Value, McpError> {
            let method = payload["method"].as_str().unwrap().to_owned();
            self.calls.lock().unwrap().push(method.clone());
            let result = match method.as_str() {
                "initialize" => {
                    json!({"protocolVersion": MCP_PROTOCOL_VERSION, "capabilities": {}, "serverInfo": {"name": "mock", "version": "0.1"}})
                }
                "tools/list" => {
                    json!({"tools": [{"name":"read_issue", "inputSchema":{"type":"object"}, "description":"read issue"}]})
                }
                "tools/call" => json!({"content":[{"type":"text", "text":"mock result"}]}),
                _ => return Err(McpError::Transport("unexpected request".into())),
            };
            Ok(json!({"jsonrpc":"2.0", "id":payload["id"], "result":result}))
        }

        async fn notify(&self, payload: &Value) -> Result<(), McpError> {
            self.calls
                .lock()
                .unwrap()
                .push(payload["method"].as_str().unwrap().to_owned());
            Ok(())
        }
    }

    struct Approval(bool);
    #[async_trait]
    impl McpApprovalGate for Approval {
        async fn approve(&self, _tool: &str, _arguments: &Value) -> bool {
            self.0
        }
    }

    #[tokio::test]
    async fn handshake_and_list_validate_server_tools() {
        let client = McpClient::new(
            FakeTransport {
                calls: Mutex::new(Vec::new()),
            },
            Arc::new(Approval(false)),
        );
        assert!(matches!(
            client.list_tools().await,
            Err(McpError::NotInitialized)
        ));
        client.initialize().await.unwrap();
        let tools = client.list_tools().await.unwrap();
        assert_eq!(tools.len(), 1);
        assert_eq!(tools[0].name, "read_issue");
    }

    #[tokio::test]
    async fn toolbox_requires_allowlist_and_caps_calls() {
        let transport = FakeTransport {
            calls: Mutex::new(Vec::new()),
        };
        let client = McpClient::new(transport, Arc::new(Approval(true)));
        let toolbox =
            McpToolbox::connect(client, McpToolPolicy::new(["read_issue"], 1), None, None)
                .await
                .unwrap();
        assert_eq!(toolbox.definitions().len(), 1);
        assert!(
            toolbox
                .call("read_issue", json!({"id": 1}), None)
                .await
                .is_ok()
        );
        assert_eq!(toolbox.remaining_calls(), 0);
        assert!(matches!(
            toolbox.call("read_issue", json!({"id": 2}), None).await,
            Err(McpError::BudgetExceeded(_))
        ));
        assert!(matches!(
            toolbox.call("other", json!({}), None).await,
            Err(McpError::ToolNotAllowed(_))
        ));

        let denied = McpToolbox::connect(
            McpClient::new(
                FakeTransport {
                    calls: Mutex::new(Vec::new()),
                },
                Arc::new(Approval(true)),
            ),
            McpToolPolicy::new(["not_advertised"], 2),
            None,
            None,
        )
        .await;
        assert!(matches!(denied, Err(McpError::ToolNotAllowed(_))));
    }

    #[tokio::test]
    async fn toolbox_checks_the_shared_run_budget_before_calling() {
        let ledger = UsageLedger::new(Some(ryoko_models::Budget::new(0.0)));
        let toolbox = McpToolbox::connect(
            McpClient::new(
                FakeTransport {
                    calls: Mutex::new(Vec::new()),
                },
                Arc::new(Approval(true)),
            ),
            McpToolPolicy::new(["read_issue"], 3),
            Some(ledger),
            Some("task-a".into()),
        )
        .await
        .unwrap();
        assert!(matches!(
            toolbox.call("read_issue", json!({"id": 1}), None).await,
            Err(McpError::BudgetExceeded(_))
        ));
        assert_eq!(toolbox.remaining_calls(), 3);
    }

    #[tokio::test]
    async fn toolbox_enforces_the_owning_tasks_budget_scope() {
        let ledger = UsageLedger::new(Some(
            ryoko_models::Budget::unlimited().with_task_token_limit(1),
        ));
        ledger.record_for(
            Some("task-a"),
            "mock",
            "mock",
            ryoko_models::Usage::new(2, 0),
            None,
        );
        let toolbox = McpToolbox::connect(
            McpClient::new(
                FakeTransport {
                    calls: Mutex::new(Vec::new()),
                },
                Arc::new(Approval(true)),
            ),
            McpToolPolicy::new(["read_issue"], 3),
            Some(ledger),
            None,
        )
        .await
        .unwrap();
        assert!(matches!(
            toolbox
                .call("read_issue", json!({"id": 1}), Some("task-a"))
                .await,
            Err(McpError::BudgetExceeded(_))
        ));
        assert!(
            toolbox
                .call("read_issue", json!({"id": 2}), Some("task-b"))
                .await
                .is_ok()
        );
    }

    #[tokio::test]
    async fn every_call_needs_host_approval() {
        let client = McpClient::new(
            FakeTransport {
                calls: Mutex::new(Vec::new()),
            },
            Arc::new(Approval(false)),
        );
        client.initialize().await.unwrap();
        assert!(matches!(
            client.call_tool("read_issue", json!({"id": 1})).await,
            Err(McpError::PermissionDenied)
        ));
        assert!(
            !client
                .transport
                .calls
                .lock()
                .unwrap()
                .iter()
                .any(|method| method == "tools/call")
        );

        let client = McpClient::new(
            FakeTransport {
                calls: Mutex::new(Vec::new()),
            },
            Arc::new(Approval(true)),
        );
        client.initialize().await.unwrap();
        assert!(
            client
                .call_tool("read_issue", json!({"id": 2}))
                .await
                .is_ok()
        );
        assert_eq!(
            client
                .transport
                .calls
                .lock()
                .unwrap()
                .iter()
                .filter(|method| *method == "tools/call")
                .count(),
            1
        );
    }

    /// Asker that replays a scripted answer list and records what it was asked.
    struct ScriptedAsker {
        answers: Mutex<VecDeque<McpApprovalScope>>,
        asked: Mutex<Vec<String>>,
    }

    impl ScriptedAsker {
        fn new(answers: Vec<McpApprovalScope>) -> Self {
            Self {
                answers: Mutex::new(answers.into()),
                asked: Mutex::new(Vec::new()),
            }
        }

        fn asked(&self) -> Vec<String> {
            self.asked.lock().unwrap().clone()
        }
    }

    #[async_trait]
    impl McpApprovalAsker for ScriptedAsker {
        async fn ask(&self, tool: &str, _arguments: &Value) -> McpApprovalScope {
            self.asked.lock().unwrap().push(tool.to_owned());
            self.answers
                .lock()
                .unwrap()
                .pop_front()
                .unwrap_or(McpApprovalScope::Deny)
        }
    }

    #[tokio::test]
    async fn a_once_scope_is_not_remembered() {
        let asker = Arc::new(ScriptedAsker::new(vec![
            McpApprovalScope::Once,
            McpApprovalScope::Once,
        ]));
        let gate = SessionApprovalGate::new(asker.clone());
        assert!(gate.approve("list_issues", &json!({})).await);
        assert!(gate.approve("list_issues", &json!({})).await);
        assert_eq!(asker.asked().len(), 2, "a once-only answer must re-ask");
        assert!(gate.grants().is_empty());
    }

    #[tokio::test]
    async fn a_run_scope_applies_to_that_tool_only() {
        let asker = Arc::new(ScriptedAsker::new(vec![
            McpApprovalScope::ForRun,
            McpApprovalScope::Once,
        ]));
        let gate = SessionApprovalGate::new(asker.clone());
        assert!(gate.approve("list_issues", &json!({})).await);
        assert!(gate.approve("list_issues", &json!({})).await);
        assert_eq!(
            asker.asked(),
            vec!["list_issues"],
            "the grant answers later calls"
        );

        // A different tool still asks, so approving one tool never widens the
        // scope to the rest of the catalogue.
        assert!(gate.approve("delete_issue", &json!({})).await);
        assert_eq!(asker.asked(), vec!["list_issues", "delete_issue"]);
        assert_eq!(gate.grants(), vec!["list_issues".to_owned()]);
        assert!(gate.decisions()[1].reused_grant);
    }

    #[tokio::test]
    async fn a_denial_is_sticky_and_never_re_asks() {
        let asker = Arc::new(ScriptedAsker::new(vec![
            McpApprovalScope::Deny,
            McpApprovalScope::ForRun,
        ]));
        let gate = SessionApprovalGate::new(asker.clone());
        assert!(!gate.approve("delete_issue", &json!({})).await);
        assert!(!gate.approve("delete_issue", &json!({})).await);
        assert_eq!(
            asker.asked(),
            vec!["delete_issue"],
            "a refusal is not re-asked"
        );
        assert!(gate.grants().is_empty());
        assert!(!gate.decisions()[1].allowed);
    }

    #[tokio::test]
    async fn revoking_grants_forgets_allowances_but_not_refusals() {
        let asker = Arc::new(ScriptedAsker::new(vec![
            McpApprovalScope::ForRun,
            McpApprovalScope::Deny,
            McpApprovalScope::Once,
            McpApprovalScope::Deny,
        ]));
        let gate = SessionApprovalGate::new(asker.clone());
        assert!(gate.approve("list_issues", &json!({})).await);
        assert!(!gate.approve("delete_issue", &json!({})).await);
        gate.revoke_grants();
        assert!(gate.grants().is_empty());
        assert!(gate.approve("list_issues", &json!({})).await, "asks again");
        assert!(!gate.approve("delete_issue", &json!({})).await);
        assert_eq!(
            asker.asked(),
            vec!["list_issues", "delete_issue", "list_issues"]
        );
    }

    #[tokio::test]
    async fn decisions_and_grants_never_record_tool_arguments() {
        let asker = Arc::new(ScriptedAsker::new(vec![McpApprovalScope::ForRun]));
        let gate = SessionApprovalGate::new(asker);
        gate.approve("list_issues", &json!({"token": "ghp_secret_value"}))
            .await;
        let logged = serde_json::to_string(&gate.decisions()).unwrap();
        assert!(!logged.contains("ghp_secret_value"), "{logged}");
        assert!(logged.contains("list_issues"));
    }

    #[tokio::test]
    async fn a_blank_tool_name_is_refused_without_asking() {
        let asker = Arc::new(ScriptedAsker::new(vec![McpApprovalScope::ForRun]));
        let gate = SessionApprovalGate::new(asker.clone());
        assert!(!gate.approve("   ", &json!({})).await);
        assert!(asker.asked().is_empty());
    }

    #[tokio::test]
    async fn a_session_gate_drives_the_client_approval_boundary() {
        let asker = Arc::new(ScriptedAsker::new(vec![McpApprovalScope::ForRun]));
        let gate = Arc::new(SessionApprovalGate::new(asker));
        let client = McpClient::new(
            FakeTransport {
                calls: Mutex::new(Vec::new()),
            },
            gate.clone(),
        );
        client.initialize().await.unwrap();
        assert!(
            client
                .call_tool("read_issue", json!({"id": 1}))
                .await
                .is_ok()
        );
        assert!(
            client
                .call_tool("read_issue", json!({"id": 2}))
                .await
                .is_ok()
        );
        let calls = client
            .transport
            .calls
            .lock()
            .unwrap()
            .iter()
            .filter(|method| *method == "tools/call")
            .count();
        assert_eq!(
            calls, 2,
            "the run scope still gates every call through the gate"
        );
    }
}
