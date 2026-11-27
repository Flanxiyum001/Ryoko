//! Minimal Debug Adapter Protocol bridge.
//!
//! Ryoko does not emulate a debugger. It starts a user-selected DAP adapter
//! (for example `dlv dap`) over stdio, forwards framed JSON messages both ways,
//! and leaves launch/breakpoint semantics to the adapter. Every message is
//! size-bounded and process arguments are never sourced from model output.

use std::io::{BufRead, BufReader, Write};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};
use std::thread;

use serde::Serialize;
use tauri::{AppHandle, Emitter, State};

use crate::lsp_frame::{MAX_FRAME_BYTES, frame, read_frame};
use crate::paths::host_path;
use crate::workspace::{WorkspaceState, current_root};

const MAX_MESSAGE_BYTES: usize = 1024 * 1024;
const MAX_ARGS: usize = 32;
const MAX_STDERR_LINE_BYTES: usize = 8 * 1024;

#[derive(Default)]
pub struct DebugState {
    session: Mutex<Option<DapSession>>,
    next_id: AtomicU64,
}

struct DapSession {
    id: u64,
    child: Child,
    stdin: ChildStdin,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct DapEvent {
    session_id: u64,
    message: String,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct DapLog {
    session_id: u64,
    message: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DapStatus {
    pub running: bool,
    pub session_id: Option<u64>,
}

fn stop_session(state: &DebugState) {
    let session = {
        let mut guard = state
            .session
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        guard.take()
    };
    if let Some(mut session) = session {
        let _ = session.child.kill();
        let _ = session.child.wait();
    }
}

pub fn shutdown(state: &DebugState) {
    stop_session(state);
}

pub fn validate_adapter(adapter: &str) -> Result<String, String> {
    let trimmed = adapter.trim().to_owned();
    if trimmed.is_empty() || trimmed.len() > 300 || trimmed.chars().any(char::is_control) {
        return Err("invalid debug adapter executable".to_owned());
    }
    Ok(trimmed)
}

pub fn validate_args(args: &[String]) -> Result<(), String> {
    if args.len() > MAX_ARGS
        || args
            .iter()
            .any(|argument| argument.len() > 1_000 || argument.chars().any(char::is_control))
    {
        return Err("invalid debug adapter arguments".to_owned());
    }
    Ok(())
}

pub fn validate_message(message: &str) -> Result<(), String> {
    if message.len() > MAX_MESSAGE_BYTES {
        return Err("debug adapter message exceeds 1 MiB".to_owned());
    }
    let value: serde_json::Value = serde_json::from_str(message)
        .map_err(|error| format!("invalid debug adapter message: {error}"))?;
    if !value.is_object() {
        return Err("debug adapter message must be a JSON object".to_owned());
    }
    Ok(())
}

#[tauri::command]
pub fn dap_start(
    app: AppHandle,
    state: State<'_, DebugState>,
    workspace: State<'_, WorkspaceState>,
    adapter: String,
    args: Option<Vec<String>>,
) -> Result<DapStatus, String> {
    stop_session(&state);
    let root = host_path(&current_root(&workspace)?);
    let adapter = validate_adapter(&adapter)?;
    let args = args.unwrap_or_default();
    validate_args(&args)?;
    let mut child = Command::new(&adapter)
        .args(&args)
        .current_dir(root)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|error| format!("cannot start debug adapter {adapter}: {error}"))?;
    let stdin = child
        .stdin
        .take()
        .ok_or_else(|| "debug adapter stdin is unavailable".to_owned())?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| "debug adapter stdout is unavailable".to_owned())?;
    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| "debug adapter stderr is unavailable".to_owned())?;
    let id = state
        .next_id
        .fetch_add(1, Ordering::SeqCst)
        .saturating_add(1);
    *state
        .session
        .lock()
        .unwrap_or_else(|error| error.into_inner()) = Some(DapSession { id, child, stdin });

    let output_app = app.clone();
    thread::spawn(move || {
        let mut reader = BufReader::new(stdout);
        loop {
            match read_frame(&mut reader) {
                Ok(Some(bytes)) => match String::from_utf8(bytes) {
                    Ok(message) => {
                        if output_app
                            .emit(
                                "ryoko-dap-message",
                                DapEvent {
                                    session_id: id,
                                    message,
                                },
                            )
                            .is_err()
                        {
                            break;
                        }
                    }
                    Err(_) => {
                        let _ = output_app.emit(
                            "ryoko-dap-log",
                            DapLog {
                                session_id: id,
                                message: "debug adapter emitted non-UTF-8".to_owned(),
                            },
                        );
                        break;
                    }
                },
                Ok(None) => break,
                Err(error) => {
                    let _ = output_app.emit(
                        "ryoko-dap-log",
                        DapLog {
                            session_id: id,
                            message: format!("debug adapter protocol error: {error}"),
                        },
                    );
                    break;
                }
            }
        }
    });
    thread::spawn(move || {
        let mut reader = BufReader::new(stderr);
        let mut line = Vec::new();
        let mut truncated = false;
        loop {
            let (take, complete, empty) = match reader.fill_buf() {
                Ok([]) => (0, false, true),
                Ok(buffer) => {
                    let complete = buffer.iter().position(|byte| *byte == b'\n');
                    let take = complete.map_or(buffer.len(), |position| position + 1);
                    let available = MAX_STDERR_LINE_BYTES.saturating_sub(line.len());
                    line.extend_from_slice(&buffer[..take.min(available)]);
                    if take > available {
                        truncated = true;
                    }
                    (take, complete.is_some(), false)
                }
                Err(_) => break,
            };
            if empty {
                break;
            }
            reader.consume(take);
            if complete {
                let mut message = String::from_utf8_lossy(&line).trim_end().to_owned();
                if truncated {
                    message.push_str(" [truncated]");
                }
                let _ = app.emit(
                    "ryoko-dap-log",
                    DapLog {
                        session_id: id,
                        message,
                    },
                );
                line.clear();
                truncated = false;
            }
        }
        if !line.is_empty() || truncated {
            let mut message = String::from_utf8_lossy(&line).trim_end().to_owned();
            if truncated {
                message.push_str(" [truncated]");
            }
            let _ = app.emit(
                "ryoko-dap-log",
                DapLog {
                    session_id: id,
                    message,
                },
            );
        }
    });

    Ok(DapStatus {
        running: true,
        session_id: Some(id),
    })
}

#[tauri::command]
pub fn dap_send(message: String, state: State<'_, DebugState>) -> Result<(), String> {
    validate_message(&message)?;
    let bytes = frame(&message);
    if bytes.len() > MAX_FRAME_BYTES {
        return Err("debug adapter frame exceeds the limit".to_owned());
    }
    let mut guard = state
        .session
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    let session = guard
        .as_mut()
        .ok_or_else(|| "the debug adapter is not running".to_owned())?;
    session
        .stdin
        .write_all(&bytes)
        .and_then(|_| session.stdin.flush())
        .map_err(|error| format!("debug adapter write failed: {error}"))
}

#[tauri::command]
pub fn dap_status(state: State<'_, DebugState>) -> Result<DapStatus, String> {
    let mut guard = state
        .session
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    let Some(session) = guard.as_mut() else {
        return Ok(DapStatus {
            running: false,
            session_id: None,
        });
    };
    let running = session
        .child
        .try_wait()
        .map_err(|error| format!("cannot inspect debug adapter: {error}"))?
        .is_none();
    if !running {
        guard.take();
        return Ok(DapStatus {
            running: false,
            session_id: None,
        });
    }
    Ok(DapStatus {
        running,
        session_id: Some(session.id),
    })
}

#[tauri::command]
pub fn dap_stop(state: State<'_, DebugState>) -> Result<(), String> {
    stop_session(&state);
    Ok(())
}

#[tauri::command]
pub fn dap_restart(
    app: AppHandle,
    state: State<'_, DebugState>,
    workspace: State<'_, WorkspaceState>,
    adapter: String,
    args: Option<Vec<String>>,
) -> Result<DapStatus, String> {
    dap_start(app, state, workspace, adapter, args)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_validate_adapter() {
        assert!(validate_adapter("dlv").is_ok());
        assert!(validate_adapter("  python3  ").is_ok());
        assert_eq!(validate_adapter("  python3  ").unwrap(), "python3");

        assert!(validate_adapter("").is_err());
        assert!(validate_adapter("   ").is_err());
        assert!(validate_adapter(&"a".repeat(301)).is_err());
        assert!(validate_adapter("adapter\nwith\nnewline").is_err());
        assert!(validate_adapter("adapter\0null").is_err());
    }

    #[test]
    fn test_validate_args() {
        assert!(validate_args(&[]).is_ok());
        assert!(validate_args(&["dap".to_owned(), "--log".to_owned()]).is_ok());

        let too_many: Vec<String> = (0..33).map(|i| format!("arg{i}")).collect();
        assert!(validate_args(&too_many).is_err());

        let long_arg = vec!["x".repeat(1001)];
        assert!(validate_args(&long_arg).is_err());

        let ctrl_arg = vec!["arg\nwith\nnewline".to_owned()];
        assert!(validate_args(&ctrl_arg).is_err());
    }

    #[test]
    fn test_validate_message() {
        assert!(validate_message(r#"{"type":"request","command":"initialize"}"#).is_ok());

        assert!(validate_message("").is_err());
        assert!(validate_message("not json").is_err());
        assert!(validate_message("42").is_err());
        assert!(validate_message(r#"["array"]"#).is_err());
    }

    #[test]
    fn test_initial_debug_state() {
        let state = DebugState::default();
        let guard = state.session.lock().unwrap();
        assert!(guard.is_none());
    }
}
