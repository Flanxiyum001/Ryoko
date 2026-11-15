//! Native, operator-approved selective staging of local Git text hunks.
//! Git runs with an explicit argv, never a shell. The worktree is not edited.
use std::io::{Read, Write};
use std::path::Path;
use std::process::{Command, Stdio};

use serde::Serialize;
use tauri::{AppHandle, State};
use tauri_plugin_dialog::{DialogExt, MessageDialogButtons};

use crate::workspace::{WorkspaceState, current_root};

#[path = "../../../../crates/ryoko-worktree/src/hunks.rs"]
mod hunks;

const MAX_DIFF: usize = 1024 * 1024;

#[derive(Serialize)]
pub struct ReviewHunkView {
    id: usize,
    path: String,
    header: String,
    preview: String,
}

#[derive(Serialize)]
pub struct ReviewSnapshot {
    patch: String,
    hunks: Vec<ReviewHunkView>,
}

fn git_diff(root: &Path) -> Result<String, String> {
    let top = Command::new("git")
        .current_dir(root)
        .args(["rev-parse", "--show-toplevel"])
        .stdin(Stdio::null())
        .output()
        .map_err(|error| format!("cannot locate Git repository: {error}"))?;
    if !top.status.success() {
        return Err("open a Git repository to stage hunks".into());
    }
    let top_path = String::from_utf8(top.stdout)
        .map_err(|_| "invalid Git repository path".to_owned())?;
    let canonical = Path::new(top_path.trim())
        .canonicalize()
        .map_err(|error| format!("cannot resolve Git root: {error}"))?;
    if canonical != root {
        return Err("open the repository root to review Git hunks".into());
    }
    let mut child = Command::new("git")
        .current_dir(root)
        .args([
            "diff",
            "--no-ext-diff",
            "--no-textconv",
            "--no-color",
            "--binary",
            "--ignore-submodules=all",
            "--",
            ".",
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .env("GIT_TERMINAL_PROMPT", "0")
        .spawn()
        .map_err(|error| format!("cannot inspect Git diff: {error}"))?;
    let mut output = Vec::new();
    let read_result = child
        .stdout
        .take()
        .ok_or_else(|| "Git diff stdout unavailable".to_owned())?
        .take((MAX_DIFF + 1) as u64)
        .read_to_end(&mut output);
    if read_result.is_err() || output.len() > MAX_DIFF {
        let _ = child.kill();
        let _ = child.wait();
        return Err("Git diff is unavailable or exceeds the 1 MiB review limit".into());
    }
    if !child
        .wait()
        .map_err(|error| format!("cannot finish Git diff: {error}"))?
        .success()
    {
        return Err("Git diff failed; open a valid repository".into());
    }
    String::from_utf8(output).map_err(|_| "Git diff contains invalid UTF-8".into())
}

fn apply_cached(root: &Path, patch: &str) -> Result<(), String> {
    let mut child = Command::new("git")
        .current_dir(root)
        .args(["apply", "--cached", "--whitespace=nowarn", "-"])
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .env("GIT_TERMINAL_PROMPT", "0")
        .spawn()
        .map_err(|error| format!("cannot start Git staging: {error}"))?;
    let mut input = child
        .stdin
        .take()
        .ok_or_else(|| "Git apply stdin unavailable".to_owned())?;
    if input.write_all(patch.as_bytes()).is_err() {
        let _ = child.kill();
        let _ = child.wait();
        return Err("could not pass selected hunks to Git".into());
    }
    drop(input);
    if !child
        .wait()
        .map_err(|error| format!("cannot finish Git staging: {error}"))?
        .success()
    {
        return Err("Git refused selected hunks; index or files may have changed".into());
    }
    Ok(())
}

#[tauri::command]
pub async fn list_review_hunks(state: State<'_, WorkspaceState>) -> Result<ReviewSnapshot, String> {
    let root = current_root(&state)?;
    let patch = tauri::async_runtime::spawn_blocking(move || git_diff(&root))
        .await
        .map_err(|_| "Git diff worker failed".to_owned())??;
    let parsed = hunks::HunkPatch::parse(&patch)?;
    let hunks = parsed
        .hunks()
        .iter()
        .map(|hunk| ReviewHunkView {
            id: hunk.id,
            path: hunk.path.clone(),
            header: hunk.header.clone(),
            preview: hunk.preview.clone(),
        })
        .collect();
    Ok(ReviewSnapshot { patch, hunks })
}

#[tauri::command]
pub async fn stage_review_hunks(
    ids: Vec<usize>,
    expected_patch: String,
    state: State<'_, WorkspaceState>,
    app: AppHandle,
) -> Result<(), String> {
    if expected_patch.len() > MAX_DIFF {
        return Err("hunk review exceeds 1 MiB".into());
    }
    let root = current_root(&state)?;
    let source = root.clone();
    let current = tauri::async_runtime::spawn_blocking(move || git_diff(&source))
        .await
        .map_err(|_| "Git diff worker failed".to_owned())??;
    if current != expected_patch {
        return Err("files changed since hunk preview; refresh before staging".into());
    }
    let parsed = hunks::HunkPatch::parse(&current)?;
    let patch = parsed.select(&ids)?;
    let selected = parsed
        .hunks()
        .iter()
        .filter(|hunk| ids.contains(&hunk.id))
        .take(12)
        .map(|hunk| format!("#{} {} {}", hunk.id, hunk.path, hunk.header))
        .collect::<Vec<_>>()
        .join("\n");
    let description = format!(
        "Stage {} selected Git text hunk(s) in the current workspace index?\n\n{}\n\nThis does not edit files, commit, merge or push.",
        ids.len(),
        selected
    );
    let approved = tauri::async_runtime::spawn_blocking(move || {
        app.dialog()
            .message(description)
            .title("Ryoko — approve Git hunk staging")
            .buttons(MessageDialogButtons::OkCancelCustom(
                "Stage once".to_owned(),
                "Cancel".to_owned(),
            ))
            .blocking_show()
    })
    .await
    .map_err(|_| "could not display native Git staging permission".to_owned())?;
    if !approved {
        return Err("Git hunk staging cancelled".into());
    }
    if current_root(&state)? != root {
        return Err("workspace changed while awaiting permission".into());
    }
    let target = root.clone();
    tauri::async_runtime::spawn_blocking(move || {
        // Fail closed if a tool, extension or another process changed the
        // unstaged diff while native approval was displayed.
        if git_diff(&target)? != current {
            return Err("Git diff changed during approval; refresh and retry".into());
        }
        apply_cached(&target, &patch)
    })
    .await
    .map_err(|_| "Git staging worker failed".to_owned())?
}
