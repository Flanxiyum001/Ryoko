//! Tauri commands for the connector catalog.
//!
//! The shell reads and writes `<workspace>/.ryoko/connectors.json` through
//! [`ryoko_core::connectors`], so the file format, the validation rules and the
//! built-in presets are the same ones the CLI uses. The file holds environment
//! variable *names*, never secret values.
//!
//! Only two things live here: the view model the panel renders, and a bounded
//! stdio probe that lists a server's tools without calling any of them.
use std::collections::BTreeMap;
use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::time::Duration;

use ryoko_core::connectors::{
    ConnectorConfig, ConnectorRegistry, ConnectorStatus, ConnectorTool, ConnectorTransport,
    builtin_presets, readiness, resolve_required_env,
};
use serde::Serialize;
use tauri::State;

use crate::workspace::{WorkspaceState, current_root};

const PROBE_TIMEOUT: Duration = Duration::from_secs(15);
const MAX_PROBE_BYTES: u64 = 1024 * 1024;
const MAX_PROBE_LINES: usize = 64;
const MAX_TOOLS: usize = 128;

/// One connector plus the offline readiness the panel shows.
#[derive(Debug, Clone, Serialize)]
pub struct ConnectorView {
    #[serde(flatten)]
    pub entry: ConnectorConfig,
    pub status: ConnectorStatus,
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

fn env_lookup(name: &str) -> Option<String> {
    std::env::var(name).ok()
}

fn view(entry: &ConnectorConfig, source: &str) -> ConnectorView {
    let readiness = readiness(entry, env_lookup);
    ConnectorView {
        tools: entry.catalog(),
        status: readiness.status,
        missing_env: readiness.missing_env,
        source: source.to_owned(),
        entry: entry.clone(),
    }
}

fn presets() -> Vec<ConnectorConfig> {
    builtin_presets().connectors
}

fn load(root: &Path) -> Result<ConnectorRegistry, String> {
    let path = ConnectorRegistry::path(root);
    ConnectorRegistry::load(root).map_err(|error| format!("{}: {error}", path.display()))
}

/// Writes the registry atomically; `ryoko-core` refuses a symlinked directory
/// or file so a repository cannot redirect the write.
fn save(root: &Path, registry: &ConnectorRegistry) -> Result<PathBuf, String> {
    registry.save(root).map_err(|error| error.to_string())
}

fn snapshot(root: &Path) -> ConnectorSnapshot {
    let path = ConnectorRegistry::path(root).display().to_string();
    let presets = presets()
        .iter()
        .map(|entry| view(entry, "preset"))
        .collect::<Vec<_>>();
    match load(root) {
        Ok(registry) => ConnectorSnapshot {
            path,
            connectors: registry
                .connectors
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
    F: FnOnce(&mut ConnectorConfig) -> Result<(), String>,
{
    let mut registry = load(root)?;
    let entry = registry
        .get_mut(id)
        .ok_or_else(|| format!("connector `{id}` is not configured"))?;
    change(entry)?;
    entry.validate().map_err(|error| error.to_string())?;
    save(root, &registry)?;
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
    let mut registry = load(&root)?;
    let entry = presets()
        .into_iter()
        .find(|entry| entry.id == preset)
        .ok_or_else(|| format!("unknown connector preset `{preset}`"))?;
    registry.add(entry).map_err(|error| error.to_string())?;
    save(&root, &registry)?;
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
        entry.allowed_tools = entry
            .select_tools(tools)
            .map_err(|error| error.to_string())?;
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
        .connectors
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

fn probe(entry: &ConnectorConfig) -> Result<ConnectorInspection, String> {
    let ConnectorTransport::Stdio { command, args } = &entry.transport else {
        return Err(format!(
            "inspecting a {} connector is not supported in this build",
            entry.transport.kind()
        ));
    };
    let env: BTreeMap<String, String> =
        resolve_required_env(&entry.id, &entry.required_env, env_lookup)
            .map_err(|error| error.to_string())?;

    let mut child = Command::new(command)
        .args(args)
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

    fn github() -> ConnectorConfig {
        builtin_presets()
            .connectors
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
            entry.validate().expect("preset validates");
        }
    }

    #[test]
    fn readiness_is_the_same_answer_the_cli_gives() {
        let mut entry = github();
        assert_eq!(
            readiness(&entry, |_| Some("set".into())).status,
            ConnectorStatus::Disabled
        );
        entry.enabled = true;
        entry.required_env.clear();
        assert_eq!(readiness(&entry, |_| None).status, ConnectorStatus::Ready);
        entry.allowed_tools.clear();
        assert_eq!(readiness(&entry, |_| None).status, ConnectorStatus::NoTools);

        // A declared variable that is unset fails closed and names itself.
        entry.allowed_tools = vec!["list_issues".into()];
        entry.required_env = vec!["RYOKO_TEST_CONNECTOR_UNSET_VARIABLE".into()];
        let readiness = readiness(&entry, |_| None);
        assert_eq!(readiness.status, ConnectorStatus::MissingEnv);
        assert_eq!(
            readiness.missing_env,
            vec!["RYOKO_TEST_CONNECTOR_UNSET_VARIABLE"]
        );
    }

    #[test]
    fn write_tools_are_tagged_and_never_pre_allowed() {
        let entry = github();
        let tools = entry.catalog();
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
        assert!(entry.validate().is_err());

        let mut entry = github();
        entry.transport = ConnectorTransport::Stdio {
            command: " ".into(),
            args: Vec::new(),
        };
        assert!(entry.validate().is_err());

        let mut entry = github();
        entry.transport = ConnectorTransport::Sse {
            url: "http://mcp.example.com/sse".into(),
        };
        assert!(entry.validate().is_err());

        let mut entry = github();
        entry.required_env = vec!["github_token".into()];
        assert!(entry.validate().is_err());
    }

    #[test]
    fn a_registry_round_trips_and_a_missing_file_is_empty() {
        let dir = TempRoot::new();
        assert!(load(dir.path()).unwrap().connectors.is_empty());

        let mut registry = ConnectorRegistry::default();
        registry.connectors.push(github());
        save(dir.path(), &registry).unwrap();
        let loaded = load(dir.path()).unwrap();
        assert_eq!(loaded.connectors.len(), 1);
        assert_eq!(loaded.connectors[0].id, "github");
        assert!(!loaded.connectors[0].enabled);

        registry.connectors.clear();
        save(dir.path(), &registry).unwrap();
        assert!(load(dir.path()).unwrap().connectors.is_empty());
    }

    #[test]
    fn saving_refuses_a_symlinked_metadata_directory() {
        let dir = TempRoot::new();
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(dir.path().join("elsewhere"), dir.path().join(".ryoko"))
                .unwrap();
            std::fs::create_dir_all(dir.path().join("elsewhere")).unwrap();
            let mut registry = ConnectorRegistry::default();
            registry.connectors.push(github());
            let error = save(dir.path(), &registry).expect_err("symlink must be refused");
            assert!(error.contains("symlink"), "{error}");
        }
    }

    #[test]
    fn a_tool_selection_only_accepts_advertised_tools_and_deduplicates() {
        let entry = github();
        let selected = entry
            .select_tools([
                "list_issues".to_string(),
                "list_issues".to_string(),
                "create_issue".to_string(),
            ])
            .unwrap();
        assert_eq!(selected, vec!["list_issues", "create_issue"]);
        let error = entry
            .select_tools(["drop_database".to_string()])
            .expect_err("unknown tools are refused");
        assert!(error.to_string().contains("drop_database"), "{error}");
    }

    #[test]
    fn inspecting_a_network_connector_is_refused() {
        let mut entry = github();
        entry.transport = ConnectorTransport::Sse {
            url: "https://mcp.example.com/sse".into(),
        };
        let error = probe(&entry).expect_err("only stdio can be probed");
        assert!(error.contains("not supported"), "{error}");
    }
}
