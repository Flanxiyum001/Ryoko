//! Multi-server language-server process pool.
//!
//! `ryoko-lsp` always runs; standard servers registered in
//! `.ryoko/desktop/lsp.json` (see [`crate::servers`]) run beside it. Each
//! server is one child process; frames cross the Tauri boundary as JSON
//! strings with an explicit server id, and the webview multiplexer routes
//! messages (see `docs/protocols/DESKTOP_LSP_MULTIPLEX.md`).

use std::collections::HashMap;
use std::env;
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::Mutex;
use std::thread;

use serde::Serialize;
use tauri::{AppHandle, Emitter, State};

use crate::lsp_frame::{MAX_FRAME_BYTES, frame, read_frame};
use crate::servers::{LspRegistry, registry_path};
use crate::workspace::{WorkspaceState, current_root};

pub const RYOKO_LSP_ID: &str = "ryoko-lsp";

#[derive(Default)]
pub struct LspState {
    children: Mutex<HashMap<String, Child>>,
}

#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct LspServerInfo {
    pub registry: LspRegistry,
    pub registry_path: Option<String>,
}

fn resolve_binary(name: &str) -> Result<PathBuf, String> {
    let file = if cfg!(windows) {
        format!("{name}.exe")
    } else {
        name.to_owned()
    };
    if name == RYOKO_LSP_ID {
        if let Some(path) = env::var_os("RYOKO_LSP_BIN") {
            return Ok(PathBuf::from(path));
        }
        let executable = env::current_exe()
            .map_err(|error| format!("cannot resolve the Ryoko executable: {error}"))?;
        if let Some(directory) = executable.parent() {
            let candidate = directory.join(&file);
            if candidate.is_file() {
                return Ok(candidate);
            }
        }
        // `tauri dev` builds the desktop crate in its own target directory
        // (it is excluded from the workspace), while `ryoko-lsp` lands in the
        // workspace `target/`. Debug builds also look there so a local
        // `cargo build -p ryoko-native --bin ryoko-lsp` is picked up.
        if cfg!(debug_assertions) {
            if let Some(candidate) = find_in_ancestor_targets(&executable, &file) {
                return Ok(candidate);
            }
        }
        return Err(format!(
            "ryoko-lsp was not found next to {}; build it with \
             `cargo build -p ryoko-native --bin ryoko-lsp` and point RYOKO_LSP_BIN at it",
            current_executable_hint()
        ));
    }
    // Registry commands are bare names resolved through PATH; spawn fails
    // loudly later if the operator never installed the server.
    Ok(PathBuf::from(&file))
}

/// Looks for `target/debug/<file>` or `target/release/<file>` in every
/// ancestor directory of `start`, nearest first.
fn find_in_ancestor_targets(start: &Path, file: &str) -> Option<PathBuf> {
    start.ancestors().skip(1).find_map(|directory| {
        ["debug", "release"]
            .iter()
            .map(|profile| directory.join("target").join(profile).join(file))
            .find(|candidate| candidate.is_file())
    })
}

fn current_executable_hint() -> String {
    env::current_exe()
        .map(|path| path.display().to_string())
        .unwrap_or_else(|_| "the Ryoko executable".to_owned())
}

fn stop_one(children: &mut HashMap<String, Child>, id: &str) {
    if let Some(mut child) = children.remove(id) {
        let _ = child.kill();
        let _ = child.wait();
    }
}

fn stop_all(state: &LspState) {
    let mut children = state
        .children
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    for id in children.keys().cloned().collect::<Vec<_>>() {
        stop_one(&mut children, &id);
    }
}

pub fn shutdown(state: &LspState) {
    stop_all(state);
}

#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct LspStartResult {
    pub started: Vec<String>,
    pub failed: Vec<String>,
}

/// Starts `ryoko-lsp` plus every registry server, in the open workspace.
/// A standard server that is not installed never breaks the shell: it is
/// reported as failed and everything else keeps running.
#[tauri::command]
pub fn lsp_start(
    app: AppHandle,
    state: State<'_, LspState>,
    workspace: State<'_, WorkspaceState>,
    root: String,
    allow_external_servers: bool,
) -> Result<LspStartResult, String> {
    stop_all(&state);
    let root = PathBuf::from(&root)
        .canonicalize()
        .map_err(|error| format!("cannot open the language-server workspace: {error}"))?;
    if !root.is_dir() {
        return Err("language-server workspace is not a directory".to_owned());
    }
    // Only the workspace explicitly opened in the native shell may be used as
    // an LSP process working directory. Never let webview input choose a new root.
    let root = {
        let opened = current_root(&workspace)?;
        if root != opened {
            return Err("language-server root does not match the open workspace".to_owned());
        }
        root
    };
    let registry = LspRegistry::load(&root)?;

    let mut started = Vec::new();
    let mut failed = Vec::new();
    let mut targets: Vec<(String, PathBuf, Vec<String>)> = vec![(
        RYOKO_LSP_ID.to_owned(),
        resolve_binary(RYOKO_LSP_ID)?,
        Vec::new(),
    )];
    // Opening an untrusted repository must not execute its configured
    // language-server commands without an explicit operator approval.
    for server in registry.servers.iter().filter(|_| allow_external_servers) {
        match resolve_binary(&server.command) {
            Ok(binary) => targets.push((server.id.clone(), binary, server.args.clone())),
            Err(_) => failed.push(format!(
                "{}: command '{}' not found on PATH",
                server.id, server.command
            )),
        }
    }

    let mut children = state
        .children
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    for (id, binary, args) in targets {
        match spawn_server(&app, &root, &id, &binary, &args) {
            Ok(child) => {
                children.insert(id.clone(), child);
                started.push(id);
            }
            Err(error) => failed.push(format!("{id}: {error}")),
        }
    }
    if !children.contains_key(RYOKO_LSP_ID) {
        return Err("ryoko-lsp could not start; see the language-server log".to_owned());
    }
    Ok(LspStartResult { started, failed })
}

fn spawn_server(
    app: &AppHandle,
    root: &Path,
    id: &str,
    binary: &Path,
    args: &[String],
) -> Result<Child, String> {
    let mut command = Command::new(binary);
    command
        .current_dir(crate::paths::host_path(root))
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = command
        .spawn()
        .map_err(|error| format!("cannot start {id} at {}: {error}", binary.display()))?;

    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| format!("{id} stdout is unavailable"))?;
    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| format!("{id} stderr is unavailable"))?;

    let output_app = app.clone();
    let output_id = id.to_owned();
    thread::spawn(move || {
        let mut reader = BufReader::new(stdout);
        loop {
            match read_frame(&mut reader) {
                Ok(Some(bytes)) => match String::from_utf8(bytes) {
                    Ok(message) => {
                        let _ = output_app.emit(
                            "lsp-frame",
                            FrameEvent {
                                server: output_id.clone(),
                                message,
                            },
                        );
                    }
                    Err(_) => {
                        let _ = output_app.emit(
                            "lsp-log",
                            LogEvent {
                                server: output_id.clone(),
                                line: "non-UTF-8 frame".to_owned(),
                            },
                        );
                        break;
                    }
                },
                Ok(None) => break,
                Err(error) => {
                    let _ = output_app.emit(
                        "lsp-log",
                        LogEvent {
                            server: output_id.clone(),
                            line: format!("protocol error: {error}"),
                        },
                    );
                    break;
                }
            }
        }
        let _ = output_app.emit("lsp-exit", ExitEvent { server: output_id });
    });

    let log_app = app.clone();
    let log_id = id.to_owned();
    thread::spawn(move || {
        let reader = BufReader::new(stderr);
        for line in reader.lines().map_while(Result::ok) {
            let _ = log_app.emit(
                "lsp-log",
                LogEvent {
                    server: log_id.clone(),
                    line,
                },
            );
        }
    });
    Ok(child)
}

#[derive(Serialize, Clone)]
struct FrameEvent {
    server: String,
    message: String,
}

#[derive(Serialize, Clone)]
struct LogEvent {
    server: String,
    line: String,
}

#[derive(Serialize, Clone)]
struct ExitEvent {
    server: String,
}

#[tauri::command]
pub fn lsp_send(state: State<'_, LspState>, server: String, message: String) -> Result<(), String> {
    if server != RYOKO_LSP_ID && server.is_empty() {
        return Err("language-server id is empty".to_owned());
    }
    if message.len() > MAX_FRAME_BYTES {
        return Err("outbound language-server message exceeds size limit".to_owned());
    }
    serde_json::from_str::<serde_json::Value>(&message)
        .map_err(|error| format!("invalid LSP message: {error}"))?;
    let mut children = state
        .children
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    let child = children
        .get_mut(&server)
        .ok_or_else(|| format!("language server {server} is not running"))?;
    let stdin = child
        .stdin
        .as_mut()
        .ok_or_else(|| "language-server stdin is closed".to_owned())?;
    stdin
        .write_all(&frame(&message))
        .and_then(|()| stdin.flush())
        .map_err(|error| format!("cannot write to {server}: {error}"))
}

#[tauri::command]
pub fn lsp_stop(state: State<'_, LspState>, server: Option<String>) -> Result<(), String> {
    let mut children = state
        .children
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    match server {
        Some(id) => stop_one(&mut children, &id),
        None => {
            for id in children.keys().cloned().collect::<Vec<_>>() {
                stop_one(&mut children, &id);
            }
        }
    }
    Ok(())
}

/// The validated registry for the workspace, for the webview multiplexer.
#[tauri::command]
pub fn lsp_registry(state: State<'_, WorkspaceState>) -> Result<LspServerInfo, String> {
    let root = current_root(&state)?;
    let registry = LspRegistry::load(&root)?;
    let registry_path = registry_path(&root)
        .exists()
        .then(|| registry_path(&root).display().to_string());
    Ok(LspServerInfo {
        registry,
        registry_path,
    })
}

#[cfg(test)]
mod resolve_tests {
    use super::find_in_ancestor_targets;
    use std::fs;

    #[test]
    fn finds_workspace_build_from_a_nested_desktop_target() {
        let root = std::env::temp_dir().join(format!("ryoko-lsp-resolve-{}", std::process::id()));
        let workspace_bin = root.join("target/debug");
        let desktop_bin = root.join("apps/desktop/src-tauri/target/debug");
        fs::create_dir_all(&workspace_bin).unwrap();
        fs::create_dir_all(&desktop_bin).unwrap();
        fs::write(workspace_bin.join("ryoko-lsp"), b"").unwrap();
        let exe = desktop_bin.join("ryoko-desktop");
        assert_eq!(
            find_in_ancestor_targets(&exe, "ryoko-lsp"),
            Some(workspace_bin.join("ryoko-lsp"))
        );
        assert_eq!(find_in_ancestor_targets(&exe, "missing-binary"), None);
        fs::remove_dir_all(&root).unwrap();
    }
}
