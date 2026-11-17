//! Operator-controlled MCP stdio transport for the CLI.
//! External server processes never inherit the complete Ryoko environment.
use std::path::Path;
use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use ryoko_core::mcp::{
    McpApprovalGate, McpClient, McpError, McpToolPolicy, McpToolService, McpToolbox, McpTransport,
};
use ryoko_models::UsageLedger;
use serde_json::Value;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
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
        let mut command = Command::from(command_for(executable, args, pass_env)?);
        command.kill_on_drop(true);
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

fn command_for(
    executable: &Path,
    args: &[String],
    pass_env: &[String],
) -> Result<std::process::Command, McpError> {
    if args.len() > 64 || pass_env.len() > 32 {
        return Err(McpError::Transport(
            "server argument/environment limit exceeded".into(),
        ));
    }
    let mut command = std::process::Command::new(executable);
    command.args(args);
    // PATH enables explicitly chosen interpreter wrappers. HOME and
    // USERPROFILE are deliberately not forwarded: they expose ~/.ssh, ~/.aws
    // and other per-user credentials to the MCP server.
    command.env_clear();
    for name in ["PATH", "TMPDIR", "TEMP"] {
        if let Some(value) = std::env::var_os(name) {
            command.env(name, value);
        }
    }
    for name in pass_env {
        if !valid_env_name(name) {
            return Err(McpError::Transport("invalid --allow-env name".into()));
        }
        let value = std::env::var_os(name)
            .ok_or_else(|| McpError::Transport(format!("environment variable {name} not set")))?;
        command.env(name, value);
    }
    command
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    Ok(command)
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

/// Builds the host-approved tool surface attached to a `run --team` session.
/// The allowlist is mandatory, every call still goes through the terminal gate,
/// and the run's shared ledger is checked before each call.
pub async fn connect_toolbox(
    server: &Path,
    args: &[String],
    pass_env: &[String],
    allow_tools: &[String],
    max_calls: u64,
    ledger: UsageLedger,
) -> Result<Arc<dyn McpToolService>, McpError> {
    if allow_tools.is_empty() {
        return Err(McpError::ToolNotAllowed(
            "pass at least one --mcp-allow-tool".into(),
        ));
    }
    let transport = StdioMcpTransport::spawn(server, args, pass_env).await?;
    let client = McpClient::new(transport, Arc::new(TerminalApproval))
        .with_allowed_tools(allow_tools.iter().cloned());
    let toolbox = McpToolbox::connect(
        client,
        McpToolPolicy::new(allow_tools.iter().cloned(), max_calls),
        Some(ledger),
        None,
    )
    .await?;
    Ok(Arc::new(toolbox))
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
    use ryoko_core::mcp::{McpClient, McpToolbox};
    use ryoko_models::{Budget, UsageLedger};
    use serde_json::json;
    use std::path::Path;
    use std::sync::{Arc, OnceLock};
    use tempfile::{TempDir, tempdir};

    /// A tiny MCP server compiled with the toolchain that is already running
    /// these tests. This avoids depending on python3, node or any other
    /// interpreter being installed on the test machine.
    const FAKE_SERVER: &str = r##"
use std::io::{self, BufRead, Write};

fn field<'a>(line: &'a str, key: &str) -> Option<&'a str> {
    let needle = format!("\"{key}\":");
    let start = line.find(&needle)? + needle.len();
    let rest = line[start..].trim_start();
    if let Some(rest) = rest.strip_prefix('"') {
        let end = rest.find('"')?;
        Some(&rest[..end])
    } else {
        let end = rest.find(|ch| ch == ',' || ch == '}').unwrap_or(rest.len());
        Some(rest[..end].trim())
    }
}

fn main() {
    let stdin = io::stdin();
    let mut stdout = io::stdout();
    for line in stdin.lock().lines() {
        let Ok(line) = line else { break };
        if line.trim().is_empty() {
            continue;
        }
        let method = field(&line, "method").unwrap_or("");
        if method == "notifications/initialized" {
            continue;
        }
        let id = field(&line, "id").unwrap_or("0");
        let response = match method {
            "initialize" => format!(
                "{{\"jsonrpc\":\"2.0\",\"id\":{id},\"result\":{INIT}}}"
            ),
            "tools/list" => format!(
                "{{\"jsonrpc\":\"2.0\",\"id\":{id},\"result\":{TOOLS}}}"
            ),
            "tools/call" => format!(
                "{{\"jsonrpc\":\"2.0\",\"id\":{id},\"result\":{CALL}}}"
            ),
            _ => format!(
                "{{\"jsonrpc\":\"2.0\",\"id\":{id},\"error\":{{\"code\":-32601,\"message\":\"method not found\"}}}}"
            ),
        };
        let _ = writeln!(stdout, "{response}");
        let _ = stdout.flush();
    }
}

const INIT: &str = r#"{"protocolVersion":"2025-06-18","capabilities":{},"serverInfo":{"name":"fake","version":"1"}}"#;
const TOOLS: &str = r#"{"tools":[{"name":"allowed_tool","description":"allowed","inputSchema":{"type":"object"}},{"name":"other_tool","description":"other","inputSchema":{"type":"object"}}]}"#;
const CALL: &str = r#"{"content":[{"type":"text","text":"ok"}]}"#;
"##;

    struct TestApproval(bool);
    #[async_trait::async_trait]
    impl McpApprovalGate for TestApproval {
        async fn approve(&self, _tool: &str, _arguments: &serde_json::Value) -> bool {
            self.0
        }
    }

    fn fake_server_binary() -> &'static Path {
        static SERVER: OnceLock<(TempDir, std::path::PathBuf)> = OnceLock::new();
        let (_, binary) = SERVER.get_or_init(|| {
            let dir = tempdir().expect("fake MCP server temp dir");
            let source = dir.path().join("fake_mcp_server.rs");
            std::fs::write(&source, FAKE_SERVER).expect("write fake MCP server source");
            let binary = dir.path().join(if cfg!(windows) {
                "fake_mcp_server.exe"
            } else {
                "fake_mcp_server"
            });
            let rustc = std::env::var_os("RUSTC").unwrap_or_else(|| "rustc".into());
            let output = std::process::Command::new(rustc)
                .arg("--edition=2021")
                .arg("-O")
                .arg(&source)
                .arg("-o")
                .arg(&binary)
                .output()
                .expect("rustc must be available to build the fake MCP server");
            assert!(
                output.status.success(),
                "could not build fake MCP server: {}",
                String::from_utf8_lossy(&output.stderr)
            );
            (dir, binary)
        });
        binary
    }

    #[test]
    fn stdio_command_does_not_forward_home_or_userprofile() {
        let command = command_for(Path::new("mcp-server"), &[], &[]).unwrap();
        let names: Vec<String> = command
            .get_envs()
            .map(|(name, _)| name.to_string_lossy().into_owned())
            .collect();
        assert!(
            !names.iter().any(|name| name.eq_ignore_ascii_case("HOME")),
            "HOME must not be forwarded: {names:?}"
        );
        assert!(
            !names
                .iter()
                .any(|name| name.eq_ignore_ascii_case("USERPROFILE")),
            "USERPROFILE must not be forwarded: {names:?}"
        );
    }

    #[test]
    fn rejects_invalid_environment_names() {
        assert!(valid_env_name("GITHUB_TOKEN"));
        assert!(!valid_env_name("PATH=bad"));
        assert!(!valid_env_name("lowercase"));
        assert!(!valid_env_name(""));
    }

    #[tokio::test]
    async fn stdio_fake_server_allowlist_and_call_cap() {
        let transport = StdioMcpTransport::spawn(fake_server_binary(), &[], &[])
            .await
            .unwrap();
        let client = McpClient::new(transport, Arc::new(TestApproval(true)))
            .with_allowed_tools(["allowed_tool".to_string()]);
        let ledger = UsageLedger::new(None);
        let toolbox = McpToolbox::connect(
            client,
            McpToolPolicy::new(["allowed_tool".to_string()], 1),
            Some(ledger),
            None,
        )
        .await
        .unwrap();
        assert_eq!(toolbox.definitions().len(), 1);
        assert!(
            toolbox
                .call("allowed_tool", json!({"id": 1}), None)
                .await
                .is_ok()
        );
        assert_eq!(toolbox.remaining_calls(), 0);
        assert!(matches!(
            toolbox.call("allowed_tool", json!({"id": 2}), None).await,
            Err(McpError::BudgetExceeded(_))
        ));
        assert!(matches!(
            toolbox.call("other_tool", json!({}), None).await,
            Err(McpError::ToolNotAllowed(_))
        ));
    }

    #[tokio::test]
    async fn stdio_fake_server_approval_denial() {
        let transport = StdioMcpTransport::spawn(fake_server_binary(), &[], &[])
            .await
            .unwrap();
        let client = McpClient::new(transport, Arc::new(TestApproval(false)));
        client.initialize().await.unwrap();
        let tools = client.list_tools().await.unwrap();
        assert_eq!(tools.len(), 2);
        assert!(matches!(
            client.call_tool("allowed_tool", json!({"id": 1})).await,
            Err(McpError::PermissionDenied)
        ));
    }

    #[tokio::test]
    async fn stdio_fake_server_budget_exceeded() {
        let transport = StdioMcpTransport::spawn(fake_server_binary(), &[], &[])
            .await
            .unwrap();
        let client = McpClient::new(transport, Arc::new(TestApproval(true)))
            .with_allowed_tools(["allowed_tool".to_string()]);
        let ledger = UsageLedger::new(Some(Budget::new(0.0)));
        let toolbox = McpToolbox::connect(
            client,
            McpToolPolicy::new(["allowed_tool".to_string()], 3),
            Some(ledger),
            None,
        )
        .await
        .unwrap();
        assert!(matches!(
            toolbox.call("allowed_tool", json!({"id": 1}), None).await,
            Err(McpError::BudgetExceeded(_))
        ));
        assert_eq!(toolbox.remaining_calls(), 3);
    }
}
