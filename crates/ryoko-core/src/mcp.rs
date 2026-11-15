//! Transport-neutral MCP JSON-RPC client for host-approved tool calls.
//!
//! The core owns protocol validation and a mandatory human approval boundary.
//! Transports and UI prompts are supplied by a trusted host (CLI/desktop).
//! Tool output is untrusted data, never interpreted as agent instructions.
use std::collections::HashSet;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

const MCP_PROTOCOL_VERSION: &str = "2025-06-18";
const MAX_TOOLS: usize = 256;
const MAX_PAGES: usize = 16;
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

/// JSON-RPC MCP client. Use a trusted transport and approval gate; do not
/// allow model-generated configuration to choose a server executable.
pub struct McpClient<T: McpTransport> {
    transport: T,
    approval: Arc<dyn McpApprovalGate>,
    next_id: AtomicU64,
    initialized: std::sync::atomic::AtomicBool,
}

impl<T: McpTransport> McpClient<T> {
    pub fn new(transport: T, approval: Arc<dyn McpApprovalGate>) -> Self {
        Self {
            transport,
            approval,
            next_id: AtomicU64::new(1),
            initialized: std::sync::atomic::AtomicBool::new(false),
        }
    }

    async fn request(&self, method: &str, params: Value) -> Result<Value, McpError> {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let payload = json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": method,
            "params": params
        });
        let response = self.transport.request(&payload).await?;
        if response["jsonrpc"] != "2.0" || response["id"].as_u64() != Some(id) {
            return Err(McpError::InvalidResponse("JSON-RPC version or response ID mismatch".into()));
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
            return Err(McpError::InvalidResponse("result exceeds 1 MiB limit".into()));
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
            return Err(McpError::UnsupportedVersion(version.chars().take(80).collect()));
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
            let tools = response["tools"]
                .as_array()
                .ok_or_else(|| McpError::InvalidResponse("tools/list missing tools array".into()))?;
            if result.len().saturating_add(tools.len()) > MAX_TOOLS {
                return Err(McpError::InvalidResponse("tool catalogue exceeds limit".into()));
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
                    return Err(McpError::InvalidResponse("invalid or duplicate MCP tool".into()));
                }
                result.push(tool);
            }
            match response.get("nextCursor").and_then(Value::as_str) {
                Some(next) if !next.is_empty() => {
                    if !seen.insert(next.to_owned()) {
                        return Err(McpError::InvalidResponse("cyclic MCP pagination cursor".into()));
                    }
                    cursor = Some(next.to_owned());
                }
                _ => return Ok(result),
            }
        }
        Err(McpError::InvalidResponse("MCP pagination limit exceeded".into()))
    }

    pub async fn call_tool(&self, tool: &str, arguments: Value) -> Result<Value, McpError> {
        self.require_initialized()?;
        if tool.is_empty()
            || tool.len() > 128
            || tool.chars().any(char::is_control)
            || !arguments.is_object()
        {
            return Err(McpError::InvalidResponse("invalid tool name or arguments".into()));
        }
        // Approval is never inferred from successful initialization, discovery,
        // model choice or a previously approved call.
        if !self.approval.approve(tool, &arguments).await {
            return Err(McpError::PermissionDenied);
        }
        self.request("tools/call", json!({"name": tool, "arguments": arguments})).await
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
                "initialize" => json!({"protocolVersion": MCP_PROTOCOL_VERSION, "capabilities": {}, "serverInfo": {"name": "mock", "version": "0.1"}}),
                "tools/list" => json!({"tools": [{"name":"read_issue", "inputSchema":{"type":"object"}, "description":"read issue"}]}),
                "tools/call" => json!({"content":[{"type":"text", "text":"mock result"}]}),
                _ => return Err(McpError::Transport("unexpected request".into())),
            };
            Ok(json!({"jsonrpc":"2.0", "id":payload["id"], "result":result}))
        }

        async fn notify(&self, payload: &Value) -> Result<(), McpError> {
            self.calls.lock().unwrap().push(payload["method"].as_str().unwrap().to_owned());
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
            FakeTransport { calls: Mutex::new(Vec::new()) },
            Arc::new(Approval(false)),
        );
        assert!(matches!(client.list_tools().await, Err(McpError::NotInitialized)));
        client.initialize().await.unwrap();
        let tools = client.list_tools().await.unwrap();
        assert_eq!(tools.len(), 1);
        assert_eq!(tools[0].name, "read_issue");
    }

    #[tokio::test]
    async fn every_call_needs_host_approval() {
        let client = McpClient::new(
            FakeTransport { calls: Mutex::new(Vec::new()) },
            Arc::new(Approval(false)),
        );
        client.initialize().await.unwrap();
        assert!(matches!(client.call_tool("read_issue", json!({"id": 1})).await, Err(McpError::PermissionDenied)));
        assert!(!client.transport.calls.lock().unwrap().iter().any(|method| method == "tools/call"));

        let client = McpClient::new(
            FakeTransport { calls: Mutex::new(Vec::new()) },
            Arc::new(Approval(true)),
        );
        client.initialize().await.unwrap();
        assert!(client.call_tool("read_issue", json!({"id": 2})).await.is_ok());
        assert_eq!(client.transport.calls.lock().unwrap().iter().filter(|method| *method == "tools/call").count(), 1);
    }
}
