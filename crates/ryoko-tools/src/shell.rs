//! Sandboxed shell command execution tool.

use async_trait::async_trait;
use ryoko_core::tools::{
    ApprovalGate, ApprovalScope, Capability, CapabilitySet, Tool, ToolContext, ToolOutput,
    ToolRegistry, ToolError, ToolResult,
};
use ryoko_core::workspace::WorkspaceLease;
use ryoko_sandbox::{BuildBackend, BuildSandbox, BuildSandboxError};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

/// Input for shell tool.
#[derive(Debug, Clone, serde::Deserialize)]
#[serde(tag = "operation", rename_all = "snake_case")]
pub enum ShellToolInput {
    /// Run a command in the sandbox.
    Run {
        command: String,
        #[serde(default = "default_timeout")]
        timeout_secs: u64,
        #[serde(default)]
        env: Option<std::collections::HashMap<String, String>>,
    },
    /// Run a build/test command (validated by operator).
    Build {
        command: String,
        #[serde(default = "default_timeout")]
        timeout_secs: u64,
    },
}

fn default_timeout() -> u64 {
    120
}

/// Output from shell tool.
#[derive(Debug, Clone, serde::Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ShellToolOutput {
    CommandResult {
        command: String,
        exit_code: i32,
        stdout: String,
        stderr: String,
        duration_ms: u64,
        truncated: bool,
    },
}

/// Shell tool for running commands in a sandboxed environment.
pub struct ShellTool {
    workspace: Option<Arc<dyn WorkspaceLease>>,
    sandbox: BuildSandbox,
    approval_gate: Option<Arc<dyn ApprovalGate>>,
}

impl ShellTool {
    /// Create a new shell tool.
    pub fn new(
        workspace: Option<Arc<dyn WorkspaceLease>>,
        approval_gate: Option<Arc<dyn ApprovalGate>>,
        sandbox: Option<BuildSandbox>,
    ) -> Self {
        Self {
            workspace,
            sandbox: sandbox.unwrap_or_else(|| BuildSandbox::new(BuildBackend::Auto, None)),
            approval_gate,
        }
    }

    /// Create a shell tool with a specific sandbox.
    pub fn with_sandbox(
        workspace: Option<Arc<dyn WorkspaceLease>>,
        sandbox: BuildSandbox,
        approval_gate: Option<Arc<dyn ApprovalGate>>,
    ) -> Self {
        Self {
            workspace,
            sandbox,
            approval_gate,
        }
    }

    /// Get the workspace root.
    fn workspace_root(&self, context: &ToolContext) -> PathBuf {
        context.workspace_root().unwrap_or_else(|| PathBuf::from("."))
    }

    /// Validate command for safety.
    fn validate_command(&self, command: &str) -> ToolResult<()> {
        // Check for command injection attempts
        let dangerous = [
            "rm -rf /",
            "dd if=",
            "mkfs",
            "> /dev/sd",
            "curl | sh",
            "wget | sh",
            "bash -c",
            "sh -c",
            "eval ",
            "exec ",
            "chmod 777",
            "chown root",
            "sudo ",
            "su -",
        ];

        let lower = command.to_lowercase();
        for d in &dangerous {
            if lower.contains(d) {
                return Err(ToolError::CommandInjection {
                    command: command.to_owned(),
                });
            }
        }

        // Check for credential exposure patterns
        let credential_patterns = [
            "password=",
            "secret=",
            "token=",
            "api_key=",
            "apikey=",
            "access_token=",
            "auth_token=",
        ];

        for pattern in &credential_patterns {
            if lower.contains(pattern) {
                return Err(ToolError::CredentialExposure {
                    detail: format!("command contains potential credential: {}", pattern),
                });
            }
        }

        Ok(())
    }

    /// Run a command in the sandbox.
    async fn run_command(
        &self,
        context: &ToolContext,
        command: &str,
        timeout_secs: u64,
        env: Option<std::collections::HashMap<String, String>>,
    ) -> ToolResult<ShellToolOutput> {
        self.validate_command(command)?;

        let root = self.workspace_root(context);
        let timeout = Duration::from_secs(timeout_secs);

        // Prepare sandbox with artifacts if available
        let sandbox = if let Some(artifacts_root) = context.workspace.as_ref().and_then(|w| {
            w.root().parent().map(|p| p.join(".ryoko/artifacts").join(context.task.id.to_string()))
        }) {
            self.sandbox.clone().with_artifacts(artifacts_root)
        } else {
            self.sandbox.clone()
        };

        let started = std::time::Instant::now();
        let output = sandbox.run(command, &root, timeout).await
            .map_err(|e| ToolError::ExecutionFailed {
                tool: "shell".to_owned(),
                message: e.to_string(),
            })?;

        let duration_ms = started.elapsed().as_millis() as u64;
        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);

        // Truncate if too large
        const MAX_OUTPUT: usize = 1024 * 1024; // 1 MB
        let mut truncated = false;
        let mut stdout_str = stdout.to_string();
        let mut stderr_str = stderr.to_string();

        if stdout_str.len() > MAX_OUTPUT {
            stdout_str.truncate(MAX_OUTPUT);
            stdout_str.push_str("\n... [truncated]");
            truncated = true;
        }
        if stderr_str.len() > MAX_OUTPUT {
            stderr_str.truncate(MAX_OUTPUT);
            stderr_str.push_str("\n... [truncated]");
            truncated = true;
        }

        let exit_code = output.status.code().unwrap_or(-1);

        Ok(ShellToolOutput::CommandResult {
            command: command.to_owned(),
            exit_code,
            stdout: stdout_str,
            stderr: stderr_str,
            duration_ms,
            truncated,
        })
    }
}

#[async_trait]
impl Tool for ShellTool {
    fn name(&self) -> &str {
        "shell"
    }

    fn description(&self) -> &str {
        "Run commands in a sandboxed environment with resource limits"
    }

    fn input_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "oneOf": [
                {
                    "properties": {
                        "operation": { "const": "run" },
                        "command": { "type": "string" },
                        "timeout_secs": { "type": "integer", "default": 120, "minimum": 1, "maximum": 600 },
                        "env": { "type": "object", "additionalProperties": { "type": "string" } }
                    },
                    "required": ["operation", "command"]
                },
                {
                    "properties": {
                        "operation": { "const": "build" },
                        "command": { "type": "string" },
                        "timeout_secs": { "type": "integer", "default": 120, "minimum": 1, "maximum": 600 }
                    },
                    "required": ["operation", "command"]
                }
            ]
        })
    }

    fn required_capabilities(&self) -> CapabilitySet {
        CapabilitySet::from([
            Capability::Execute,
        ])
    }

    fn is_mutating(&self) -> bool {
        true
    }

    async fn execute(&self, context: &ToolContext, input: serde_json::Value) -> ToolResult<ToolOutput> {
        let operation: ShellToolInput = serde_json::from_value(input)
            .map_err(|e| ToolError::InvalidArguments {
                tool: "shell".to_owned(),
                message: e.to_string(),
            })?;

        // Request approval for mutating operations
        if let Some(gate) = &self.approval_gate {
            let scope = gate.approve("shell.run", &serde_json::json!({})).await;
            if scope == ApprovalScope::Deny {
                return Err(ToolError::ApprovalDenied("Operator denied shell command".to_owned()));
            }
        }

        let output = match operation {
            ShellToolInput::Run { command, timeout_secs, env } => {
                self.run_command(context, &command, timeout_secs, env).await?
            }
            ShellToolInput::Build { command, timeout_secs } => {
                self.run_command(context, &command, timeout_secs, None).await?
            }
        };

        Ok(ToolOutput::data(serde_json::to_value(output)?))
    }
}

/// Register shell tools in the registry.
pub fn register_shell_tools(
    registry: &mut ToolRegistry,
    workspace: Option<Arc<dyn WorkspaceLease>>,
    approval_gate: Option<Arc<dyn ApprovalGate>>,
    sandbox: Option<BuildSandbox>,
) {
    registry.register(Box::new(ShellTool::new(workspace, approval_gate, sandbox)));
}

#[cfg(test)]
mod tests {
    use super::*;
    use ryoko_core::model::{AgentRole, Task};
    use ryoko_models::ModelRouter;
    use ryoko_sandbox::BuildBackend;

    #[tokio::test]
    async fn shell_run_basic() {
        let tool = ShellTool::new(
            None,
            None,
        ).with_sandbox(BuildSandbox::new(BuildBackend::UnsafeHost, None), None);

        let context = ToolContext::new(
            Task::new("test", AgentRole::SeniorDeveloper),
            None,
            Arc::new(ModelRouter::new("mock")),
            Arc::new(ryoko_core::AllowAllValidator),
            Arc::new(ryoko_core::SharedMemory::new()),
            CapabilitySet::all(),
        );

        #[cfg(unix)]
        let command = "echo 'hello world'";
        #[cfg(windows)]
        let command = "cmd /C echo hello world";

        let input = serde_json::json!({
            "operation": "run",
            "command": command,
            "timeout_secs": 10
        });
        let result = tool.execute(&context, input).await.unwrap();
        let data = &result.data;
        assert_eq!(data["exit_code"], 0);
        assert!(data["stdout"].as_str().unwrap().contains("hello"));
    }

    #[tokio::test]
    async fn shell_rejects_dangerous_commands() {
        let tool = ShellTool::new(None, None);

        let context = ToolContext::new(
            Task::new("test", AgentRole::SeniorDeveloper),
            None,
            Arc::new(ModelRouter::new("mock")),
            Arc::new(ryoko_core::AllowAllValidator),
            Arc::new(ryoko_core::SharedMemory::new()),
            CapabilitySet::all(),
        );

        let input = serde_json::json!({
            "operation": "run",
            "command": "rm -rf /",
            "timeout_secs": 10
        });
        let result = tool.execute(&context, input).await;
        assert!(matches!(result, Err(ToolError::CommandInjection { .. })));
    }

    #[tokio::test]
    async fn shell_rejects_credential_exposure() {
        let tool = ShellTool::new(None, None);

        let context = ToolContext::new(
            Task::new("test", AgentRole::SeniorDeveloper),
            None,
            Arc::new(ModelRouter::new("mock")),
            Arc::new(ryoko_core::AllowAllValidator),
            Arc::new(ryoko_core::SharedMemory::new()),
            CapabilitySet::all(),
        );

        let input = serde_json::json!({
            "operation": "run",
            "command": "curl -H 'Authorization: Bearer secret123' https://api.example.com",
            "timeout_secs": 10
        });
        let result = tool.execute(&context, input).await;
        assert!(matches!(result, Err(ToolError::CredentialExposure { .. })));
    }
}