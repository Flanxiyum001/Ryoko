use std::env;
use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::Mutex;
use std::thread;

use tauri::{AppHandle, Emitter, State};

use crate::lsp_frame::{MAX_FRAME_BYTES, frame, read_frame};
use crate::workspace::{WorkspaceState, current_root};

#[derive(Default)]
pub struct LspState {
    child: Mutex<Option<Child>>,
}

fn resolve_binary() -> Result<PathBuf, String> {
    if let Some(path) = env::var_os("RYOKO_LSP_BIN") {
        return Ok(PathBuf::from(path));
    }
    let executable = env::current_exe()
        .map_err(|error| format!("cannot resolve the Ryoko executable: {error}"))?;
    if let Some(directory) = executable.parent() {
        for name in ["ryoko-lsp", "ryoko-lsp.exe"] {
            let candidate = directory.join(name);
            if candidate.is_file() {
                return Ok(candidate);
            }
        }
    }
    Err(format!(
        "ryoko-lsp was not found next to {}; build it with `cargo build -p ryoko-native --bin ryoko-lsp` and point RYOKO_LSP_BIN at it",
        executable.display()
    ))
}

fn stop_child(state: &LspState) -> Result<(), String> {
    let mut guard = state
        .child
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    if let Some(mut child) = guard.take() {
        let _ = child.kill();
        let _ = child.wait();
    }
    Ok(())
}

pub fn shutdown(state: &LspState) {
    let _ = stop_child(state);
}

#[tauri::command]
pub fn lsp_start(
    app: AppHandle,
    state: State<'_, LspState>,
    workspace: State<'_, WorkspaceState>,
    root: String,
) -> Result<(), String> {
    stop_child(&state)?;
    let binary = resolve_binary()?;
    let root = PathBuf::from(&root)
        .canonicalize()
        .map_err(|error| format!("cannot open the language-server workspace: {error}"))?;
    if !root.is_dir() {
        return Err("language-server workspace is not a directory".to_owned());
    }
    // Only the workspace explicitly opened in the native shell may be used as
    // an LSP process working directory. Never let webview input choose a new root.
    if root != current_root(&workspace)? {
        return Err("language-server root does not match the open workspace".to_owned());
    }
    let mut child = Command::new(&binary)
        .current_dir(&root)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|error| {
            format!(
                "cannot start ryoko-lsp at {}: {error}. Build it with `cargo build -p ryoko-native --bin ryoko-lsp` or set RYOKO_LSP_BIN.",
                binary.display()
            )
        })?;

    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| "ryoko-lsp stdout is unavailable".to_owned())?;
    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| "ryoko-lsp stderr is unavailable".to_owned())?;

    let output_app = app.clone();
    thread::spawn(move || {
        let mut reader = BufReader::new(stdout);
        loop {
            match read_frame(&mut reader) {
                Ok(Some(bytes)) => match String::from_utf8(bytes) {
                    Ok(message) => {
                        let _ = output_app.emit("ryoko-lsp-message", message);
                    }
                    Err(_) => {
                        let _ =
                            output_app.emit("ryoko-lsp-log", "ryoko-lsp emitted a non-UTF-8 frame");
                        break;
                    }
                },
                Ok(None) => break,
                Err(error) => {
                    let _ = output_app.emit(
                        "ryoko-lsp-log",
                        format!("ryoko-lsp protocol error: {error}"),
                    );
                    break;
                }
            }
        }
        let _ = output_app.emit("ryoko-lsp-closed", ());
    });

    let log_app = app;
    thread::spawn(move || {
        let reader = BufReader::new(stderr);
        for line in reader.lines().map_while(Result::ok) {
            let _ = log_app.emit("ryoko-lsp-log", line);
        }
    });

    *state
        .child
        .lock()
        .unwrap_or_else(|error| error.into_inner()) = Some(child);
    Ok(())
}

#[tauri::command]
pub fn lsp_send(state: State<'_, LspState>, message: String) -> Result<(), String> {
    if message.len() > MAX_FRAME_BYTES {
        return Err("outbound language-server message exceeds size limit".to_owned());
    }
    serde_json::from_str::<serde_json::Value>(&message)
        .map_err(|error| format!("invalid LSP message: {error}"))?;
    let mut guard = state
        .child
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    let child = guard
        .as_mut()
        .ok_or_else(|| "ryoko-lsp is not running".to_owned())?;
    let stdin = child
        .stdin
        .as_mut()
        .ok_or_else(|| "ryoko-lsp stdin is closed".to_owned())?;
    stdin
        .write_all(&frame(&message))
        .and_then(|()| stdin.flush())
        .map_err(|error| format!("cannot write to ryoko-lsp: {error}"))
}

#[tauri::command]
pub fn lsp_stop(state: State<'_, LspState>) -> Result<(), String> {
    stop_child(&state)
}
