//! Read-only access to orchestrator event logs for the Mission Control panel.
//!
//! `ryoko run --event-log .ryoko/events/run.jsonl` writes bounded JSONL events
//! inside the workspace. The shell only lists and reads files in that directory;
//! it never accepts an arbitrary path from the frontend.

use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::Serialize;
use tauri::State;

use crate::workspace::{WorkspaceState, current_root};

const EVENT_DIR: &str = ".ryoko/events";
const MAX_LOG_BYTES: u64 = 4 * 1024 * 1024;

#[derive(Serialize)]
pub struct MissionLog {
    pub name: String,
    pub bytes: u64,
    pub modified_ms: u64,
}

/// Resolve the event folder, disallowing links that escape the workspace.
fn event_dir(root: &Path) -> Result<Option<PathBuf>, String> {
    let path = root.join(EVENT_DIR);
    let canonical = match path.canonicalize() {
        Ok(canonical) => canonical,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(format!("cannot resolve Mission Control directory: {error}")),
    };
    if !canonical.starts_with(root) || !canonical.is_dir() {
        return Err("Mission Control log directory escapes the open workspace".to_owned());
    }
    Ok(Some(canonical))
}

fn valid_log_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 128
        && name.ends_with(".jsonl")
        && !name.contains('/')
        && !name.contains('\\')
        && !name.contains("..")
        && !name.chars().any(char::is_control)
}

fn log_path(root: &Path, name: &str) -> Result<PathBuf, String> {
    if !valid_log_name(name) {
        return Err("invalid Mission Control log name".to_owned());
    }
    let dir = event_dir(root)?
        .ok_or_else(|| "Mission Control log directory does not exist".to_owned())?;
    let candidate = dir.join(name);
    let metadata = candidate
        .symlink_metadata()
        .map_err(|error| format!("cannot inspect Mission Control log: {error}"))?;
    if !metadata.file_type().is_file() {
        return Err("Mission Control log is not a regular file".to_owned());
    }
    let canonical = candidate
        .canonicalize()
        .map_err(|error| format!("cannot resolve Mission Control log: {error}"))?;
    if canonical.parent() != Some(dir.as_path()) {
        return Err("Mission Control log escapes the event directory".to_owned());
    }
    Ok(canonical)
}

#[tauri::command]
pub fn list_mission_logs(state: State<'_, WorkspaceState>) -> Result<Vec<MissionLog>, String> {
    let root = current_root(&state)?;
    let Some(dir) = event_dir(&root)? else {
        return Ok(Vec::new());
    };
    let entries = match fs::read_dir(&dir) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(format!("cannot list Mission Control logs: {error}")),
    };
    let mut logs = Vec::new();
    for entry in entries.filter_map(Result::ok) {
        let name = entry.file_name().to_string_lossy().into_owned();
        if !valid_log_name(&name) {
            continue;
        }
        let Ok(file_type) = entry.file_type() else {
            continue;
        };
        if !file_type.is_file() {
            continue;
        }
        let Ok(metadata) = entry.metadata() else {
            continue;
        };
        if !metadata.is_file() || metadata.len() > MAX_LOG_BYTES {
            continue;
        }
        let modified_ms = metadata
            .modified()
            .ok()
            .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
            .map(|duration| duration.as_millis() as u64)
            .unwrap_or_else(|| {
                SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .map(|duration| duration.as_millis() as u64)
                    .unwrap_or_default()
            });
        logs.push(MissionLog {
            name,
            bytes: metadata.len(),
            modified_ms,
        });
    }
    logs.sort_by_key(|left| std::cmp::Reverse(left.modified_ms));
    Ok(logs)
}

#[tauri::command]
pub fn read_mission_log(name: String, state: State<'_, WorkspaceState>) -> Result<String, String> {
    let root = current_root(&state)?;
    let path = log_path(&root, &name)?;
    let metadata =
        fs::metadata(&path).map_err(|error| format!("cannot read Mission Control log: {error}"))?;
    if !metadata.is_file() {
        return Err("Mission Control log is not a regular file".to_owned());
    }
    if metadata.len() > MAX_LOG_BYTES {
        return Err("Mission Control log is larger than 4 MiB".to_owned());
    }
    // Hard cap reading as well as metadata checking: a growing file cannot
    // force unlimited allocation after the size check.
    let mut buffer = String::new();
    fs::File::open(&path)
        .map_err(|error| format!("cannot open Mission Control log: {error}"))?
        .take(MAX_LOG_BYTES + 1)
        .read_to_string(&mut buffer)
        .map_err(|error| format!("cannot read Mission Control log as UTF-8: {error}"))?;
    if buffer.len() as u64 > MAX_LOG_BYTES {
        return Err("Mission Control log is larger than 4 MiB".to_owned());
    }
    Ok(buffer)
}

#[cfg(test)]
mod tests {
    use super::{event_dir, valid_log_name};

    #[test]
    fn only_plain_jsonl_names_are_accepted() {
        assert!(valid_log_name("run.jsonl"));
        assert!(valid_log_name("2026-11-15_abc.jsonl"));
        assert!(!valid_log_name(""));
        assert!(!valid_log_name("run.txt"));
        assert!(!valid_log_name("../run.jsonl"));
        assert!(!valid_log_name("nested/run.jsonl"));
        assert!(!valid_log_name("nested\\run.jsonl"));
    }

    #[cfg(unix)]
    #[test]
    fn refuses_event_directories_that_escape_via_symlink() {
        use std::os::unix::fs::symlink;
        use std::time::{SystemTime, UNIX_EPOCH};
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let base =
            std::env::temp_dir().join(format!("ryoko-mission-{}-{stamp}", std::process::id()));
        let outside = base.with_extension("outside");
        std::fs::create_dir_all(base.join(".ryoko")).unwrap();
        std::fs::create_dir_all(&outside).unwrap();
        symlink(&outside, base.join(".ryoko/events")).unwrap();
        let root = base.canonicalize().unwrap();
        assert!(event_dir(&root).is_err());
        std::fs::remove_dir_all(base).unwrap();
        std::fs::remove_dir_all(outside).unwrap();
    }
}
