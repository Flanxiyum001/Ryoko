//! PTY-backed integrated terminal.
//!
//! The shell is started only after an explicit UI action, inside the currently
//! opened workspace. Commands and working directories never come from model
//! output. Output is streamed as bounded UTF-8 text events to xterm.js.

use std::io::{Read, Write};
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};
use std::thread;

use portable_pty::{Child, CommandBuilder, MasterPty, PtySize, native_pty_system};
use serde::Serialize;
use tauri::{AppHandle, Emitter, State};

use crate::terminal_decode::TerminalUtf8Decoder;
use crate::workspace::{WorkspaceState, current_root};

const MAX_INPUT_BYTES: usize = 32 * 1024;
const READ_BUFFER: usize = 16 * 1024;

#[derive(Default)]
pub struct TerminalState {
    session: Mutex<Option<TerminalSession>>,
    next_id: AtomicU64,
}

struct TerminalSession {
    id: u64,
    master: Box<dyn MasterPty + Send>,
    writer: Box<dyn Write + Send>,
    child: Box<dyn Child + Send + Sync>,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct TerminalOutput {
    session_id: u64,
    data: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TerminalStatus {
    pub running: bool,
    pub session_id: Option<u64>,
}

fn default_shell() -> String {
    if cfg!(windows) {
        "powershell.exe".to_owned()
    } else {
        std::env::var("SHELL")
            .ok()
            .filter(|shell| shell.starts_with('/') && !shell.chars().any(char::is_control))
            .unwrap_or_else(|| "/bin/sh".to_owned())
    }
}

fn stop_session(state: &TerminalState) {
    let mut guard = state
        .session
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    if let Some(mut session) = guard.take() {
        let _ = session.child.kill();
        let _ = session.child.wait();
    }
}

pub fn shutdown(state: &TerminalState) {
    stop_session(state);
}

#[tauri::command]
pub fn terminal_start(
    app: AppHandle,
    state: State<'_, TerminalState>,
    workspace: State<'_, WorkspaceState>,
    shell: Option<String>,
) -> Result<TerminalStatus, String> {
    stop_session(&state);
    let root = current_root(&workspace)?;
    let shell = shell
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty())
        .unwrap_or_else(default_shell);
    if shell.len() > 300 || shell.chars().any(char::is_control) {
        return Err("invalid shell executable".to_owned());
    }
    let pty = native_pty_system()
        .openpty(PtySize {
            rows: 30,
            cols: 100,
            pixel_width: 0,
            pixel_height: 0,
        })
        .map_err(|error| format!("cannot open a terminal PTY: {error}"))?;
    let mut command = CommandBuilder::new(&shell);
    command.cwd(root.as_os_str());
    command.env("TERM", "xterm-256color");
    command.env("COLORTERM", "truecolor");
    if shell.to_ascii_lowercase().contains("powershell")
        || shell.to_ascii_lowercase().contains("pwsh")
    {
        command.arg("-NoLogo");
    }
    let child = pty
        .slave
        .spawn_command(command)
        .map_err(|error| format!("cannot start {shell}: {error}"))?;
    drop(pty.slave);
    let reader = pty
        .master
        .try_clone_reader()
        .map_err(|error| format!("cannot read terminal PTY: {error}"))?;
    let writer = pty
        .master
        .take_writer()
        .map_err(|error| format!("cannot write terminal PTY: {error}"))?;
    let id = state
        .next_id
        .fetch_add(1, Ordering::SeqCst)
        .saturating_add(1);
    *state
        .session
        .lock()
        .unwrap_or_else(|error| error.into_inner()) = Some(TerminalSession {
        id,
        master: pty.master,
        writer,
        child,
    });

    thread::spawn(move || {
        let mut reader = reader;
        let mut buffer = [0u8; READ_BUFFER];
        let mut decoder = TerminalUtf8Decoder::default();
        loop {
            match reader.read(&mut buffer) {
                Ok(0) => break,
                Ok(read) => {
                    let data = decoder.push(&buffer[..read]);
                    if !data.is_empty()
                        && app
                            .emit(
                                "ryoko-terminal-output",
                                TerminalOutput {
                                    session_id: id,
                                    data,
                                },
                            )
                            .is_err()
                    {
                        return;
                    }
                }
                Err(_) => break,
            }
        }
        let trailing = decoder.finish();
        if !trailing.is_empty() {
            let _ = app.emit(
                "ryoko-terminal-output",
                TerminalOutput {
                    session_id: id,
                    data: trailing,
                },
            );
        }
    });

    Ok(TerminalStatus {
        running: true,
        session_id: Some(id),
    })
}

#[tauri::command]
pub fn terminal_write(data: String, state: State<'_, TerminalState>) -> Result<(), String> {
    if data.len() > MAX_INPUT_BYTES {
        return Err("terminal input exceeds 32 KiB".to_owned());
    }
    let mut guard = state
        .session
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    let session = guard
        .as_mut()
        .ok_or_else(|| "the terminal is not running".to_owned())?;
    session
        .writer
        .write_all(data.as_bytes())
        .and_then(|_| session.writer.flush())
        .map_err(|error| format!("terminal write failed: {error}"))
}

#[tauri::command]
pub fn terminal_resize(
    rows: u16,
    cols: u16,
    state: State<'_, TerminalState>,
) -> Result<(), String> {
    let guard = state
        .session
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    let session = guard
        .as_ref()
        .ok_or_else(|| "the terminal is not running".to_owned())?;
    session
        .master
        .resize(PtySize {
            rows: rows.clamp(2, 500),
            cols: cols.clamp(20, 500),
            pixel_width: 0,
            pixel_height: 0,
        })
        .map_err(|error| format!("terminal resize failed: {error}"))
}

#[tauri::command]
pub fn terminal_status(state: State<'_, TerminalState>) -> Result<TerminalStatus, String> {
    let mut guard = state
        .session
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    let Some(session) = guard.as_mut() else {
        return Ok(TerminalStatus {
            running: false,
            session_id: None,
        });
    };
    let running = session
        .child
        .try_wait()
        .map_err(|error| format!("cannot inspect terminal: {error}"))?
        .is_none();
    if !running {
        guard.take();
        return Ok(TerminalStatus {
            running: false,
            session_id: None,
        });
    }
    Ok(TerminalStatus {
        running,
        session_id: Some(session.id),
    })
}

#[tauri::command]
pub fn terminal_stop(state: State<'_, TerminalState>) -> Result<(), String> {
    stop_session(&state);
    Ok(())
}
