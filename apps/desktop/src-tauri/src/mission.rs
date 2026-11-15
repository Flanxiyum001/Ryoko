//! Read-only access to orchestrator event logs for the Mission Control panel.
//!
//! `ryoko run --event-log .ryoko/events/run.jsonl` writes bounded JSONL events
//! inside the workspace. The shell only lists and reads files in that directory;
//! it never accepts an arbitrary path from the frontend.

use std::fs;
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

fn event_dir(root: &Path) -> PathBuf {
    root.join(EVENT_DIR)
}

fn valid_log_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 128
        && name.ends_with(".jsonl")
        && !name.contains('/')
        && !name.contains('\\')
        && !name.contains("..")
        && !name.contains('\0')
}

fn log_path(root: &Path, name: &str) -> Result<PathBuf, String> {
    if !valid_log_name(name) {
        return Err("invalid Mission Control log name".to_owned());
    }
    Ok(event_dir(root).join(name))
}

#[tauri::command]
pub fn list_mission_logs(state: State<'_, WorkspaceState>) -> Result<Vec<MissionLog>, String> {
    let root = current_root(&state)?;
    let dir = event_dir(&root);
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
    logs.sort_by(|left, right| right.modified_ms.cmp(&left.modified_ms));
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
    fs::read_to_string(&path)
        .map_err(|error| format!("cannot read Mission Control log as UTF-8: {error}"))
}

#[cfg(test)]
mod tests {
    use super::valid_log_name;

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
}
