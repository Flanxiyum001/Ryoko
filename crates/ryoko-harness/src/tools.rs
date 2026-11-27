//! Built-in tool definitions, host extension points and the gated command
//! runner.

use std::path::{Component, Path};
use std::process::Stdio;
use std::time::Duration;

use async_trait::async_trait;
use ryoko_models::ToolDefinition;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

/// Largest file the harness will stage.
pub const MAX_STAGED_FILE_BYTES: usize = 256 * 1024;
/// Most files one run may stage.
pub const MAX_STAGED_FILES: usize = 64;
const MAX_COMMAND_ARGS: usize = 32;
const MAX_COMMAND_ARG_CHARS: usize = 1024;
const COMMAND_TIMEOUT: Duration = Duration::from_secs(120);
const COMMAND_OUTPUT_BYTES: usize = 32 * 1024;

/// A file change the agent proposes. Nothing is written to disk; the host
/// shows these as diffs and applies only what the user approves.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProposedChange {
    /// Workspace-relative path with `/` separators.
    pub path: String,
    /// The file's content before the run, or `None` for a new file.
    pub original: Option<String>,
    /// The proposed full content.
    pub content: String,
}

/// A command the agent wants to run, shown to the user for approval.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CommandRequest {
    pub program: String,
    pub args: Vec<String>,
    /// The agent's explanation, shown next to the approval prompt.
    pub reason: String,
}

/// Host-controlled approval for side effects. Implementations typically ask
/// the user in the UI. Agents can never change the gate.
#[async_trait]
pub trait ApprovalGate: Send + Sync {
    async fn approve_command(&self, request: &CommandRequest) -> bool;
}

/// Refuses every command. The default gate.
pub struct DenyAllCommands;

#[async_trait]
impl ApprovalGate for DenyAllCommands {
    async fn approve_command(&self, _request: &CommandRequest) -> bool {
        false
    }
}

/// An extra tool supplied by the host, such as an approved MCP tool.
#[async_trait]
pub trait ExternalTool: Send + Sync {
    fn definition(&self) -> ToolDefinition;
    /// Returns the tool's text output, or an error message for the model.
    async fn call(&self, arguments: Value) -> Result<String, String>;
}

pub(crate) const BUILTIN_NAMES: &[&str] = &[
    "read_file",
    "list_files",
    "search_text",
    "write_file",
    "edit_file",
    "update_plan",
    "remember",
    "load_skill",
    "run_command",
    "delegate",
];

pub(crate) struct ToolSet {
    pub commands: bool,
    pub delegate: bool,
    pub skills: bool,
}

pub(crate) fn builtin_definitions(set: &ToolSet) -> Vec<ToolDefinition> {
    let mut tools = vec![
        ToolDefinition::new(
            "read_file",
            "Read a UTF-8 source file. Paths are relative to the workspace root and use '/'. \
             Shows your staged version if you already changed the file.",
            json!({"type":"object","properties":{"path":{"type":"string"}},"required":["path"]}),
        ),
        ToolDefinition::new(
            "list_files",
            "List source files in the workspace (respects .gitignore).",
            json!({"type":"object","properties":{"limit":{"type":"integer","minimum":1,"maximum":100}}}),
        ),
        ToolDefinition::new(
            "search_text",
            "Find lines containing a literal string across workspace source files.",
            json!({"type":"object","properties":{"query":{"type":"string"}},"required":["query"]}),
        ),
        ToolDefinition::new(
            "write_file",
            "Stage the full new content of a file (creates it if missing). The user reviews \
             every staged file as a diff before anything is written.",
            json!({"type":"object","properties":{"path":{"type":"string"},"content":{"type":"string"}},"required":["path","content"]}),
        ),
        ToolDefinition::new(
            "edit_file",
            "Stage a targeted edit: replace `old` with `new`. `old` must appear exactly once.",
            json!({"type":"object","properties":{"path":{"type":"string"},"old":{"type":"string"},"new":{"type":"string"}},"required":["path","old","new"]}),
        ),
        ToolDefinition::new(
            "update_plan",
            "Replace your step-by-step plan. Keep it short; mark one step in_progress at a time.",
            json!({"type":"object","properties":{"items":{"type":"array","items":{"type":"object","properties":{"task":{"type":"string"},"status":{"type":"string","enum":["pending","in_progress","done"]}},"required":["task","status"]}}},"required":["items"]}),
        ),
        ToolDefinition::new(
            "remember",
            "Save one short, durable fact about this project for future sessions (conventions, \
             commands, decisions). Never store secrets.",
            json!({"type":"object","properties":{"note":{"type":"string"}},"required":["note"]}),
        ),
    ];
    if set.skills {
        tools.push(ToolDefinition::new(
            "load_skill",
            "Load the full instructions of one of the listed skills.",
            json!({"type":"object","properties":{"name":{"type":"string"}},"required":["name"]}),
        ));
    }
    if set.commands {
        tools.push(ToolDefinition::new(
            "run_command",
            "Run a program in the workspace root, such as a test or build. The user must approve \
             each command. It sees the files on disk, not your staged edits.",
            json!({"type":"object","properties":{"program":{"type":"string"},"args":{"type":"array","items":{"type":"string"}},"reason":{"type":"string"}},"required":["program","reason"]}),
        ));
    }
    if set.delegate {
        tools.push(ToolDefinition::new(
            "delegate",
            "Hand a focused, self-contained sub-task to a fresh sub-agent with its own context. \
             It shares your staged edits and returns a summary.",
            json!({"type":"object","properties":{"task":{"type":"string"}},"required":["task"]}),
        ));
    }
    tools
}

/// Validates a model-supplied workspace path for staging.
pub(crate) fn validate_relative_path(path: &str) -> Result<String, String> {
    if path.is_empty() || path.len() > 512 || path.contains('\\') || path.contains('\0') {
        return Err("invalid path: use a workspace-relative path with '/'".into());
    }
    let relative = Path::new(path);
    if relative.is_absolute() {
        return Err("invalid path: absolute paths are not allowed".into());
    }
    for component in relative.components() {
        match component {
            Component::Normal(name) if !name.to_string_lossy().starts_with('.') => {}
            _ => {
                return Err(
                    "invalid path: no '..', hidden folders or drive prefixes allowed".into(),
                );
            }
        }
    }
    Ok(path.trim_start_matches("./").to_owned())
}

pub(crate) fn parse_command(arguments: &Value) -> Result<CommandRequest, String> {
    #[derive(Deserialize)]
    struct Raw {
        program: String,
        #[serde(default)]
        args: Vec<String>,
        #[serde(default)]
        reason: String,
    }
    let raw: Raw = serde_json::from_value(arguments.clone())
        .map_err(|error| format!("invalid run_command arguments: {error}"))?;
    let program = raw.program.trim();
    if program.is_empty()
        || program.len() > 64
        || program.contains(['/', '\\', '\0'])
        || program.chars().any(char::is_control)
    {
        return Err("program must be a bare program name such as `cargo` or `npm`".into());
    }
    if raw.args.len() > MAX_COMMAND_ARGS
        || raw
            .args
            .iter()
            .any(|arg| arg.chars().count() > MAX_COMMAND_ARG_CHARS || arg.contains('\0'))
    {
        return Err("too many or too long command arguments".into());
    }
    Ok(CommandRequest {
        program: program.to_owned(),
        args: raw.args,
        reason: raw.reason.chars().take(500).collect(),
    })
}

/// Runs an approved command in `root` with a minimal environment, no stdin,
/// a timeout and bounded output. No shell is involved.
pub(crate) async fn run_approved_command(root: &Path, request: &CommandRequest) -> String {
    let mut command = tokio::process::Command::new(&request.program);
    command
        .args(&request.args)
        .current_dir(root)
        .env_clear()
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    // Keep only what toolchains need to start; never forward credentials.
    for key in [
        "PATH",
        "HOME",
        "USERPROFILE",
        "SYSTEMROOT",
        "SystemRoot",
        "WINDIR",
        "TEMP",
        "TMP",
        "LANG",
        "PATHEXT",
        "CARGO_HOME",
        "RUSTUP_HOME",
        "APPDATA",
        "LOCALAPPDATA",
    ] {
        if let Some(value) = std::env::var_os(key) {
            command.env(key, value);
        }
    }
    let child = match command.spawn() {
        Ok(child) => child,
        Err(error) => return format!("could not start `{}`: {error}", request.program),
    };
    match tokio::time::timeout(COMMAND_TIMEOUT, child.wait_with_output()).await {
        Err(_) => format!("`{}` timed out after {COMMAND_TIMEOUT:?}", request.program),
        Ok(Err(error)) => format!("`{}` failed: {error}", request.program),
        Ok(Ok(output)) => {
            let mut text = format!(
                "exit status: {}\n",
                output
                    .status
                    .code()
                    .map_or_else(|| "terminated".to_owned(), |code| code.to_string())
            );
            text.push_str("--- stdout ---\n");
            text.push_str(&String::from_utf8_lossy(&output.stdout));
            text.push_str("\n--- stderr ---\n");
            text.push_str(&String::from_utf8_lossy(&output.stderr));
            truncate_middle(&text, COMMAND_OUTPUT_BYTES)
        }
    }
}

/// Keeps the start and end of long output, which is where errors usually are.
pub(crate) fn truncate_middle(text: &str, limit: usize) -> String {
    if text.len() <= limit {
        return text.to_owned();
    }
    let half = limit / 2;
    let mut head_end = half;
    while !text.is_char_boundary(head_end) {
        head_end -= 1;
    }
    let mut tail_start = text.len() - half;
    while !text.is_char_boundary(tail_start) {
        tail_start += 1;
    }
    format!(
        "{}\n[… {} bytes omitted …]\n{}",
        &text[..head_end],
        tail_start - head_end,
        &text[tail_start..]
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_unsafe_paths() {
        for bad in [
            "",
            "/etc/passwd",
            "../x.rs",
            "src/../../x",
            ".git/config",
            "src/.env",
            "C:\\x",
            "a\\b",
        ] {
            assert!(validate_relative_path(bad).is_err(), "{bad}");
        }
        assert_eq!(
            validate_relative_path("src/main.rs").unwrap(),
            "src/main.rs"
        );
    }

    #[test]
    fn rejects_shell_style_programs() {
        for bad in ["", "/bin/sh", "..\\cmd.exe", "./run"] {
            assert!(parse_command(&json!({"program": bad})).is_err(), "{bad}");
        }
        let ok =
            parse_command(&json!({"program":"cargo","args":["test"],"reason":"verify"})).unwrap();
        assert_eq!(ok.args, ["test"]);
    }

    #[test]
    fn truncates_on_char_boundaries() {
        let text = "é".repeat(100);
        let out = truncate_middle(&text, 51);
        assert!(out.contains("omitted"));
    }
}
