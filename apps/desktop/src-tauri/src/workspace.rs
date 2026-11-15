use std::fs;
use std::io::Write;
use std::path::PathBuf;
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

use ignore::WalkBuilder;
use serde::Serialize;
use tauri::State;

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
    fs::read_to_string(&real).map_err(|error| format!("cannot read {path} as UTF-8: {error}"))
}

#[tauri::command]
pub fn write_file(
    path: String,
    content: String,
    state: State<'_, WorkspaceState>,
) -> Result<(), String> {
    let root = current_root(&state)?;
    if content.len() as u64 > MAX_FILE_BYTES {
        return Err(format!("refusing to write more than 2 MiB: {path}"));
    }
    let candidate = safe_join(&root, &path)?;
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
    // Windows cannot rename over an existing file; Unix gets an atomic replace.
    if cfg!(windows) && target.exists() {
        if let Err(error) = fs::remove_file(&target) {
            let _ = fs::remove_file(&temp);
            return Err(format!("cannot replace {path}: {error}"));
        }
    }
    fs::rename(&temp, &target).map_err(|error| {
        let _ = fs::remove_file(&temp);
        format!("cannot save {path}: {error}")
    })
}
