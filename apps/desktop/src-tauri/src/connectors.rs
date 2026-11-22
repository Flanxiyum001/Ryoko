//! Tauri commands for the connector catalog.
//!
//! The shell reads and writes `<workspace>/.ryoko/connectors.json`. The file
//! holds environment variable *names*, never secret values, and writes refuse a
//! symlinked `.ryoko` directory so a repository cannot redirect them.
use std::collections::BTreeSet;
use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use tauri::State;

use crate::workspace::{WorkspaceState, current_root};

const CONNECTORS_RELATIVE: &str = ".ryoko/connectors.json";
const PROBE_TIMEOUT: Duration = Duration::from_secs(15);
const MAX_PROBE_BYTES: u64 = 1024 * 1024;
const MAX_PROBE_LINES: usize = 64;
const MAX_TOOLS: usize = 128;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConnectorTransport {
    /// `stdio`, `sse` or `streamable_http`.
    pub kind: String,
    #[serde(default)]
    pub command: Option<String>,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default)]
    pub url: Option<String>,
}

/// Mirrors the on-disk schema exactly, so the CLI and this shell share one
/// file format (snake_case keys, no secret values).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConnectorEntry {
    pub id: String,
    pub name: String,
    pub category: String,
    #[serde(default)]
    pub description: String,
    pub transport: ConnectorTransport,
    #[serde(default)]
    pub required_env: Vec<String>,
    #[serde(default)]
    pub allowed_tools: Vec<String>,
    #[serde(default)]
    pub write_tools: Vec<String>,
    #[serde(default)]
    pub enabled: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_calls_per_run: Option<u32>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ConnectorTool {
    pub name: String,
    pub write: bool,
    pub allowed: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct ConnectorView {
    #[serde(flatten)]
    pub entry: ConnectorEntry,
    /// `ready`, `disabled`, `missing_env` or `no_tools`.
    pub status: String,
    pub missing_env: Vec<String>,
    pub tools: Vec<ConnectorTool>,
    /// `configured` for workspace entries, `preset` for built-in templates.
    pub source: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct ConnectorSnapshot {
    pub path: String,
    pub connectors: Vec<ConnectorView>,
    pub presets: Vec<ConnectorView>,
    /// A malformed registry is reported instead of hiding the presets.
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ConnectorInspection {
    pub connector_id: String,
    pub tools: Vec<String>,
    /// Shown next to the result, for example a truncated list notice.
    pub note: String,
}

#[derive(Debug, Deserialize)]
struct RegistryFile {
    #[serde(default)]
    connectors: Vec<ConnectorEntry>,
}

fn registry_path(root: &Path) -> PathBuf {
    root.join(CONNECTORS_RELATIVE)
}

fn valid_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 64
        && !id.contains("__")
        && id
            .bytes()
            .next()
            .is_some_and(|byte| byte.is_ascii_lowercase())
        && id.bytes().all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-' || byte == b'_'
        })
}

fn valid_env_name(name: &str) -> bool {
    let mut bytes = name.bytes();
    matches!(bytes.next(), Some(b'A'..=b'Z' | b'_'))
        && bytes.all(|byte| byte.is_ascii_uppercase() || byte.is_ascii_digit() || byte == b'_')
}

fn env_is_set(name: &str) -> bool {
    std::env::var(name).is_ok_and(|value| !value.trim().is_empty())
}

fn catalog(entry: &ConnectorEntry) -> Vec<ConnectorTool> {
    let mut tools: Vec<ConnectorTool> = Vec::new();
    for name in entry.allowed_tools.iter().chain(entry.write_tools.iter()) {
        if tools.iter().any(|tool| &tool.name == name) {
            continue;
        }
        tools.push(ConnectorTool {
            name: name.clone(),
            write: entry.write_tools.iter().any(|write| write == name),
            allowed: entry.allowed_tools.contains(name),
        });
    }
    tools
}

fn status(entry: &ConnectorEntry) -> (String, Vec<String>) {
    let missing_env = entry
        .required_env
        .iter()
        .filter(|name| !env_is_set(name))
        .cloned()
        .collect::<Vec<_>>();
    let status = if !entry.enabled {
        "disabled"
    } else if !missing_env.is_empty() {
        "missing_env"
    } else if entry.allowed_tools.is_empty() {
        "no_tools"
    } else {
        "ready"
    };
    (status.to_owned(), missing_env)
}

fn view(entry: &ConnectorEntry, source: &str) -> ConnectorView {
    let (status, missing_env) = status(entry);
    ConnectorView {
        tools: catalog(entry),
        status,
        missing_env,
        source: source.to_owned(),
        entry: entry.clone(),
    }
}

fn presets() -> Vec<ConnectorEntry> {
    let parsed: RegistryFile =
        serde_json::from_str(include_str!("../../../../configs/connectors.example.json"))
            .expect("the built-in connector catalogue is validated by its tests");
    parsed
        .connectors
        .into_iter()
        .map(|mut entry| {
            entry.enabled = false;
            entry
        })
        .collect()
}

fn load(root: &Path) -> Result<Vec<ConnectorEntry>, String> {
    let path = registry_path(root);
    if !path.exists() {
        return Ok(Vec::new());
    }
    let text = std::fs::read_to_string(&path)
        .map_err(|error| format!("cannot read {}: {error}", path.display()))?;
    let registry: RegistryFile = serde_json::from_str(&text)
        .map_err(|error| format!("{} is not valid connector JSON: {error}", path.display()))?;
    if registry.connectors.len() > 32 {
        return Err("a connector registry holds at most 32 entries".to_owned());
    }
    for entry in &registry.connectors {
        validate(entry)?;
    }
    Ok(registry.connectors)
}

fn validate(entry: &ConnectorEntry) -> Result<(), String> {
    if !valid_id(&entry.id) {
        return Err(format!("invalid connector id `{}`", entry.id));
    }
    if entry.name.trim().is_empty() || entry.category.trim().is_empty() {
        return Err(format!(
            "connector `{}` needs a name and category",
            entry.id
        ));
    }
    if !matches!(
        entry.transport.kind.as_str(),
        "stdio" | "sse" | "streamable_http"
    ) {
        return Err(format!(
            "connector `{}` has an unknown transport `{}`",
            entry.id, entry.transport.kind
        ));
    }
    if entry.transport.kind == "stdio" {
        if entry
            .transport
            .command
            .as_deref()
            .is_none_or(|command| command.trim().is_empty())
        {
            return Err(format!("connector `{}` needs a stdio command", entry.id));
        }
        if entry.transport.args.len() > 64 {
            return Err(format!("connector `{}` has too many arguments", entry.id));
        }
    } else {
        let url = entry.transport.url.as_deref().unwrap_or_default();
        if !url.starts_with("https://")
            && !url.starts_with("http://127.0.0.1")
            && !url.starts_with("http://localhost")
        {
            return Err(format!(
                "connector `{}` must use an HTTPS endpoint outside loopback",
                entry.id
            ));
        }
        if url.contains(['?', '#', '@']) {
            return Err(format!(
                "connector `{}` endpoint must not carry credentials, query or fragment",
                entry.id
            ));
        }
    }
    if entry.required_env.len() > 16 {
        return Err(format!(
            "connector `{}` declares too many variables",
            entry.id
        ));
    }
    for name in &entry.required_env {
        if !valid_env_name(name) {
            return Err(format!(
                "connector `{}` has an invalid environment variable name",
                entry.id
            ));
        }
    }
    for tools in [&entry.allowed_tools, &entry.write_tools] {
        if tools.len() > 64 {
            return Err(format!("connector `{}` lists too many tools", entry.id));
        }
        for tool in tools {
            if tool.is_empty()
                || tool.len() > 128
                || tool.chars().any(|ch| ch.is_control() || ch.is_whitespace())
            {
                return Err(format!(
                    "connector `{}` lists an invalid tool name",
                    entry.id
                ));
            }
        }
    }
    Ok(())
}

/// Writes the registry atomically, refusing a symlinked directory or file.
fn save(root: &Path, connectors: &[ConnectorEntry]) -> Result<PathBuf, String> {
    let path = registry_path(root);
    let directory = path
        .parent()
        .ok_or_else(|| "connector path has no parent".to_owned())?;
    std::fs::create_dir_all(directory)
        .map_err(|error| format!("cannot create {}: {error}", directory.display()))?;
    for candidate in [directory, path.as_path()] {
        if std::fs::symlink_metadata(candidate)
            .map(|metadata| metadata.file_type().is_symlink())
            .unwrap_or(false)
        {
            return Err(format!(
                "refusing to write connector settings through a symlink: {}",
                candidate.display()
            ));
        }
    }
    let payload = serde_json::json!({ "version": 1, "connectors": connectors });
    let mut text = serde_json::to_string_pretty(&payload)
        .map_err(|error| format!("cannot encode connector settings: {error}"))?;
    text.push('\n');
    let staged = path.with_extension("json.tmp");
    std::fs::write(&staged, text)
        .map_err(|error| format!("cannot write {}: {error}", staged.display()))?;
    std::fs::rename(&staged, &path)
        .map_err(|error| format!("cannot replace {}: {error}", path.display()))?;
    Ok(path)
}

/// Forces the CLI and desktop to write the same directory: a tool name that
/// the connector does not advertise cannot be added to its allowlist.
fn apply_tools(
    entry: &ConnectorEntry,
    id: &str,
    tools: Vec<String>,
) -> Result<Vec<String>, String> {
    let mut known = BTreeSet::new();
    for name in entry.allowed_tools.iter().chain(entry.write_tools.iter()) {
        known.insert(name.clone());
    }
    let mut allowed = Vec::new();
    for tool in tools {
        if !known.contains(&tool) {
            return Err(format!(
                "`{tool}` is not a tool the `{id}` connector advertises"
            ));
        }
        if !allowed.contains(&tool) {
            allowed.push(tool);
        }
    }
    Ok(allowed)
}

fn snapshot(root: &Path) -> ConnectorSnapshot {
    let path = registry_path(root).display().to_string();
    let presets = presets()
        .iter()
        .map(|entry| view(entry, "preset"))
        .collect::<Vec<_>>();
    match load(root) {
        Ok(entries) => ConnectorSnapshot {
            path,
            connectors: entries
                .iter()
                .map(|entry| view(entry, "configured"))
                .collect(),
            presets,
            error: None,
        },
        Err(error) => ConnectorSnapshot {
            path,
            connectors: Vec::new(),
            presets,
            error: Some(error),
        },
    }
}

fn update<F>(root: &Path, id: &str, change: F) -> Result<ConnectorSnapshot, String>
where
    F: FnOnce(&mut ConnectorEntry) -> Result<(), String>,
{
    let mut connectors = load(root)?;
    let entry = connectors
        .iter_mut()
        .find(|entry| entry.id == id)
        .ok_or_else(|| format!("connector `{id}` is not configured"))?;
    change(entry)?;
    validate(entry)?;
    save(root, &connectors)?;
    Ok(snapshot(root))
}

#[tauri::command]
pub fn list_connectors(state: State<'_, WorkspaceState>) -> Result<ConnectorSnapshot, String> {
    Ok(snapshot(&current_root(&state)?))
}

#[tauri::command]
pub fn add_connector(
    preset: String,
    state: State<'_, WorkspaceState>,
) -> Result<ConnectorSnapshot, String> {
    let root = current_root(&state)?;
    let mut connectors = load(&root)?;
    if connectors.iter().any(|entry| entry.id == preset) {
        return Err(format!("connector `{preset}` is already configured"));
    }
    let entry = presets()
        .into_iter()
        .find(|entry| entry.id == preset)
        .ok_or_else(|| format!("unknown connector preset `{preset}`"))?;
    connectors.push(entry);
    save(&root, &connectors)?;
    Ok(snapshot(&root))
}

#[tauri::command]
pub fn set_connector_enabled(
    id: String,
    enabled: bool,
    state: State<'_, WorkspaceState>,
) -> Result<ConnectorSnapshot, String> {
    let root = current_root(&state)?;
    update(&root, &id, |entry| {
        if enabled && entry.allowed_tools.is_empty() {
            return Err("Allow at least one tool before enabling this connector.".to_owned());
        }
        entry.enabled = enabled;
        Ok(())
    })
}

#[tauri::command]
pub fn set_connector_tools(
    id: String,
    tools: Vec<String>,
    state: State<'_, WorkspaceState>,
) -> Result<ConnectorSnapshot, String> {
    let root = current_root(&state)?;
    update(&root, &id, |entry| {
        entry.allowed_tools = apply_tools(entry, &id, tools)?;
        Ok(())
    })
}

/// Spawns the configured stdio server, performs the MCP handshake and returns
/// the tool names it advertises. Nothing is called and no argument is sent.
#[tauri::command]
pub async fn inspect_connector(
    id: String,
    state: State<'_, WorkspaceState>,
) -> Result<ConnectorInspection, String> {
    let root = current_root(&state)?;
    let entry = load(&root)?
        .into_iter()
        .find(|entry| entry.id == id)
        .ok_or_else(|| format!("connector `{id}` is not configured"))?;
    let connector_id = id.clone();
    let mut inspection = tauri::async_runtime::spawn_blocking(move || probe(&entry))
        .await
        .map_err(|error| format!("connector inspection failed: {error}"))??;
    inspection.connector_id = connector_id;
    Ok(inspection)
}

fn probe(entry: &ConnectorEntry) -> Result<ConnectorInspection, String> {
    if entry.transport.kind != "stdio" {
        return Err(format!(
            "inspecting a {} connector is not supported in this build",
            entry.transport.kind
        ));
    }
    let command = entry
        .transport
        .command
        .as_deref()
        .ok_or_else(|| "connector has no stdio command".to_owned())?;
    let mut missing = Vec::new();
    let mut env = Vec::new();
    for name in &entry.required_env {
        match std::env::var(name) {
            Ok(value) if !value.trim().is_empty() => env.push((name.clone(), value)),
            _ => missing.push(name.clone()),
        }
    }
    if !missing.is_empty() {
        return Err(format!(
            "Set {} before inspecting this connector.",
            missing.join(", ")
        ));
    }

    let mut child = Command::new(command)
        .args(&entry.transport.args)
        .env_clear()
        .envs(
            ["PATH", "TMPDIR", "TEMP"]
                .iter()
                .filter_map(|name| std::env::var_os(name).map(|value| (*name, value))),
        )
        .envs(env)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|error| format!("cannot start `{command}`: {error}"))?;
    let stdin = child
        .stdin
        .take()
        .ok_or_else(|| "connector stdin unavailable".to_owned())?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| "connector stdout unavailable".to_owned())?;
    let (sender, receiver) = mpsc::channel::<Result<String, String>>();
    std::thread::spawn(move || {
        let bounded = stdout.take(MAX_PROBE_BYTES);
        for line in BufReader::new(bounded).lines() {
            match line {
                Ok(line) => {
                    if sender.send(Ok(line)).is_err() {
                        return;
                    }
                }
                Err(error) => {
                    let _ = sender.send(Err(error.to_string()));
                    return;
                }
            }
        }
    });

    let result = handshake(stdin, &receiver);
    let _ = child.kill();
    let _ = child.wait();
    result
}

fn handshake(
    mut stdin: impl Write,
    receiver: &mpsc::Receiver<Result<String, String>>,
) -> Result<ConnectorInspection, String> {
    let initialize = serde_json::json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "initialize",
        "params": {
            "protocolVersion": "2025-06-18",
            "capabilities": {},
            "clientInfo": {"name": "ryoko-desktop", "version": env!("CARGO_PKG_VERSION")}
        }
    });
    write_frame(&mut stdin, &initialize)?;
    let response = next_response(receiver, 1)?;
    if response.get("error").is_some() {
        return Err("the connector rejected initialization".to_owned());
    }
    write_frame(
        &mut stdin,
        &serde_json::json!({"jsonrpc": "2.0", "method": "notifications/initialized"}),
    )?;
    write_frame(
        &mut stdin,
        &serde_json::json!({"jsonrpc": "2.0", "id": 2, "method": "tools/list", "params": {}}),
    )?;
    let response = next_response(receiver, 2)?;
    let tools = response["result"]["tools"]
        .as_array()
        .ok_or_else(|| "the connector returned no tool catalogue".to_owned())?;
    let mut names = Vec::new();
    for tool in tools.iter().take(MAX_TOOLS + 1) {
        let name = tool["name"]
            .as_str()
            .ok_or_else(|| "the connector advertised an invalid tool".to_owned())?;
        names.push(name.to_owned());
    }
    let note = if names.len() > MAX_TOOLS {
        names.truncate(MAX_TOOLS);
        format!("showing the first {MAX_TOOLS} tools")
    } else {
        format!("{} tool(s) advertised by the server", names.len())
    };
    Ok(ConnectorInspection {
        connector_id: String::new(),
        tools: names,
        note,
    })
}

fn write_frame(stdin: &mut impl Write, payload: &serde_json::Value) -> Result<(), String> {
    let mut bytes = serde_json::to_vec(payload)
        .map_err(|error| format!("cannot encode the MCP request: {error}"))?;
    if bytes.len() > 64 * 1024 {
        return Err("the MCP request exceeds 64 KiB".to_owned());
    }
    bytes.push(b'\n');
    stdin
        .write_all(&bytes)
        .and_then(|()| stdin.flush())
        .map_err(|error| format!("the connector closed its input: {error}"))
}

fn next_response(
    receiver: &mpsc::Receiver<Result<String, String>>,
    id: u64,
) -> Result<serde_json::Value, String> {
    for _ in 0..MAX_PROBE_LINES {
        let line = receiver
            .recv_timeout(PROBE_TIMEOUT)
            .map_err(|_| "the connector did not answer in time".to_owned())?
            .map_err(|error| format!("cannot read the connector response: {error}"))?;
        if line.len() > MAX_PROBE_BYTES as usize {
            return Err("the connector response is too large".to_owned());
        }
        let Ok(value) = serde_json::from_str::<serde_json::Value>(&line) else {
            continue;
        };
        if value["id"].as_u64() == Some(id) {
            return Ok(value);
        }
    }
    Err("the connector returned too many notifications".to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn github() -> ConnectorEntry {
        presets()
            .into_iter()
            .find(|entry| entry.id == "github")
            .expect("github preset")
    }

    /// Self-cleaning workspace directory without adding a test dependency to
    /// the shell's locked manifest.
    struct TempRoot(PathBuf);

    impl TempRoot {
        fn new() -> Self {
            static COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
            let path = std::env::temp_dir().join(format!(
                "ryoko-connectors-{}-{}",
                std::process::id(),
                COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
            ));
            std::fs::create_dir_all(&path).expect("temp workspace");
            Self(path)
        }

        fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for TempRoot {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn the_shipped_catalogue_matches_the_shared_file() {
        let presets = presets();
        assert_eq!(
            presets
                .iter()
                .map(|entry| entry.id.as_str())
                .collect::<Vec<_>>(),
            vec!["github", "cloudflare", "filesystem", "fetch", "custom"]
        );
        for entry in &presets {
            assert!(!entry.enabled);
            validate(entry).expect("preset validates");
        }
    }

    #[test]
    fn readiness_matches_the_cli_rules() {
        let mut entry = github();
        assert_eq!(status(&entry).0, "disabled");
        entry.enabled = true;
        entry.required_env.clear();
        assert_eq!(status(&entry).0, "ready");
        entry.allowed_tools.clear();
        assert_eq!(status(&entry).0, "no_tools");

        // A declared variable that this process never sets fails closed.
        entry.allowed_tools = vec!["list_issues".into()];
        entry.required_env = vec!["RYOKO_TEST_CONNECTOR_UNSET_VARIABLE".into()];
        let (state, missing) = status(&entry);
        assert_eq!(state, "missing_env");
        assert_eq!(missing, vec!["RYOKO_TEST_CONNECTOR_UNSET_VARIABLE"]);
    }

    #[test]
    fn write_tools_are_tagged_and_never_pre_allowed() {
        let entry = github();
        let tools = catalog(&entry);
        let create = tools
            .iter()
            .find(|tool| tool.name == "create_issue")
            .unwrap();
        assert!(create.write);
        assert!(!create.allowed);
        let list = tools
            .iter()
            .find(|tool| tool.name == "list_issues")
            .unwrap();
        assert!(!list.write);
        assert!(list.allowed);
    }

    #[test]
    fn invalid_entries_are_refused_before_they_reach_disk() {
        let mut entry = github();
        entry.id = "GitHub".into();
        assert!(validate(&entry).is_err());

        let mut entry = github();
        entry.transport.kind = "stdio".into();
        entry.transport.command = Some(" ".into());
        assert!(validate(&entry).is_err());

        let mut entry = github();
        entry.transport.kind = "sse".into();
        entry.transport.url = Some("http://mcp.example.com/sse".into());
        assert!(validate(&entry).is_err());

        let mut entry = github();
        entry.required_env = vec!["github_token".into()];
        assert!(validate(&entry).is_err());
    }

    #[test]
    fn a_registry_round_trips_and_a_missing_file_is_empty() {
        let dir = TempRoot::new();
        assert!(load(dir.path()).unwrap().is_empty());
        let entry = github();
        save(dir.path(), std::slice::from_ref(&entry)).unwrap();
        let loaded = load(dir.path()).unwrap();
        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0].id, "github");
        assert!(!loaded[0].enabled);
    }

    #[test]
    fn saving_refuses_a_symlinked_metadata_directory() {
        let dir = TempRoot::new();
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(dir.path().join("elsewhere"), dir.path().join(".ryoko"))
                .unwrap();
            std::fs::create_dir_all(dir.path().join("elsewhere")).unwrap();
            let error = save(dir.path(), &[github()]).expect_err("symlink must be refused");
            assert!(error.contains("symlink"), "{error}");
        }
    }

    #[test]
    fn a_tool_selection_only_accepts_advertised_tools_and_deduplicates() {
        let entry = github();
        let selected = apply_tools(
            &entry,
            "github",
            vec![
                "list_issues".into(),
                "list_issues".into(),
                "create_issue".into(),
            ],
        )
        .unwrap();
        assert_eq!(selected, vec!["list_issues", "create_issue"]);
        let error = apply_tools(&entry, "github", vec!["drop_database".into()])
            .expect_err("unknown tools are refused");
        assert!(error.contains("drop_database"), "{error}");
    }
}
