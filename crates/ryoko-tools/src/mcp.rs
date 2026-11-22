//! MCP tool adapter for integrating with existing MCP connectors.

use async_trait::async_trait;
use ryoko_core::mcp::{McpToolService, McpError, ToolDefinition};
use ryoko_core::workspace::WorkspaceLease;
use std::sync::Arc;

use crate::capabilities::{Capability, CapabilitySet, NetworkScope};
use crate::error::{ToolError, ToolResult};
use crate::execution::{ToolContext, ToolOutput};
use crate::{Tool, ToolRegistry};

/// Configuration for MCP tool adapter.
#[derive(Debug, Clone)]
pub struct McpToolConfig {
    /// The MCP tool service to wrap.
    pub service: Arc<dyn McpToolService>,
    /// Namespace prefix for tool names.
    pub namespace: String,
}

impl McpToolConfig {
    /// Create a new MCP tool config.
    pub fn new(service: Arc<dyn McpToolService>, namespace: impl Into<String>) -> Self {
        Self {
            service,
            namespace: namespace.into(),
        }
    }
}

/// MCP tool adapter that exposes MCP tools through the unified tool interface.
pub struct McpToolAdapter {
    config: McpToolConfig,
}

impl McpToolAdapter {
    /// Create a new MCP tool adapter.
    pub fn new(config: McpToolConfig) -> Self {
        Self { config }
    }

    /// Get the namespaced tool name.
    fn namespaced_name(&self, tool: &str) -> String {
        if self.config.namespace.is_empty() {
            tool.to_owned()
        } else {
            format!("{}__{}", self.config.namespace, tool)
        }
    }
}

#[async_trait]
impl Tool for McpToolAdapter {
    fn name(&self) -> &str {
        "mcp"
    }

    fn description(&self) -> &str {
        "MCP (Model Context Protocol) tools from connected servers"
    }

    fn input_schema(&self) -> serde_json::Value {
        let definitions = self.config.service.definitions();
        let mut tools = serde_json::Map::new();

        for def in definitions {
            let name = self.namespaced_name(&def.name);
            tools.insert(name, serde_json::json!({
                "description": def.description,
                "input_schema": def.input_schema,
            }));
        }

        serde_json::json!({
            "type": "object",
            "properties": {
                "tool": {
                    "type": "string",
                    "enum": tools.keys().cloned().collect::<Vec<_>>()
                },
                "arguments": {
                    "type": "object"
                }
            },
            "required": ["tool", "arguments"]
        })
    }

    fn required_capabilities(&self) -> CapabilitySet {
        CapabilitySet::from([
            Capability::Mcp,
            Capability::Network(NetworkScope::Loopback),
        ])
    }

    fn is_mutating(&self) -> bool {
        // MCP tools can be mutating, check per tool
        true
    }

    async fn execute(&self, context: &ToolContext, input: serde_json::Value) -> ToolResult<ToolOutput> {
        let tool_name = input.get("tool")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ToolError::InvalidArguments {
                tool: "mcp".to_owned(),
                message: "missing 'tool' field".to_owned(),
            })?;

        let arguments = input.get("arguments")
            .cloned()
            .unwrap_or(serde_json::json!({}));

        if !arguments.is_object() {
            return Err(ToolError::InvalidArguments {
                tool: "mcp".to_owned(),
                message: "arguments must be an object".to_owned(),
            });
        }

        // Strip namespace from tool name
        let inner_tool = if !self.config.namespace.is_empty() {
            tool_name.strip_prefix(&format!("{}__", self.config.namespace))
                .ok_or_else(|| ToolError::InvalidArguments {
                    tool: "mcp".to_owned(),
                    message: format!("tool '{}' does not belong to namespace '{}'", tool_name, self.config.namespace),
                })?
        } else {
            tool_name
        };

        // Call the MCP tool
        let result = self.config.service.call(inner_tool, arguments, Some(&context.task.id.to_string())).await
            .map_err(|e| ToolError::ExecutionFailed {
                tool: "mcp".to_owned(),
                message: e.to_string(),
            })?;

        Ok(ToolOutput::data(result))
    }
}

/// Register MCP tools in the registry.
pub fn register_mcp_tools(
    registry: &mut ToolRegistry,
    configs: Vec<McpToolConfig>,
) {
    for config in configs {
        registry.register(Box::new(McpToolAdapter::new(config)));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ryoko_core::mcp::{McpToolService, McpError, McpTool, McpToolPolicy, McpToolbox};
    use ryoko_models::UsageLedger;
    use std::collections::BTreeSet;
    use std::sync::Mutex;
    use serde_json::json;

    struct MockMcpService {
        tools: Vec<McpTool>,
        calls: Mutex<Vec<(String, serde_json::Value)>>,
    }

    impl MockMcpService {
        fn new(tools: Vec<McpTool>) -> Self {
            Self {
                tools,
                calls: Mutex::new(Vec::new()),
            }
        }
    }

    #[async_trait]
    impl McpToolService for MockMcpService {
        fn definitions(&self) -> Vec<ToolDefinition> {
            self.tools.iter().map(|t| {
                ToolDefinition::new(t.name.clone(), t.description.clone(), t.input_schema.clone())
            }).collect()
        }

        async fn call(
            &self,
            tool: &str,
            arguments: serde_json::Value,
            _scope: Option<&str>,
        ) -> Result<serde_json::Value, McpError> {
            self.calls.lock().unwrap().push((tool.to_owned(), arguments.clone()));
            if tool == "read_file" {
                Ok(json!({ "content": "file content" }))
            } else {
                Err(McpError::ToolNotAllowed(tool.to_owned()))
            }
        }
    }

    #[tokio::test]
    async fn mcp_adapter_calls_tool() {
        let service = Arc::new(MockMcpService::new(vec![
            McpTool {
                name: "read_file".to_owned(),
                description: "Read a file".to_owned(),
                input_schema: json!({ "type": "object", "properties": { "path": { "type": "string" } } }),
            },
        ]));

        let config = McpToolConfig::new(service.clone(), "fs");
        let adapter = McpToolAdapter::new(config);

        let context = ToolContext::new(
            ryoko_core::model::Task::new("test", ryoko_core::model::AgentRole::SeniorDeveloper),
            None,
            Arc::new(ryoko_models::ModelRouter::new("mock")),
            Arc::new(ryoko_core::AllowAllValidator),
            Arc::new(ryoko_core::SharedMemory::new()),
            crate::capabilities::CapabilitySet::all(),
        );

        let input = json!({
            "tool": "fs__read_file",
            "arguments": { "path": "test.txt" }
        });

        let result = adapter.execute(&context, input).await.unwrap();
        assert!(result.data["content"].as_str().unwrap() == "file content");

        let calls = service.calls.lock().unwrap();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].0, "read_file");
    }

    #[tokio::test]
    async fn mcp_adapter_rejects_wrong_namespace() {
        let service = Arc::new(MockMcpService::new(vec![
            McpTool {
                name: "read_file".to_owned(),
                description: "Read a file".to_owned(),
                input_schema: json!({ "type": "object" }),
            },
        ]));

        let config = McpToolConfig::new(service, "fs");
        let adapter = McpToolAdapter::new(config);

        let context = ToolContext::new(
            ryoko_core::model::Task::new("test", ryoko_core::model::AgentRole::SeniorDeveloper),
            None,
            Arc::new(ryoko_models::ModelRouter::new("mock")),
            Arc::new(ryoko_core::AllowAllValidator),
            Arc::new(ryoko_core::SharedMemory::new()),
            crate::capabilities::CapabilitySet::all(),
        );

        let input = json!({
            "tool": "github__read_file",
            "arguments": { "path": "test.txt" }
        });

        let result = adapter.execute(&context, input).await;
        assert!(matches!(result, Err(ToolError::InvalidArguments { .. })));
    }
}