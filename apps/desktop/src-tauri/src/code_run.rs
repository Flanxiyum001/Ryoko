//! User-approved, non-interactive language tasks. Unlike the integrated PTY,
//! a run is direct argv execution (no shell interpretation) and cannot start
//! merely because a workspace was opened or an agent suggested a command.
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;

use serde::Serialize;
use tauri::{AppHandle, Emitter, State};
use tauri_plugin_dialog::{DialogExt, MessageDialogButtons};

use crate::paths::safe_join;
use crate::terminal_decode::TerminalUtf8Decoder;
use crate::workspace::{WorkspaceState, current_root};

const MAX_ARG_BYTES: usize = 2048;
const MAX_ARGS: usize = 32;
const MAX_OUTPUT_BYTES: usize = 2 * 1024 * 1024;

#[derive(Default)]
pub struct CodeRunState {
    active: Mutex<Option<ActiveTask>>,
    next_id: AtomicU64,
}

struct ActiveTask {
    id: u64,
    root: PathBuf,
    child: Child,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CodeOutput {
    session_id: u64,
    stream: &'static str,
    data: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CodeRunStatus {
    running: bool,
    session_id: Option<u64>,
    exit_code: Option<i32>,
}

impl CodeRunStatus {
    fn idle() -> Self {
        Self {
            running: false,
            session_id: None,
            exit_code: None,
        }
    }
}

fn target_file(root: &Path, relative: &str) -> Result<PathBuf, String> {
    let candidate = safe_join(root, relative)?;
    let target = candidate
        .canonicalize()
        .map_err(|error| format!("cannot run {relative}: {error}"))?;
    if !target.starts_with(root) || !target.is_file() {
        return Err("task target must be a regular file inside the open workspace".to_owned());
    }
    Ok(target)
}

fn prepare_argv(
    program: &str,
    args: &[String],
    file: &Path,
    root: &Path,
) -> Result<Vec<String>, String> {
    if program.trim().is_empty()
        || program.len() > MAX_ARG_BYTES
        || program.chars().any(char::is_control)
    {
        return Err(
            "invalid task executable; enter an executable path or name, not a shell command"
                .to_owned(),
        );
    }
    if args.len() > MAX_ARGS {
        return Err(format!("task accepts at most {MAX_ARGS} arguments"));
    }
    let mut result = Vec::with_capacity(args.len());
    for arg in args {
        if arg.len() > MAX_ARG_BYTES || arg.chars().any(char::is_control) {
            return Err("invalid task argument (maximum 2048 bytes, no line breaks)".to_owned());
        }
        result.push(match arg.as_str() {
            "{file}" => file.to_string_lossy().into_owned(),
            "{workspace}" => root.to_string_lossy().into_owned(),
            _ => arg.clone(),
        });
    }
    Ok(result)
}

fn relay<R: Read + Send + 'static>(
    mut input: R,
    app: AppHandle,
    session_id: u64,
    stream: &'static str,
    emitted: Arc<AtomicUsize>,
    truncated: Arc<AtomicBool>,
) {
    thread::spawn(move || {
        let mut decoder = TerminalUtf8Decoder::default();
        let mut chunk = [0u8; 4096];
        loop {
            let read = match input.read(&mut chunk) {
                Ok(0) | Err(_) => break,
                Ok(read) => read,
            };
            let old = emitted.fetch_add(read, Ordering::Relaxed);
            let allowed = read.min(MAX_OUTPUT_BYTES.saturating_sub(old));
            if allowed > 0 {
                let data = decoder.push(&chunk[..allowed]);
                if !data.is_empty() {
                    let _ = app.emit(
                        "ryoko-code-output",
                        CodeOutput {
                            session_id,
                            stream,
                            data,
                        },
                    );
                }
            }
            if allowed < read && !truncated.swap(true, Ordering::SeqCst) {
                let _ = app.emit(
                    "ryoko-code-output",
                    CodeOutput {
                        session_id,
                        stream: "system",
                        data: "\n[Output truncated at 2 MiB. Process continues.]\n".to_owned(),
                    },
                );
            }
        }
        let remaining = decoder.finish();
        if !remaining.is_empty() {
            let _ = app.emit(
                "ryoko-code-output",
                CodeOutput {
                    session_id,
                    stream,
                    data: remaining,
                },
            );
        }
    });
}

fn inspect_task(active: &mut Option<ActiveTask>) -> Result<CodeRunStatus, String> {
    let Some(task) = active.as_mut() else {
        return Ok(CodeRunStatus::idle());
    };
    match task.child.try_wait() {
        Ok(None) => Ok(CodeRunStatus {
            running: true,
            session_id: Some(task.id),
            exit_code: None,
        }),
        Ok(Some(exit)) => {
            let id = task.id;
            *active = None;
            Ok(CodeRunStatus {
                running: false,
                session_id: Some(id),
                exit_code: exit.code(),
            })
        }
        Err(error) => Err(format!("cannot inspect code task: {error}")),
    }
}

/// The OS confirmation cannot be bypassed by an untrusted renderer or agent.
/// The user can run any installed toolchain but no commands run on open.
#[tauri::command]
pub async fn code_run_start(
    path: String,
    program: String,
    args: Vec<String>,
    app: AppHandle,
    state: State<'_, CodeRunState>,
    workspace: State<'_, WorkspaceState>,
) -> Result<CodeRunStatus, String> {
    let root = current_root(&workspace)?;
    let target = target_file(&root, &path)?;
    let argv = prepare_argv(&program, &args, &target, &root)?;
    {
        let mut guard = state.active.lock().unwrap_or_else(|e| e.into_inner());
        if inspect_task(&mut guard)?.running {
            return Err(
                "another code task is running; stop it before launching a new task".to_owned(),
            );
        }
    }

    let display = format!(
        "Executable: {}\nArguments: {}\nFile: {}\nWorkspace: {}\n\nThis runs project code with your local account permissions. Only approve trusted code. Ryoko will not invoke a shell.",
        program,
        argv.join(" "),
        target.display(),
        root.display(),
    );
    let prompt_app = app.clone();
    let approved = tauri::async_runtime::spawn_blocking(move || {
        prompt_app
            .dialog()
            .message(display)
            .title("Ryoko — approve code execution")
            .buttons(MessageDialogButtons::OkCancelCustom(
                "Run once".to_owned(),
                "Cancel".to_owned(),
            ))
            .blocking_show()
    })
    .await
    .map_err(|_| "could not show code execution approval".to_owned())?;
    if !approved {
        return Err("code execution cancelled by user".to_owned());
    }
    if current_root(&workspace)? != root {
        return Err("workspace changed while approving the task".to_owned());
    }
    // Check symlinks again after the prompt, before launching.
    if target_file(&root, &path)? != target {
        return Err("target file changed while approving the task".to_owned());
    }
    let mut guard = state.active.lock().unwrap_or_else(|e| e.into_inner());
    if inspect_task(&mut guard)?.running {
        return Err("another code task started while approval was open".to_owned());
    }
    let mut child = Command::new(&program)
        .args(&argv)
        .current_dir(&root)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|error| {
            format!(
                "cannot start {program}: {error}. Check that the toolchain is installed and on PATH"
            )
        })?;

    let id = state
        .next_id
        .fetch_add(1, Ordering::SeqCst)
        .saturating_add(1);
    let emitted = Arc::new(AtomicUsize::new(0));
    let truncated = Arc::new(AtomicBool::new(false));
    if let Some(stdout) = child.stdout.take() {
        relay(
            stdout,
            app.clone(),
            id,
            "stdout",
            emitted.clone(),
            truncated.clone(),
        );
    }
    if let Some(stderr) = child.stderr.take() {
        relay(stderr, app, id, "stderr", emitted, truncated);
    }
    *guard = Some(ActiveTask { id, root, child });
    Ok(CodeRunStatus {
        running: true,
        session_id: Some(id),
        exit_code: None,
    })
}

#[tauri::command]
pub fn code_run_status(
    state: State<'_, CodeRunState>,
    workspace: State<'_, WorkspaceState>,
) -> Result<CodeRunStatus, String> {
    let root = current_root(&workspace)?;
    let mut guard = state.active.lock().unwrap_or_else(|e| e.into_inner());
    if guard.as_ref().is_some_and(|task| task.root != root) {
        if let Some(task) = guard.as_mut() {
            let _ = task.child.kill();
            let _ = task.child.wait();
        }
        *guard = None;
        return Ok(CodeRunStatus::idle());
    }
    inspect_task(&mut guard)
}

#[tauri::command]
pub fn code_run_stop(state: State<'_, CodeRunState>) -> Result<(), String> {
    let mut guard = state.active.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(mut task) = guard.take() {
        task.child
            .kill()
            .map_err(|error| format!("cannot stop code task: {error}"))?;
        let _ = task.child.wait();
    }
    Ok(())
}

pub fn shutdown(state: &CodeRunState) {
    let mut guard = state.active.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(mut task) = guard.take() {
        let _ = task.child.kill();
        let _ = task.child.wait();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn expands_only_exact_placeholders_and_preserves_literals() {
        let file = Path::new("/workspace/src/main.py");
        let root = Path::new("/workspace");
        let args = vec![
            "{file}".to_owned(),
            "{workspace}".to_owned(),
            "$(touch /tmp/evil)".to_owned(),
            "{file}.backup".to_owned(),
        ];
        assert_eq!(
            prepare_argv("python3", &args, file, root).unwrap(),
            vec![
                "/workspace/src/main.py",
                "/workspace",
                "$(touch /tmp/evil)",
                "{file}.backup"
            ]
        );
    }

    #[test]
    fn refuses_invalid_executables_and_excessive_arguments() {
        let path = Path::new("/workspace/a.py");
        assert!(prepare_argv("python\nwhoami", &[], path, path).is_err());
        assert!(prepare_argv("", &[], path, path).is_err());
        assert!(prepare_argv("python", &vec!["x".to_owned(); 33], path, path).is_err());
        assert!(prepare_argv("python", &["bad\0value".to_owned()], path, path).is_err());
        assert!(prepare_argv("python", &["\u{1b}[2Jspoof".to_owned()], path, path).is_err());

    #[cfg(unix)]
    #[test]
    fn refuses_workspace_symlink_escape() {
        use std::os::unix::fs::symlink;
        use std::time::{SystemTime, UNIX_EPOCH};
        let suffix = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = std::env::temp_dir().join(format!("ryoko-run-target-{}-{suffix}", std::process::id()));
        std::fs::create_dir(&root).unwrap();
        let outside = root.with_extension("outside");
        std::fs::write(&outside, "print('hello')").unwrap();
        symlink(&outside, root.join("escape.py")).unwrap();
        assert!(target_file(&root, "escape.py").is_err());
        assert!(target_file(&root, "../escape.py").is_err());
        std::fs::remove_file(root.join("escape.py")).unwrap();
        std::fs::remove_file(outside).unwrap();
        std::fs::remove_dir(root).unwrap();
    }
    }
}
