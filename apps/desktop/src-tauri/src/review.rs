//! Native, operator-approved selective staging of local Git text hunks.
//! Git runs with an explicit argv, never a shell. The worktree is not edited.
use std::fs;
use std::io::{Read, Write};
use std::path::Path;
use std::process::{Command, Stdio};

use serde::Serialize;
use tauri::{AppHandle, State};
use tauri_plugin_dialog::{DialogExt, MessageDialogButtons};

use crate::paths::safe_join;
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
    old_start: u32,
    new_start: u32,
    new_lines: u32,
}

#[derive(Serialize)]
pub struct ReviewFileContent {
    path: String,
    original: String,
    modified: String,
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
    let top_path =
        String::from_utf8(top.stdout).map_err(|_| "invalid Git repository path".to_owned())?;
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

fn apply_patch(root: &Path, patch: &str, cached: bool, reverse: bool) -> Result<(), String> {
    let mut args = vec!["apply"];
    if cached {
        args.push("--cached");
    }
    if reverse {
        args.push("-R");
    }
    args.extend(["--whitespace=nowarn", "-"]);
    let mut child = Command::new("git")
        .current_dir(root)
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .env("GIT_TERMINAL_PROMPT", "0")
        .spawn()
        .map_err(|error| format!("cannot start Git patch: {error}"))?;
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
        .map_err(|error| format!("cannot finish Git patch: {error}"))?
        .success()
    {
        return Err("Git refused selected hunks; files may have changed".into());
    }
    Ok(())
}

/// Extract `@@ -old[,n] +new[,n] @@` ranges without trusting the rest of the
/// header. Monaco uses `new_start` to place inline hunk controls.
fn hunk_ranges(header: &str) -> (u32, u32, u32) {
    let mut old_start = 1;
    let mut new_start = 1;
    let mut new_lines = 1;
    for token in header.split_whitespace() {
        if let Some(rest) = token.strip_prefix('-') {
            if let Some(value) = rest.split(',').next() {
                old_start = value.parse().unwrap_or(1);
            }
        } else if let Some(rest) = token.strip_prefix('+') {
            let mut parts = rest.split(',');
            if let Some(value) = parts.next() {
                new_start = value.parse().unwrap_or(1);
            }
            if let Some(value) = parts.next() {
                new_lines = value.parse().unwrap_or(1);
            }
        }
    }
    (old_start, new_start, new_lines.max(1))
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
            old_start: hunk_ranges(&hunk.header).0,
            new_start: hunk_ranges(&hunk.header).1,
            new_lines: hunk_ranges(&hunk.header).2,
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
        apply_patch(&target, &patch, true, false)
    })
    .await
    .map_err(|_| "Git staging worker failed".to_owned())?
}

#[tauri::command]
pub async fn reject_review_hunks(
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
        return Err("files changed since hunk preview; refresh before rejecting".into());
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
        "Revert {} selected unstaged hunk(s) from the working files?\n\n{}\n\nThis deletes those edits from the workspace. It does not commit, merge or push.",
        ids.len(),
        selected
    );
    let approved = tauri::async_runtime::spawn_blocking(move || {
        app.dialog()
            .message(description)
            .title("Ryoko — reject Git hunks")
            .buttons(MessageDialogButtons::OkCancelCustom(
                "Reject hunks".to_owned(),
                "Cancel".to_owned(),
            ))
            .blocking_show()
    })
    .await
    .map_err(|_| "could not display native hunk rejection permission".to_owned())?;
    if !approved {
        return Err("Git hunk rejection cancelled".into());
    }
    if current_root(&state)? != root {
        return Err("workspace changed while awaiting permission".into());
    }
    let target = root.clone();
    tauri::async_runtime::spawn_blocking(move || {
        if git_diff(&target)? != current {
            return Err("Git diff changed during approval; refresh and retry".into());
        }
        apply_patch(&target, &patch, false, true)
    })
    .await
    .map_err(|_| "Git rejection worker failed".to_owned())?
}

#[tauri::command]
pub async fn read_review_file(
    path: String,
    state: State<'_, WorkspaceState>,
) -> Result<ReviewFileContent, String> {
    let root = current_root(&state)?;
    let candidate = safe_join(&root, &path)?;
    let source = root.clone();
    let workspace = root.clone();
    let modified_path = path.clone();
    let git_path = path.clone();
    let modified = tauri::async_runtime::spawn_blocking(move || -> Result<String, String> {
        let metadata = fs::symlink_metadata(&candidate)
            .map_err(|error| format!("cannot read {modified_path}: {error}"))?;
        if metadata.file_type().is_symlink()
            || !metadata.is_file()
            || metadata.len() > MAX_DIFF as u64
        {
            return Err("review file is not a regular UTF-8 file under 1 MiB".to_owned());
        }
        let real = candidate
            .canonicalize()
            .map_err(|error| format!("cannot resolve {modified_path}: {error}"))?;
        if !real.starts_with(&workspace) {
            return Err("review file escapes the workspace".to_owned());
        }
        fs::read_to_string(&real).map_err(|error| format!("cannot read {modified_path}: {error}"))
    })
    .await
    .map_err(|_| "file read worker failed".to_owned())??;
    let original = tauri::async_runtime::spawn_blocking(move || {
        let output = Command::new("git")
            .current_dir(&source)
            .args(["show", &format!("HEAD:{git_path}")])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .env("GIT_TERMINAL_PROMPT", "0")
            .output()
            .map_err(|error| format!("cannot read committed version: {error}"))?;
        if !output.status.success() || output.stdout.len() > MAX_DIFF {
            return Ok(String::new());
        }
        Ok::<String, String>(String::from_utf8_lossy(&output.stdout).into_owned())
    })
    .await
    .map_err(|_| "Git read worker failed".to_owned())??;
    Ok(ReviewFileContent {
        path,
        original,
        modified,
    })
}

#[cfg(test)]
mod tests {
    use super::hunk_ranges;

    #[test]
    fn hunk_header_ranges_are_bounded_and_parsed() {
        assert_eq!(hunk_ranges("@@ -1,2 +10,4 @@ fn demo()"), (1, 10, 4));
        assert_eq!(hunk_ranges("@@ -5 +7 @@"), (5, 7, 1));
        assert_eq!(hunk_ranges("not a header"), (1, 1, 1));
    }
}
