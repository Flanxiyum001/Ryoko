//! Native session and worktree operations for the desktop sidebar.
//!
//! Reading a published branch is read-only Git plumbing. Release, discard and
//! merge run the existing `ryoko review` workflow with an explicit argv after a
//! native confirmation. Merge still requires a passing sandboxed pre-flight
//! check; this module never skips validation and never trusts model output as a
//! command.

use std::path::Path;
use std::process::{Command, Stdio};

use rusqlite::{Connection, OpenFlags, params};
use serde::Serialize;
use tauri::{AppHandle, State};
use tauri_plugin_dialog::{DialogExt, MessageDialogButtons};

use crate::paths::safe_join;
use crate::run::resolve_cli;
use crate::sessions_projection::session_state_path;
use crate::workspace::{WorkspaceState, current_root};

const MAX_FILES: usize = 500;
const MAX_NUMSTAT_BYTES: usize = 512 * 1024;
const MAX_FILE_BYTES: u64 = 256 * 1024;
const MAX_CLI_STDOUT: usize = 1024 * 1024;
const MAX_CLI_STDERR: usize = 8 * 1024;

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionChange {
    pub path: String,
    /// `added`, `deleted`, `modified` or `binary`.
    pub status: String,
    pub additions: u64,
    pub deletions: u64,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionFileDiff {
    pub path: String,
    pub status: String,
    pub original: String,
    pub modified: String,
}

#[derive(Debug, Clone)]
struct PublishedSession {
    branch: String,
    base_branch: String,
    files: Vec<String>,
}

pub(crate) fn valid_task_id(task_id: &str) -> bool {
    task_id.len() == 36
        && task_id
            .chars()
            .all(|character| character.is_ascii_hexdigit() || character == '-')
        && task_id
            .chars()
            .filter(|character| *character == '-')
            .count()
            == 4
}

pub(crate) fn valid_target_branch(branch: &str) -> bool {
    !branch.is_empty()
        && branch.len() <= 200
        && !branch.starts_with('-')
        && !branch.contains("..")
        && !branch.contains(' ')
        && branch.chars().all(|character| {
            character.is_ascii_alphanumeric() || matches!(character, '/' | '-' | '_' | '.')
        })
}

pub(crate) fn valid_sandbox_image(image: &str) -> bool {
    let image = image.trim();
    !image.is_empty()
        && image.len() <= 300
        && !image.starts_with('-')
        && !image.chars().any(char::is_whitespace)
        && image.chars().all(|character| {
            character.is_ascii_alphanumeric()
                || matches!(character, '/' | ':' | '@' | '.' | '_' | '-')
        })
}

pub(crate) fn valid_check_command(command: &str) -> bool {
    !command.trim().is_empty()
        && command.len() <= 2_000
        && !command.chars().any(|character| character.is_control())
}

fn published_session(root: &Path, task_id: &str) -> Result<PublishedSession, String> {
    if !valid_task_id(task_id) {
        return Err("invalid agent session identifier".to_owned());
    }
    let database = session_state_path(root)?
        .ok_or_else(|| "no agent session store is available".to_owned())?;
    let connection = Connection::open_with_flags(
        database,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .map_err(|error| format!("cannot open the agent session store: {error}"))?;
    let (branch, base_branch, state, files_json): (String, String, String, String) = connection
        .query_row(
            "SELECT branch_name, base_branch, state, files_json FROM worktree_sessions WHERE task_id=?1",
            params![task_id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )
        .map_err(|_| "agent session not found".to_owned())?;
    if state != "published" {
        return Err("only published agent sessions can be reviewed or merged".to_owned());
    }
    if !valid_target_branch(&branch)
        || (!base_branch.is_empty() && base_branch != "HEAD" && !valid_target_branch(&base_branch))
    {
        return Err("agent session has an invalid branch name".to_owned());
    }
    let files = serde_json::from_str::<Vec<String>>(&files_json)
        .map_err(|_| "invalid agent file manifest".to_owned())?;
    Ok(PublishedSession {
        branch,
        base_branch,
        files,
    })
}

fn default_target(session: &PublishedSession) -> String {
    if session.base_branch.is_empty() || session.base_branch == "HEAD" {
        "main".to_owned()
    } else {
        session.base_branch.clone()
    }
}

fn run_git(root: &Path, args: &[&str]) -> Result<Vec<u8>, String> {
    let output = Command::new("git")
        .current_dir(root)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .env("GIT_TERMINAL_PROMPT", "0")
        .output()
        .map_err(|error| format!("cannot run Git: {error}"))?;
    if !output.status.success() {
        return Err("Git could not complete the requested read".to_owned());
    }
    Ok(output.stdout)
}

fn merge_base(root: &Path, target: &str, branch: &str) -> Result<String, String> {
    let output = run_git(root, &["merge-base", target, branch])?;
    let base =
        String::from_utf8(output).map_err(|_| "Git returned a non-UTF-8 merge base".to_owned())?;
    let base = base.trim();
    if base.is_empty() {
        return Err("the agent branch has no merge base with the target".to_owned());
    }
    Ok(base.to_owned())
}

fn blob_exists(root: &Path, spec: &str) -> Result<bool, String> {
    let status = Command::new("git")
        .current_dir(root)
        .args(["cat-file", "-e", spec])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .env("GIT_TERMINAL_PROMPT", "0")
        .status()
        .map_err(|error| format!("cannot inspect a Git object: {error}"))?;
    Ok(status.success())
}

fn blob_text(root: &Path, spec: &str) -> Result<String, String> {
    let size = run_git(root, &["cat-file", "-s", spec])?;
    let size = String::from_utf8(size)
        .map_err(|_| "invalid Git object size".to_owned())?
        .trim()
        .parse::<u64>()
        .map_err(|_| "invalid Git object size".to_owned())?;
    if size > MAX_FILE_BYTES {
        return Err(format!(
            "{} exceeds the {} KiB review limit",
            spec,
            MAX_FILE_BYTES / 1024
        ));
    }
    let output = run_git(root, &["show", spec])?;
    if output.len() as u64 > MAX_FILE_BYTES {
        return Err("the proposed file exceeds the review limit".to_owned());
    }
    String::from_utf8(output).map_err(|_| "binary files are not editable in text review".to_owned())
}

pub(crate) fn parse_numstat(output: &str) -> Result<Vec<SessionChange>, String> {
    let mut changes = Vec::new();
    for line in output.lines() {
        if changes.len() >= MAX_FILES {
            break;
        }
        let mut parts = line.splitn(3, '\t');
        let Some(additions) = parts.next() else {
            continue;
        };
        let Some(deletions) = parts.next() else {
            continue;
        };
        let Some(path) = parts.next() else {
            continue;
        };
        if path.is_empty() {
            continue;
        }
        let binary = additions == "-" || deletions == "-";
        let additions = additions.parse::<u64>().unwrap_or(0);
        let deletions = deletions.parse::<u64>().unwrap_or(0);
        changes.push(SessionChange {
            path: path.to_owned(),
            status: if binary {
                "binary".to_owned()
            } else {
                "modified".to_owned()
            },
            additions,
            deletions,
        });
    }
    Ok(changes)
}

#[tauri::command]
pub async fn list_agent_session_changes(
    task_id: String,
    state: State<'_, WorkspaceState>,
) -> Result<Vec<SessionChange>, String> {
    let root = current_root(&state)?;
    tauri::async_runtime::spawn_blocking(move || {
        let session = published_session(&root, &task_id)?;
        let target = default_target(&session);
        if !valid_target_branch(&target) {
            return Err("the session base branch is not a valid branch name".to_owned());
        }
        let base = merge_base(&root, &target, &session.branch)?;
        let range = format!("{target}...{}", session.branch);
        let output = run_git(&root, &["diff", "--numstat", "--no-renames", &range, "--"])?;
        if output.len() > MAX_NUMSTAT_BYTES {
            return Err("the session diff is too large to list".to_owned());
        }
        let text = String::from_utf8_lossy(&output);
        let mut changes = parse_numstat(&text)?;
        for change in &mut changes {
            safe_join(&root, &change.path)?;
            let original = blob_exists(&root, &format!("{base}:{}", change.path)).unwrap_or(false);
            let modified =
                blob_exists(&root, &format!("{}:{}", session.branch, change.path)).unwrap_or(false);
            change.status = if change.status == "binary" {
                "binary".to_owned()
            } else if !original && modified {
                "added".to_owned()
            } else if original && !modified {
                "deleted".to_owned()
            } else {
                "modified".to_owned()
            };
        }
        Ok(changes)
    })
    .await
    .map_err(|_| "session diff worker failed".to_owned())?
}

#[tauri::command]
pub async fn read_agent_session_file(
    task_id: String,
    path: String,
    state: State<'_, WorkspaceState>,
) -> Result<SessionFileDiff, String> {
    let root = current_root(&state)?;
    tauri::async_runtime::spawn_blocking(move || {
        safe_join(&root, &path)?;
        let session = published_session(&root, &task_id)?;
        if !session.files.iter().any(|entry| entry == &path) {
            return Err("file is not listed in this agent proposal".to_owned());
        }
        let target = default_target(&session);
        let base = merge_base(&root, &target, &session.branch)?;
        let original_spec = format!("{base}:{path}");
        let modified_spec = format!("{}:{path}", session.branch);
        let original_exists = blob_exists(&root, &original_spec)?;
        let modified_exists = blob_exists(&root, &modified_spec)?;
        if !original_exists && !modified_exists {
            return Err("the file is absent from both sides of the diff".to_owned());
        }
        let original = if original_exists {
            blob_text(&root, &original_spec)?
        } else {
            String::new()
        };
        let modified = if modified_exists {
            blob_text(&root, &modified_spec)?
        } else {
            String::new()
        };
        let status = match (original_exists, modified_exists) {
            (false, true) => "added",
            (true, false) => "deleted",
            _ => "modified",
        };
        Ok(SessionFileDiff {
            path,
            status: status.to_owned(),
            original,
            modified,
        })
    })
    .await
    .map_err(|_| "session file worker failed".to_owned())?
}

fn bounded_stdout(output: &[u8]) -> Result<String, String> {
    if output.len() > MAX_CLI_STDOUT {
        return Err("the review command returned too much output".to_owned());
    }
    String::from_utf8(output.to_vec())
        .map_err(|_| "the review command returned non-UTF-8 output".to_owned())
}

fn parse_cli_result(stdout: &str) -> Result<serde_json::Value, String> {
    let line = stdout
        .lines()
        .rev()
        .find(|line| line.trim_start().starts_with('{'))
        .ok_or_else(|| "the review command returned no result".to_owned())?;
    serde_json::from_str(line).map_err(|error| format!("invalid review result: {error}"))
}

fn run_review(root: &Path, args: &[String]) -> Result<serde_json::Value, String> {
    let cli = resolve_cli(root)?;
    let mut command = Command::new(cli);
    command
        .current_dir(root)
        .arg("review")
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .env("GIT_TERMINAL_PROMPT", "0");
    let output = command
        .output()
        .map_err(|error| format!("cannot start the review command: {error}"))?;
    let stdout = bounded_stdout(&output.stdout)?;
    if let Ok(result) = parse_cli_result(&stdout) {
        let validation_refusal = result.get("passed") == Some(&serde_json::Value::Bool(false));
        if output.status.success() || validation_refusal {
            return Ok(result);
        }
    }
    let detail = String::from_utf8_lossy(&output.stderr);
    Err(format!(
        "review command failed: {}",
        detail.chars().take(MAX_CLI_STDERR).collect::<String>()
    ))
}

async fn confirm(
    app: &AppHandle,
    title: &str,
    description: String,
    action: &str,
) -> Result<(), String> {
    let app = app.clone();
    let title = title.to_owned();
    let action = action.to_owned();
    let approved = tauri::async_runtime::spawn_blocking(move || {
        app.dialog()
            .message(description)
            .title(title)
            .buttons(MessageDialogButtons::OkCancelCustom(
                action,
                "Cancel".to_owned(),
            ))
            .blocking_show()
    })
    .await
    .map_err(|_| "could not display native worktree permission".to_owned())?;
    if approved {
        Ok(())
    } else {
        Err("worktree action cancelled".to_owned())
    }
}

#[tauri::command]
pub async fn release_agent_session(
    task_id: String,
    app: AppHandle,
    state: State<'_, WorkspaceState>,
) -> Result<serde_json::Value, String> {
    let root = current_root(&state)?;
    let session = published_session(&root, &task_id)?;
    confirm(
        &app,
        "Ryoko — detach agent worktree",
        format!(
            "Detach {} from disk?\n\nIts branch and published commit are kept, so the change can still be reviewed or merged later. Your checkout is not modified.",
            session.branch
        ),
        "Detach worktree",
    )
    .await?;
    if current_root(&state)? != root {
        return Err("workspace changed while awaiting permission".to_owned());
    }
    tauri::async_runtime::spawn_blocking(move || {
        run_review(
            &root,
            &[
                "--repo".into(),
                ".".into(),
                "--task".into(),
                task_id,
                "--release".into(),
                "--json".into(),
            ],
        )
    })
    .await
    .map_err(|_| "worktree release worker failed".to_owned())?
}

#[tauri::command]
pub async fn discard_agent_session(
    task_id: String,
    app: AppHandle,
    state: State<'_, WorkspaceState>,
) -> Result<serde_json::Value, String> {
    let root = current_root(&state)?;
    let session = published_session(&root, &task_id)?;
    confirm(
        &app,
        "Ryoko — discard agent worktree",
        format!(
            "Discard {}?\n\nThis deletes the isolated worktree AND its branch. The change cannot be recovered from Ryoko. Your checkout is not modified.",
            session.branch
        ),
        "Discard branch",
    )
    .await?;
    if current_root(&state)? != root {
        return Err("workspace changed while awaiting permission".to_owned());
    }
    tauri::async_runtime::spawn_blocking(move || {
        run_review(
            &root,
            &[
                "--repo".into(),
                ".".into(),
                "--task".into(),
                task_id,
                "--discard".into(),
                "--json".into(),
            ],
        )
    })
    .await
    .map_err(|_| "worktree discard worker failed".to_owned())?
}

#[tauri::command]
pub async fn merge_agent_session(
    task_id: String,
    target_branch: Option<String>,
    sandbox_image: String,
    check_command: Option<String>,
    timeout_secs: Option<u64>,
    app: AppHandle,
    state: State<'_, WorkspaceState>,
) -> Result<serde_json::Value, String> {
    if !valid_sandbox_image(&sandbox_image) {
        return Err("a single OCI sandbox image is required for merge validation".to_owned());
    }
    if let Some(command) = check_command.as_deref()
        && !valid_check_command(command)
    {
        return Err("the check command must be a single bounded command line".to_owned());
    }
    let root = current_root(&state)?;
    let session = published_session(&root, &task_id)?;
    let target = target_branch
        .map(|branch| branch.trim().to_owned())
        .filter(|branch| !branch.is_empty() && branch != "HEAD")
        .unwrap_or_else(|| default_target(&session));
    if !valid_target_branch(&target) {
        return Err("target branch is not a valid Git branch name".to_owned());
    }
    confirm(
        &app,
        "Ryoko — merge validated agent branch",
        format!(
            "Merge {} into {target}?\n\nRyoko runs the sandboxed pre-flight check first. It merges only if that check passes, then removes the isolated worktree. This updates your checkout and cannot be undone from Ryoko.",
            session.branch
        ),
        "Validate and merge",
    )
    .await?;
    if current_root(&state)? != root {
        return Err("workspace changed while awaiting permission".to_owned());
    }
    let timeout = timeout_secs.unwrap_or(300).clamp(1, 1_800);
    tauri::async_runtime::spawn_blocking(move || {
        let mut args = vec![
            "--repo".to_owned(),
            ".".to_owned(),
            "--task".to_owned(),
            task_id,
            "--target".to_owned(),
            target,
            "--accept".to_owned(),
            "--json".to_owned(),
            "--sandbox-image".to_owned(),
            sandbox_image.trim().to_owned(),
            "--timeout-secs".to_owned(),
            timeout.to_string(),
        ];
        if let Some(command) = check_command {
            args.push("--check-command".to_owned());
            args.push(command);
        }
        run_review(&root, &args)
    })
    .await
    .map_err(|_| "worktree merge worker failed".to_owned())?
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_uuid_shaped_task_ids_are_accepted() {
        assert!(valid_task_id("3f2504e0-4f89-11d3-9a0c-0305e82c3301"));
        assert!(!valid_task_id("task-1"));
        assert!(!valid_task_id("../escape"));
        assert!(!valid_task_id("3f2504e04f8911d39a0c0305e82c3301"));
    }

    #[test]
    fn target_branches_cannot_inject_options() {
        assert!(valid_target_branch("main"));
        assert!(valid_target_branch("release/2026.01"));
        for invalid in ["", "-main", "main..next", "main branch", "main;rm"] {
            assert!(!valid_target_branch(invalid), "{invalid}");
        }
        assert!(valid_target_branch("HEAD"));
    }

    #[test]
    fn sandbox_images_and_check_commands_are_bounded() {
        assert!(valid_sandbox_image("ghcr.io/acme/ryoko:1"));
        assert!(!valid_sandbox_image("--privileged"));
        assert!(!valid_sandbox_image("two words"));
        assert!(valid_check_command("cargo test --workspace"));
        assert!(!valid_check_command(""));
        assert!(!valid_check_command("cargo\ntest"));
    }

    #[test]
    fn numstat_parsing_is_bounded_and_marks_binary_files() {
        let parsed = parse_numstat("3\t1\tsrc/lib.rs\n-\t-\tlogo.png\n").unwrap();
        assert_eq!(parsed[0].path, "src/lib.rs");
        assert_eq!(parsed[0].additions, 3);
        assert_eq!(parsed[0].deletions, 1);
        assert_eq!(parsed[1].status, "binary");
    }
}
