//! Operator-controlled MCP stdio transport for the CLI.
//! External server processes never inherit the complete Ryoko environment.
use std::path::Path;
use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use ryoko_core::mcp::{McpApprovalGate, McpClient, McpError, McpTransport};
use serde_json::Value;
use tokio::io::{AsyncBufRead, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, ChildStdout, Command};
use tokio::sync::Mutex;

const MAX_FRAME_BYTES: usize = 1024 * 1024;
const MAX_REQUEST_BYTES: usize = 64 * 1024;
const MCP_TIMEOUT: Duration = Duration::from_secs(15);

struct McpProcess {
    _child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
}

pub struct StdioMcpTransport {
    inner: Mutex<McpProcess>,
}

impl StdioMcpTransport {
    pub async fn spawn(
        executable: &Path,
        args: &[String],
        pass_env: &[String],
    ) -> Result<Self, McpError> {
        if args.len() > 64 || pass_env.len() > 32 {
            return Err(McpError::Transport(
                "server argument/environment limit exceeded".into(),
            ));
        }
        let mut command = Command::new(executable);
        command.args(args);
        // PATH enables explicitly chosen interpreter/Node/Python wrappers.
        // No inherited API keys, tokens or model credentials by default.
        command.env_clear();
        for name in ["PATH", "HOME", "TMPDIR", "TEMP", "USERPROFILE"] {
            if let Some(value) = std::env::var_os(name) {
                command.env(name, value);
            }
        }
        for name in pass_env {
            if !valid_env_name(name) {
                return Err(McpError::Transport("invalid --allow-env name".into()));
            }
            let value = std::env::var_os(name).ok_or_else(|| {
                McpError::Transport(format!("environment variable {name} not set"))
            })?;
            command.env(name, value);
        }
        command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .kill_on_drop(true);
        let mut child = command
            .spawn()
            .map_err(|error| McpError::Transport(error.to_string()))?;
        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| McpError::Transport("server stdin unavailable".into()))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| McpError::Transport("server stdout unavailable".into()))?;
        Ok(Self {
            inner: Mutex::new(McpProcess {
                _child: child,
                stdin,
                stdout: BufReader::new(stdout),
            }),
        })
    }
}

fn valid_env_name(name: &str) -> bool {
    let mut bytes = name.bytes();
    matches!(bytes.next(), Some(b'A'..=b'Z' | b'_'))
        && bytes.all(|b| b.is_ascii_uppercase() || b.is_ascii_digit() || b == b'_')
}

/// Consume at most 1 MiB for one newline-terminated JSON-RPC frame. Unlike
/// read_line, a malicious server cannot force unbounded memory allocation.
async fn read_frame(reader: &mut BufReader<ChildStdout>) -> Result<Value, McpError> {
    let mut bytes = Vec::new();
    loop {
        let available = reader
            .fill_buf()
            .await
            .map_err(|error| McpError::Transport(error.to_string()))?;
        if available.is_empty() {
            return Err(McpError::Transport("MCP server closed stdout".into()));
        }
        let take = available
            .iter()
            .position(|&byte| byte == b'\n')
            .map_or(available.len(), |index| index + 1);
        if bytes.len().saturating_add(take) > MAX_FRAME_BYTES {
            return Err(McpError::InvalidResponse("MCP frame size exceeded".into()));
        }
        let ended = available[take - 1] == b'\n';
        bytes.extend_from_slice(&available[..take]);
        reader.consume(take);
        if ended {
            break;
        }
    }
    serde_json::from_slice(&bytes)
        .map_err(|_| McpError::InvalidResponse("invalid server JSON".into()))
}

async fn write_payload(state: &mut McpProcess, payload: &Value) -> Result<(), McpError> {
    let mut bytes =
        serde_json::to_vec(payload).map_err(|error| McpError::Transport(error.to_string()))?;
    if bytes.len() > MAX_REQUEST_BYTES {
        return Err(McpError::InvalidResponse(
            "MCP request exceeds 64 KiB".into(),
        ));
    }
    bytes.push(b'\n');
    state
        .stdin
        .write_all(&bytes)
        .await
        .map_err(|error| McpError::Transport(error.to_string()))?;
    state
        .stdin
        .flush()
        .await
        .map_err(|error| McpError::Transport(error.to_string()))
}

#[async_trait]
impl McpTransport for StdioMcpTransport {
    async fn request(&self, payload: &Value) -> Result<Value, McpError> {
        let mut state = self.inner.lock().await;
        tokio::time::timeout(MCP_TIMEOUT, async {
            write_payload(&mut state, payload).await?;
            for _ in 0..32 {
                let reply = read_frame(&mut state.stdout).await?;
                // MCP server notifications are not user approvals.
                if reply.get("id").is_some() {
                    return Ok(reply);
                }
            }
            Err(McpError::InvalidResponse(
                "too many server notifications".into(),
            ))
        })
        .await
        .map_err(|_| McpError::Transport("MCP server timed out".into()))?
    }

    async fn notify(&self, payload: &Value) -> Result<(), McpError> {
        let mut state = self.inner.lock().await;
        tokio::time::timeout(MCP_TIMEOUT, write_payload(&mut state, payload))
            .await
            .map_err(|_| McpError::Transport("MCP notification timed out".into()))?
    }
}

/// Every tool call requires a fresh, exact typed human response.
pub struct TerminalApproval;

#[async_trait]
impl McpApprovalGate for TerminalApproval {
    async fn approve(&self, tool: &str, arguments: &Value) -> bool {
        eprintln!("MCP tool: {tool}");
        eprintln!("Arguments: {arguments}");
        eprintln!("Allow this tool invocation once? Type YES to approve: ");
        let mut answer = String::new();
        let input = tokio::io::stdin();
        let mut reader = BufReader::new(input);
        match tokio::time::timeout(
            Duration::from_secs(90),
            tokio::io::AsyncBufReadExt::read_line(&mut reader, &mut answer),
        )
        .await
        {
            Ok(Ok(_)) => answer.trim() == "YES",
            _ => false,
        }
    }
}

pub async fn tools(server: &Path, args: &[String], pass_env: &[String]) -> Result<(), McpError> {
    let transport = StdioMcpTransport::spawn(server, args, pass_env).await?;
    let client = McpClient::new(transport, Arc::new(TerminalApproval));
    client.initialize().await?;
    let tools = client.list_tools().await?;
    println!(
        "{}",
        serde_json::to_string_pretty(&tools)
            .map_err(|error| McpError::Transport(error.to_string()))?
    );
    Ok(())
}

pub async fn call(
    server: &Path,
    args: &[String],
    pass_env: &[String],
    tool: &str,
    arguments: &str,
) -> Result<(), McpError> {
    let arguments: Value = serde_json::from_str(arguments)
        .map_err(|_| McpError::InvalidResponse("tool arguments must be valid JSON".into()))?;
    if !arguments.is_object() {
        return Err(McpError::InvalidResponse(
            "tool arguments must be a JSON object".into(),
        ));
    }
    let transport = StdioMcpTransport::spawn(server, args, pass_env).await?;
    let client = McpClient::new(transport, Arc::new(TerminalApproval));
    client.initialize().await?;
    let available = client.list_tools().await?;
    if !available.iter().any(|entry| entry.name == tool) {
        return Err(McpError::InvalidResponse(
            "tool not advertised by the MCP server".into(),
        ));
    }
    let result = client.call_tool(tool, arguments).await?;
    println!(
        "{}",
        serde_json::to_string_pretty(&result)
            .map_err(|error| McpError::Transport(error.to_string()))?
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_invalid_environment_names() {
        assert!(valid_env_name("GITHUB_TOKEN"));
        assert!(!valid_env_name("PATH=bad"));
        assert!(!valid_env_name("lowercase"));
        assert!(!valid_env_name(""));
    }
}
