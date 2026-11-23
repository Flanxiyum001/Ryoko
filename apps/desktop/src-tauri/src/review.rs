//! Native, operator-approved selective staging of local Git text hunks.
//! Git runs with an explicit argv, never a shell. The worktree is not edited.
//!
//! Every apply re-checks the reviewed snapshot against a freshly captured
//! diff: each selected hunk must still anchor to the same file identity and
//! line ranges, and any drift is reported per file as a stale review state
//! instead of being overwritten. Binary files, renames, copies, deletions,
//! mode-only and submodule changes are surfaced as explicit non-editable
//! review states rather than being silently dropped.
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
#[serde(rename_all = "camelCase")]
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
#[serde(rename_all = "camelCase")]
pub struct ReviewFileView {
    path: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    old_path: Option<String>,
    /// One of `text`, `created`, `deleted`, `renamed`, `copied`, `binary`,
    /// `mode-only`, `submodule` (see `hunks::FileReviewState::key`).
    state: String,
    /// Operator-facing explanation for the state.
    state_label: String,
    /// Whether this file supports editable per-hunk staging.
    editable: bool,
    hunk_ids: Vec<usize>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ReviewSnapshot {
    patch: String,
    hunks: Vec<ReviewHunkView>,
    files: Vec<ReviewFileView>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ReviewFileReport {
    path: String,
    hunk_ids: Vec<usize>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ReviewActionReport {
    action: &'static str,
    files: Vec<ReviewFileReport>,
}

#[derive(Serialize)]
pub struct ReviewFileContent {
    path: String,
    original: String,
    modified: String,
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

fn file_state_label(state: hunks::FileReviewState, old_path: Option<&str>) -> String {
    match state {
        hunks::FileReviewState::Text => "modified — per-hunk staging enabled".to_owned(),
        hunks::FileReviewState::Created => "new file — partial staging enabled".to_owned(),
        hunks::FileReviewState::Deleted => {
            "deleted file — restore or reject the whole file with Git; not editable here".to_owned()
        }
        hunks::FileReviewState::Renamed => match old_path {
            Some(old) => {
                format!("renamed from {old} — reviewed as one whole-file change; not editable here")
            }
            None => "renamed — reviewed as one whole-file change; not editable here".to_owned(),
        },
        hunks::FileReviewState::Copied => match old_path {
            Some(old) => {
                format!("copied from {old} — reviewed as one whole-file change; not editable here")
            }
            None => "copied — reviewed as one whole-file change; not editable here".to_owned(),
        },
        hunks::FileReviewState::Binary => {
            "binary content — inspect with Git tooling; not editable in text review".to_owned()
        }
        hunks::FileReviewState::ModeOnly => {
            "mode change only — no content hunks; apply with Git commands".to_owned()
        }
        hunks::FileReviewState::Submodule => {
            "submodule pointer — manage the submodule with Git; not editable here".to_owned()
        }
    }
}

fn review_file_views(parsed: &hunks::HunkPatch) -> Vec<ReviewFileView> {
    parsed
        .file_reviews()
        .into_iter()
        .map(|review| {
            let editable = review.state.supports_hunk_staging();
            let old_path = review.old_path;
            let state_label = file_state_label(review.state, old_path.as_deref());
            ReviewFileView {
                path: review.path,
                old_path,
                state: review.state.key().to_owned(),
                state_label,
                editable,
                hunk_ids: if editable {
                    review.hunk_ids
                } else {
                    Vec::new()
                },
            }
        })
        .collect()
}

#[tauri::command]
pub async fn list_review_hunks(state: State<'_, WorkspaceState>) -> Result<ReviewSnapshot, String> {
    let root = current_root(&state)?;
    let patch = tauri::async_runtime::spawn_blocking(move || git_diff(&root))
        .await
        .map_err(|_| "Git diff worker failed".to_owned())??;
    let parsed = hunks::HunkPatch::parse(&patch)?;
    let files = review_file_views(&parsed);
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
    Ok(ReviewSnapshot {
        patch,
        hunks,
        files,
    })
}

/// Reparses the snapshot and the fresh diff, then verifies every selected
/// hunk against the file identity and line ranges it was reviewed against.
/// Returns the patch to apply plus the per-file post-apply plan.
fn verified_selection(
    expected_patch: &str,
    current_patch: &str,
    ids: &[usize],
) -> Result<(String, Vec<ReviewFileReport>), String> {
    let reviewed = hunks::HunkPatch::parse(expected_patch)?;
    let current = hunks::HunkPatch::parse(current_patch)?;
    let audit = reviewed.verify_selection(ids, &current)?;
    if !audit.stale.is_empty() {
        let details = audit
            .stale
            .iter()
            .map(|stale| format!("#{} in {}: {}", stale.id, stale.path, stale.reason))
            .collect::<Vec<_>>()
            .join("\n");
        return Err(format!(
            "review base is stale for the selected hunks; refresh the diff and reselect. No files were changed.\n{details}"
        ));
    }
    let patch = current.select(&audit.fresh)?;
    let mut by_path: Vec<ReviewFileReport> = Vec::new();
    for id in &audit.fresh {
        let path = reviewed
            .hunks()
            .iter()
            .find(|hunk| hunk.id == *id)
            .map(|hunk| hunk.path.clone())
            .unwrap_or_else(|| "unknown".to_owned());
        match by_path.iter_mut().find(|report| report.path == path) {
            Some(report) => report.hunk_ids.push(*id),
            None => by_path.push(ReviewFileReport {
                path,
                hunk_ids: vec![*id],
            }),
        }
    }
    Ok((patch, by_path))
}

async fn run_review_action(
    ids: Vec<usize>,
    expected_patch: String,
    max_hunks_in_report: usize,
    action: &'static str,
    state: &State<'_, WorkspaceState>,
    app: &AppHandle,
) -> Result<ReviewActionReport, String> {
    if expected_patch.len() > MAX_DIFF {
        return Err("hunk review exceeds 1 MiB".into());
    }
    let root = current_root(state)?;
    let source = root.clone();
    let initial = tauri::async_runtime::spawn_blocking(move || git_diff(&source))
        .await
        .map_err(|_| "Git diff worker failed".to_owned())??;
    verified_selection(&expected_patch, &initial, &ids)?;
    let parsed = hunks::HunkPatch::parse(&initial)?;
    let selected = parsed
        .hunks()
        .iter()
        .filter(|hunk| ids.contains(&hunk.id))
        .take(max_hunks_in_report)
        .map(|hunk| format!("#{} {} {}", hunk.id, hunk.path, hunk.header))
        .collect::<Vec<_>>()
        .join("\n");
    let (title, description) = if action == "stage" {
        (
            "Ryoko — approve Git hunk staging",
            format!(
                "Stage {} selected Git text hunk(s) in the current workspace index?\n\n{}\n\nThis does not edit files, commit, merge or push.",
                ids.len(),
                selected
            ),
        )
    } else {
        (
            "Ryoko — reject Git hunks",
            format!(
                "Revert {} selected unstaged hunk(s) from the working files?\n\n{}\n\nThis deletes those edits from the workspace. It does not commit, merge or push.",
                ids.len(),
                selected
            ),
        )
    };
    let approved = tauri::async_runtime::spawn_blocking({
        let title = title.to_owned();
        move || {
            app.dialog()
                .message(description)
                .title(title)
                .buttons(MessageDialogButtons::OkCancelCustom(
                    if action == "stage" {
                        "Stage once".to_owned()
                    } else {
                        "Reject hunks".to_owned()
                    },
                    "Cancel".to_owned(),
                ))
                .blocking_show()
        }
    })
    .await
    .map_err(|_| "could not display native Git staging permission".to_owned())?;
    if !approved {
        return Err("Git hunk action cancelled".into());
    }
    if current_root(state)? != root {
        return Err("workspace changed while awaiting permission".into());
    }
    let target = root.clone();
    let expected = expected_patch.clone();
    let (applied_patch, report) = tauri::async_runtime::spawn_blocking(move || {
        // Fail closed if a tool, extension or another process changed the
        // unstaged diff while native approval was displayed.
        let current = git_diff(&target)?;
        verified_selection(&expected, &current, &ids)
    })
    .await
    .map_err(|_| "Git staging worker failed".to_owned())??;
    let target = root.clone();
    let cached = action == "stage";
    tauri::async_runtime::spawn_blocking(move || {
        apply_patch(&target, &applied_patch, cached, !cached)
    })
    .await
    .map_err(|_| "Git staging worker failed".to_owned())??;
    Ok(ReviewActionReport {
        action,
        files: report,
    })
}

#[tauri::command]
pub async fn stage_review_hunks(
    ids: Vec<usize>,
    expected_patch: String,
    state: State<'_, WorkspaceState>,
    app: AppHandle,
) -> Result<ReviewActionReport, String> {
    run_review_action(ids, expected_patch, 12, "stage", &state, &app).await
}

#[tauri::command]
pub async fn reject_review_hunks(
    ids: Vec<usize>,
    expected_patch: String,
    state: State<'_, WorkspaceState>,
    app: AppHandle,
) -> Result<ReviewActionReport, String> {
    run_review_action(ids, expected_patch, 12, "reject", &state, &app).await
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
    use super::review_file_views;
    use super::verified_selection;

    #[test]
    fn hunk_header_ranges_are_bounded_and_parsed() {
        assert_eq!(hunk_ranges("@@ -1,2 +10,4 @@ fn demo()"), (1, 10, 4));
        assert_eq!(hunk_ranges("@@ -5 +7 @@"), (5, 7, 1));
        assert_eq!(hunk_ranges("not a header"), (1, 1, 1));
    }

    const SNAPSHOT: &str = "diff --git a/src/main.rs b/src/main.rs\nindex 1111111..2222222 100644\n--- a/src/main.rs\n+++ b/src/main.rs\n@@ -1,1 +1,1 @@\n-old\n+new\n";

    #[test]
    fn selecting_hunks_of_an_unchanged_file_survives_unrelated_file_changes() {
        let drifted = format!(
            "{SNAPSHOT}diff --git a/generated.rs b/generated.rs\nindex 6666668..7777778 100644\n--- a/generated.rs\n+++ b/generated.rs\n@@ -1,1 +1,1 @@\n-a\n+b\n"
        );
        let (patch, report) = verified_selection(SNAPSHOT, &drifted, &[1]).unwrap();
        assert!(patch.contains("+new"));
        assert!(!patch.contains("generated.rs"));
        assert_eq!(report.len(), 1);
        assert_eq!(report[0].path, "src/main.rs");
        assert_eq!(report[0].hunk_ids, vec![1]);
    }

    #[test]
    fn a_changed_base_is_reported_as_a_per_file_stale_conflict() {
        let current = SNAPSHOT.replace("index 1111111", "index 9999999");
        let error = verified_selection(SNAPSHOT, &current, &[1]).unwrap_err();
        assert!(error.contains("review base is stale"), "{error}");
        assert!(error.contains("index base changed"), "{error}");
        assert!(error.contains("src/main.rs"));
        assert!(error.contains("No files were changed"));
    }

    #[test]
    fn a_missing_file_difference_is_reported_without_applying_anything() {
        let error = verified_selection(&format!("{SNAPSHOT}diff --git a/x b/x\nindex 1..2 100644\n--- a/x\n+++ b/x\n@@ -1 +1 @@\n-a\n+b\n"), SNAPSHOT, &[2]).unwrap_err();
        assert!(
            error.contains("no longer has unstaged differences"),
            "{error}"
        );
    }

    #[test]
    fn snapshot_lists_renamed_and_binary_files_as_non_editable_states() {
        let patch = "diff --git a/old.rs b/new.rs\nsimilarity index 70%\nrename from old.rs\nrename to new.rs\ndiff --git a/blob.bin b/blob.bin\nindex 1234567..89abcde 100644\nGIT binary patch\nliteral 4\nc2VjcmV0\n";
        let parsed = super::hunks::HunkPatch::parse(patch).unwrap();
        let views = review_file_views(&parsed);
        assert_eq!(views.len(), 2);
        let renamed = views
            .iter()
            .find(|view| view.path == "new.rs")
            .expect("rename entry is listed");
        assert_eq!(renamed.state, "renamed");
        assert!(!renamed.editable);
        assert_eq!(renamed.old_path.as_deref(), Some("old.rs"));
        assert!(renamed.hunk_ids.is_empty());
        assert!(renamed.state_label.contains("whole-file"));

        let binary = views
            .iter()
            .find(|view| view.path == "blob.bin")
            .expect("binary entry is listed");
        assert_eq!(binary.state, "binary");
        assert!(!binary.editable);
        assert!(binary.state_label.contains("not editable"));

        let text = super::hunks::HunkPatch::parse(SNAPSHOT).unwrap();
        let views = review_file_views(&text);
        assert_eq!(views.len(), 1);
        assert!(views[0].editable);
        assert_eq!(views[0].hunk_ids, vec![1]);
    }
}
