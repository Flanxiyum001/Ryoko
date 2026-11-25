use std::fs;
use std::io::Write;
use std::path::PathBuf;
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

use ignore::WalkBuilder;
use serde::Serialize;
use tauri::{AppHandle, State};
use tauri_plugin_dialog::{DialogExt, MessageDialogButtons};

use crate::paths::{create_directories_without_symlinks, safe_join};

const MAX_FILE_BYTES: u64 = 2 * 1024 * 1024;
const MAX_FILES: usize = 5_000;

#[derive(Default)]
pub struct WorkspaceState {
    root: Mutex<Option<PathBuf>>,
}

#[derive(Serialize)]
pub struct WorkspaceInfo {
    pub root: String,
    pub name: String,
}

pub(crate) fn current_root(state: &State<'_, WorkspaceState>) -> Result<PathBuf, String> {
    state
        .root
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .clone()
        .ok_or_else(|| "no workspace is open".to_owned())
}

/// Shows a native folder picker that stays attached to the Ryoko window.
///
/// The dialog plugin only parents its picker on Windows and macOS. On Linux an
/// unparented GTK dialog can open behind the main window or on another
/// workspace, which looks like the button does nothing. Here Linux gets a
/// modal GTK chooser that is transient for the main window instead.
#[tauri::command]
pub async fn pick_workspace_folder(
    window: tauri::WebviewWindow,
    title: Option<String>,
) -> Result<Option<String>, String> {
    let title = title.unwrap_or_else(|| "Open Folder".to_owned());
    #[cfg(target_os = "linux")]
    {
        let (sender, receiver) = std::sync::mpsc::channel();
        let target = window.clone();
        window
            .run_on_main_thread(move || {
                let _ = sender.send(linux_pick_folder(&target, &title));
            })
            .map_err(|error| format!("cannot show the folder picker: {error}"))?;
        tauri::async_runtime::spawn_blocking(move || receiver.recv())
            .await
            .map_err(|error| format!("folder picker failed: {error}"))?
            .map_err(|_| "folder picker closed unexpectedly".to_owned())?
    }
    #[cfg(not(target_os = "linux"))]
    {
        let picked = tauri::async_runtime::spawn_blocking(move || {
            window
                .dialog()
                .file()
                .set_parent(&window)
                .set_title(title)
                .blocking_pick_folder()
        })
        .await
        .map_err(|error| format!("folder picker failed: {error}"))?;
        picked
            .map(|path| {
                path.into_path()
                    .map(|path| path.display().to_string())
                    .map_err(|error| error.to_string())
            })
            .transpose()
    }
}

#[cfg(target_os = "linux")]
fn linux_pick_folder(window: &tauri::WebviewWindow, title: &str) -> Result<Option<String>, String> {
    use gtk::prelude::*;

    let parent = window
        .gtk_window()
        .map_err(|error| format!("cannot find the main window: {error}"))?;
    let dialog = gtk::FileChooserDialog::with_buttons(
        Some(title),
        Some(&parent),
        gtk::FileChooserAction::SelectFolder,
        &[
            ("_Cancel", gtk::ResponseType::Cancel),
            ("_Open", gtk::ResponseType::Accept),
        ],
    );
    dialog.set_modal(true);
    dialog.set_default_response(gtk::ResponseType::Accept);
    let response = dialog.run();
    let path = (response == gtk::ResponseType::Accept)
        .then(|| dialog.filename())
        .flatten()
        .map(|path| path.display().to_string());
    // SAFETY: the dialog is owned here, was created on this (main) thread and
    // is not used after being destroyed.
    unsafe { dialog.destroy() };
    Ok(path)
}

#[tauri::command]
pub fn open_workspace(
    path: String,
    state: State<'_, WorkspaceState>,
) -> Result<WorkspaceInfo, String> {
    let root = PathBuf::from(&path)
        .canonicalize()
        .map_err(|error| format!("cannot open workspace: {error}"))?;
    if !root.is_dir() {
        return Err("workspace path is not a directory".to_owned());
    }
    let name = root
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| root.display().to_string());
    let info = WorkspaceInfo {
        root: root.display().to_string(),
        name,
    };
    *state.root.lock().unwrap_or_else(|error| error.into_inner()) = Some(root);
    Ok(info)
}

#[tauri::command]
pub fn list_files(state: State<'_, WorkspaceState>) -> Result<Vec<String>, String> {
    let root = current_root(&state)?;
    let mut builder = WalkBuilder::new(&root);
    builder
        .hidden(false)
        .git_ignore(true)
        .git_global(false)
        .git_exclude(false)
        .parents(false)
        .follow_links(false)
        .filter_entry(|entry| {
            entry.depth() == 0
                || !matches!(
                    entry.file_name().to_str(),
                    Some(".git" | ".ryoko" | "target" | "node_modules")
                )
        });

    let mut files = Vec::new();
    for entry in builder.build().filter_map(Result::ok) {
        if files.len() >= MAX_FILES {
            break;
        }
        if !entry.file_type().is_some_and(|kind| kind.is_file()) {
            continue;
        }
        if let Ok(relative) = entry.path().strip_prefix(&root) {
            files.push(relative.to_string_lossy().replace('\\', "/"));
        }
    }
    files.sort();
    Ok(files)
}

#[tauri::command]
pub fn read_file(path: String, state: State<'_, WorkspaceState>) -> Result<String, String> {
    let root = current_root(&state)?;
    let candidate = safe_join(&root, &path)?;
    let real = candidate
        .canonicalize()
        .map_err(|error| format!("cannot read {path}: {error}"))?;
    if !real.starts_with(&root) {
        return Err(format!("path escapes the workspace: {path}"));
    }
    if !real.is_file() {
        return Err(format!("not a regular file: {path}"));
    }
    let metadata = fs::metadata(&real).map_err(|error| format!("cannot read {path}: {error}"))?;
    if metadata.len() > MAX_FILE_BYTES {
        return Err(format!("file is larger than 2 MiB: {path}"));
    }
    let content = fs::read_to_string(&real)
        .map_err(|error| format!("cannot read {path} as UTF-8: {error}"))?;
    validate_editor_text(&content, &path)?;
    Ok(content)
}

fn validate_editor_text(content: &str, path: &str) -> Result<(), String> {
    if content.contains('\0') {
        return Err(format!("binary or unsupported text encoding: {path}"));
    }
    Ok(())
}

fn requires_sensitive_write_approval(path: &str) -> bool {
    let normalized = path.replace('\\', "/").to_ascii_lowercase();
    let segments = normalized.split('/').collect::<Vec<_>>();
    segments.iter().any(|segment| {
        *segment == ".env"
            || segment.starts_with(".env.")
            || matches!(*segment, ".npmrc" | ".pypirc" | ".ssh" | "credentials")
    }) || segments
        .windows(2)
        .any(|parts| parts == [".github", "workflows"])
}

/// Sensitive files require an OS-owned confirmation. A renderer/agent cannot
/// mint a permission token or bypass this dialog by calling the IPC command.
#[tauri::command]
pub async fn write_file(
    path: String,
    content: String,
    expected_content: String,
    state: State<'_, WorkspaceState>,
    app: AppHandle,
) -> Result<(), String> {
    let root = current_root(&state)?;
    if content.len() as u64 > MAX_FILE_BYTES {
        return Err(format!("refusing to write more than 2 MiB: {path}"));
    }
    if expected_content.len() as u64 > MAX_FILE_BYTES {
        return Err("expected file version exceeds 2 MiB".to_owned());
    }
    let candidate = safe_join(&root, &path)?;
    if requires_sensitive_write_approval(&path) {
        let description = format!(
            "Allow writing sensitive project file '{path}' in '{}'? Review its contents first.\
             \n\nDeny unless you explicitly intended this change.",
            root.display()
        );
        // Blocking native dialogs must never run on Tauri's main event thread.
        let approved = tauri::async_runtime::spawn_blocking(move || {
            app.dialog()
                .message(description)
                .title("Ryoko — sensitive file permission")
                .buttons(MessageDialogButtons::OkCancelCustom(
                    "Approve once".to_owned(),
                    "Deny".to_owned(),
                ))
                .blocking_show()
        })
        .await
        .map_err(|_| "could not display permission dialog".to_owned())?;
        if !approved {
            return Err(format!("sensitive file write denied by user: {path}"));
        }
        if current_root(&state)? != root {
            return Err("workspace changed while awaiting permission; write cancelled".to_owned());
        }
    }
    let parent = candidate
        .parent()
        .ok_or_else(|| format!("invalid file path: {path}"))?;
    create_directories_without_symlinks(&root, parent)?;
    let real_parent = parent
        .canonicalize()
        .map_err(|error| format!("cannot resolve {}: {error}", parent.display()))?;
    if !real_parent.starts_with(&root) {
        return Err(format!("path escapes the workspace: {path}"));
    }
    let file_name = candidate
        .file_name()
        .ok_or_else(|| format!("invalid file path: {path}"))?;
    let target = real_parent.join(file_name);
    if let Ok(metadata) = fs::symlink_metadata(&target) {
        if metadata.file_type().is_symlink() {
            return Err(format!("refusing to write through a symlink: {path}"));
        }
        if !metadata.is_file() {
            return Err(format!("target is not a regular file: {path}"));
        }
    }

    // Never silently replace edits made outside Ryoko since the file was
    // opened. This is a best-effort optimistic-concurrency guard; robust
    // multi-process atomic compare-and-swap needs OS-specific primitives.
    let current = fs::read_to_string(&target)
        .map_err(|error| format!("cannot verify current file {path}: {error}"))?;
    if current != expected_content {
        return Err(format!(
            "file changed on disk since it was opened; reload or compare before saving: {path}"
        ));
    }

    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or_default();
    let temp = real_parent.join(format!(".ryoko-save-{}-{stamp}.tmp", std::process::id()));
    let write_result = (|| -> std::io::Result<()> {
        // Do not follow a pre-existing symlink or clobber a sibling temp file
        // if timestamps collide across saves or a malicious process races us.
        let mut options = fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options.open(&temp)?;
        file.write_all(content.as_bytes())?;
        file.sync_all()
    })();
    if let Err(error) = write_result {
        let _ = fs::remove_file(&temp);
        return Err(format!("cannot save {path}: {error}"));
    }
    // Avoid the destructive delete-before-rename fallback on Windows: a failed
    // rename must never remove the existing document. Windows native atomic
    // replacement will need a dedicated implementation for full parity.
    fs::rename(&temp, &target).map_err(|error| {
        let _ = fs::remove_file(&temp);
        format!("cannot save {path}: {error}")
    })
}

#[cfg(test)]
mod permission_tests {
    use super::{requires_sensitive_write_approval, validate_editor_text};

    #[test]
    fn rejects_binary_content_without_rejecting_unknown_text_files() {
        assert!(validate_editor_text("example\n", "file.unknown").is_ok());
        assert!(validate_editor_text("\0binary", "image.bin").is_err());
    }

    #[test]
    fn prompts_for_environment_and_ci_files() {
        for path in [
            ".env",
            ".env.local",
            "apps/api/.env.production",
            ".github/workflows/build.yml",
            r".github\workflows\desktop.yml",
            ".NPMRC",
            ".ssh/id_ed25519",
        ] {
            assert!(requires_sensitive_write_approval(path), "{path}");
        }
    }

    #[test]
    fn ordinary_source_edits_need_no_extra_prompt() {
        for path in ["src/main.rs", "apps/site/index.ts", ".github/README.md"] {
            assert!(!requires_sensitive_write_approval(path), "{path}");
        }
    }
}
