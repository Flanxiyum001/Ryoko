//! Opt-in, bounded, workspace-scoped crash recovery for unsaved editor buffers.
//!
//! Snapshots live in an application-owned directory outside the workspace Git
//! tree, are written atomically with private file permissions, and never
//! overwrite workspace files on disk. Restoring a snapshot always requires
//! explicit user review and consent.

use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use tauri::State;

use crate::paths::safe_join;
use crate::workspace::{WorkspaceState, current_root};

pub const RECOVERY_SCHEMA_VERSION: u32 = 1;
const MAX_SNAPSHOT_BYTES: usize = 2 * 1024 * 1024;
const MAX_SNAPSHOTS_PER_WORKSPACE: usize = 64;
const RETENTION_MS: u64 = 7 * 24 * 60 * 60 * 1_000;

#[derive(Default)]
pub struct RecoveryState {
    active_session: Mutex<Option<String>>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct RecoverySnapshot {
    pub schema_version: u32,
    pub workspace_root: String,
    pub path: String,
    pub file_uri: String,
    pub base_fingerprint: String,
    pub content: String,
    pub revision: u64,
    pub session_id: String,
    pub saved_at_ms: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct RecoverableEntry {
    pub path: String,
    pub file_uri: String,
    pub content: String,
    pub disk_content: String,
    pub disk_exists: bool,
    pub disk_changed: bool,
    pub revision: u64,
    pub session_id: String,
    pub saved_at_ms: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct RecoveryListResult {
    pub entries: Vec<RecoverableEntry>,
    pub corrupted_count: usize,
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis() as u64)
        .unwrap_or_default()
}

/// Deterministic cross-restart content/path fingerprint (FNV-1a 128-bit + length).
fn fingerprint_bytes(bytes: &[u8]) -> String {
    const OFFSET_BASIS: u128 = 0x6c62_272e_07bb_0142_62b8_2175_6295_c58d;
    const PRIME: u128 = 0x0000_0000_0100_0000_0000_0000_0000_013b;
    let mut hash = OFFSET_BASIS;
    for &byte in bytes {
        hash ^= u128::from(byte);
        hash = hash.wrapping_mul(PRIME);
    }
    format!("{hash:032x}-{:x}", bytes.len())
}

fn is_sensitive_path(path: &str) -> bool {
    let normalized = path.replace('\\', "/").to_ascii_lowercase();
    let segments: Vec<&str> = normalized.split('/').collect();
    segments.iter().any(|segment| {
        *segment == ".env"
            || segment.starts_with(".env.")
            || matches!(*segment, ".npmrc" | ".pypirc" | ".ssh" | "credentials")
    }) || segments
        .windows(2)
        .any(|parts| parts == [".github", "workflows"])
}

fn recovery_base_dir() -> Result<PathBuf, String> {
    #[cfg(target_os = "windows")]
    let root = std::env::var_os("APPDATA")
        .map(PathBuf::from)
        .ok_or("APPDATA is not set")?
        .join("Ryoko");
    #[cfg(target_os = "macos")]
    let root = std::env::var_os("HOME")
        .map(PathBuf::from)
        .ok_or("HOME is not set")?
        .join("Library/Application Support/Ryoko");
    #[cfg(not(any(target_os = "windows", target_os = "macos")))]
    let root = if let Some(path) = std::env::var_os("XDG_CONFIG_HOME") {
        PathBuf::from(path).join("ryoko")
    } else {
        std::env::var_os("HOME")
            .map(PathBuf::from)
            .ok_or("HOME is not set")?
            .join(".config/ryoko")
    };
    Ok(root.join("recovery"))
}

fn workspace_bucket(base_dir: &Path, workspace_root: &Path) -> PathBuf {
    let key = fingerprint_bytes(workspace_root.to_string_lossy().as_bytes());
    base_dir.join(key)
}

fn snapshot_file_path(bucket: &Path, relative_path: &str) -> PathBuf {
    let key = fingerprint_bytes(relative_path.as_bytes());
    bucket.join(format!("{key}.json"))
}

fn ensure_private_dir(dir: &Path) -> Result<(), String> {
    if let Ok(meta) = fs::symlink_metadata(dir) {
        if meta.file_type().is_symlink() {
            return Err("refusing to use a symlinked recovery directory".to_owned());
        }
        if !meta.is_dir() {
            return Err("recovery path is not a directory".to_owned());
        }
        return Ok(());
    }
    fs::create_dir_all(dir)
        .map_err(|error| format!("cannot create recovery directory: {error}"))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = fs::set_permissions(dir, fs::Permissions::from_mode(0o700));
    }
    Ok(())
}

fn validate_session_id(session_id: &str) -> Result<&str, String> {
    let trimmed = session_id.trim();
    if trimmed.is_empty()
        || trimmed.len() > 80
        || !trimmed
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
    {
        return Err("invalid recovery session id".to_owned());
    }
    Ok(trimmed)
}

#[allow(clippy::too_many_arguments)]
fn save_snapshot_in(
    base_dir: &Path,
    workspace_root: &Path,
    relative_path: &str,
    file_uri: &str,
    content: &str,
    base_content: &str,
    revision: u64,
    session_id: &str,
    saved_at_ms: u64,
) -> Result<(), String> {
    let session_id = validate_session_id(session_id)?;
    let _ = safe_join(workspace_root, relative_path)?;
    if is_sensitive_path(relative_path) {
        return Err("sensitive files are excluded from crash recovery snapshots".to_owned());
    }
    if content.len() > MAX_SNAPSHOT_BYTES || base_content.len() > MAX_SNAPSHOT_BYTES {
        return Err("buffer exceeds the 2 MiB recovery snapshot limit".to_owned());
    }
    if content.contains('\0') {
        return Err("binary buffers are excluded from crash recovery snapshots".to_owned());
    }
    ensure_private_dir(base_dir)?;
    let bucket = workspace_bucket(base_dir, workspace_root);
    ensure_private_dir(&bucket)?;

    let target = snapshot_file_path(&bucket, relative_path);
    if fs::symlink_metadata(&target).is_ok_and(|meta| meta.file_type().is_symlink()) {
        return Err("refusing to overwrite a symlinked recovery snapshot".to_owned());
    }

    // Enforce bounded snapshot count per workspace.
    if !target.exists() {
        let existing = fs::read_dir(&bucket)
            .map_err(|error| format!("cannot inspect recovery directory: {error}"))?
            .filter_map(Result::ok)
            .filter(|entry| entry.path().extension().is_some_and(|ext| ext == "json"))
            .count();
        if existing >= MAX_SNAPSHOTS_PER_WORKSPACE {
            return Err(format!(
                "recovery snapshot limit reached ({MAX_SNAPSHOTS_PER_WORKSPACE} per workspace)"
            ));
        }
    }

    let snapshot = RecoverySnapshot {
        schema_version: RECOVERY_SCHEMA_VERSION,
        workspace_root: workspace_root.display().to_string(),
        path: relative_path.to_owned(),
        file_uri: file_uri.to_owned(),
        base_fingerprint: fingerprint_bytes(base_content.as_bytes()),
        content: content.to_owned(),
        revision,
        session_id: session_id.to_owned(),
        saved_at_ms,
    };
    let encoded = serde_json::to_vec(&snapshot)
        .map_err(|error| format!("cannot encode recovery snapshot: {error}"))?;

    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or_default();
    let temp = bucket.join(format!(".recovery-{}-{stamp}.tmp", std::process::id()));
    let write_result = (|| -> std::io::Result<()> {
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options.open(&temp)?;
        file.write_all(&encoded)?;
        file.sync_all()
    })();
    if let Err(error) = write_result {
        let _ = fs::remove_file(&temp);
        return Err(format!("cannot write recovery snapshot: {error}"));
    }
    if let Err(error) = fs::rename(&temp, &target) {
        let _ = fs::remove_file(&temp);
        return Err(format!("cannot commit recovery snapshot: {error}"));
    }
    Ok(())
}

fn clear_snapshot_in(
    base_dir: &Path,
    workspace_root: &Path,
    relative_path: &str,
) -> Result<(), String> {
    let _ = safe_join(workspace_root, relative_path)?;
    let bucket = workspace_bucket(base_dir, workspace_root);
    let target = snapshot_file_path(&bucket, relative_path);
    match fs::symlink_metadata(&target) {
        Ok(meta) if meta.file_type().is_symlink() => {
            Err("refusing to remove a symlinked recovery snapshot".to_owned())
        }
        Ok(_) => fs::remove_file(&target)
            .map_err(|error| format!("cannot remove recovery snapshot: {error}")),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(format!("cannot inspect recovery snapshot: {error}")),
    }
}

fn retire_session_in(base_dir: &Path, session_id: &str) -> Result<usize, String> {
    let session_id = validate_session_id(session_id)?;
    let Ok(buckets) = fs::read_dir(base_dir) else {
        return Ok(0);
    };
    let mut removed = 0usize;
    for bucket_entry in buckets.filter_map(Result::ok) {
        let bucket_path = bucket_entry.path();
        if fs::symlink_metadata(&bucket_path)
            .map(|meta| meta.file_type().is_symlink() || !meta.is_dir())
            .unwrap_or(true)
        {
            continue;
        }
        let Ok(files) = fs::read_dir(&bucket_path) else {
            continue;
        };
        for file_entry in files.filter_map(Result::ok) {
            let path = file_entry.path();
            if path.extension().is_none_or(|ext| ext != "json") {
                continue;
            }
            if fs::symlink_metadata(&path)
                .map(|meta| meta.file_type().is_symlink() || !meta.is_file())
                .unwrap_or(true)
            {
                continue;
            }
            let Ok(text) = fs::read_to_string(&path) else {
                continue;
            };
            let Ok(snapshot) = serde_json::from_str::<RecoverySnapshot>(&text) else {
                continue;
            };
            if snapshot.session_id == session_id && fs::remove_file(&path).is_ok() {
                removed += 1;
            }
        }
    }
    Ok(removed)
}

fn list_snapshots_in(
    base_dir: &Path,
    workspace_root: &Path,
    now: u64,
    exclude_session: Option<&str>,
) -> Result<RecoveryListResult, String> {
    let bucket = workspace_bucket(base_dir, workspace_root);
    if !bucket.exists() {
        return Ok(RecoveryListResult {
            entries: Vec::new(),
            corrupted_count: 0,
        });
    }
    if fs::symlink_metadata(&bucket).is_ok_and(|meta| meta.file_type().is_symlink()) {
        return Err("refusing to read a symlinked recovery directory".to_owned());
    }

    let mut entries = Vec::new();
    let mut corrupted_count = 0usize;
    let dir = fs::read_dir(&bucket)
        .map_err(|error| format!("cannot read recovery directory: {error}"))?;
    for item in dir.filter_map(Result::ok) {
        let path = item.path();
        if path.extension().is_none_or(|ext| ext != "json") {
            continue;
        }
        let Ok(meta) = fs::symlink_metadata(&path) else {
            corrupted_count += 1;
            continue;
        };
        if meta.file_type().is_symlink()
            || !meta.is_file()
            || meta.len() as usize > MAX_SNAPSHOT_BYTES + 16 * 1024
        {
            corrupted_count += 1;
            continue;
        }
        let Ok(text) = fs::read_to_string(&path) else {
            corrupted_count += 1;
            continue;
        };
        let Ok(snapshot) = serde_json::from_str::<RecoverySnapshot>(&text) else {
            corrupted_count += 1;
            continue;
        };
        if snapshot.schema_version != RECOVERY_SCHEMA_VERSION
            || snapshot.workspace_root != workspace_root.display().to_string()
            || safe_join(workspace_root, &snapshot.path).is_err()
            || is_sensitive_path(&snapshot.path)
        {
            corrupted_count += 1;
            continue;
        }
        // Expire stale snapshots per retention policy.
        if now.saturating_sub(snapshot.saved_at_ms) > RETENTION_MS {
            let _ = fs::remove_file(&path);
            continue;
        }
        if exclude_session.is_some_and(|active| active == snapshot.session_id) {
            continue;
        }

        let candidate = safe_join(workspace_root, &snapshot.path)?;
        let (disk_exists, disk_content, disk_changed) = match fs::symlink_metadata(&candidate) {
            Ok(disk_meta) if disk_meta.file_type().is_symlink() || !disk_meta.is_file() => {
                (true, String::new(), true)
            }
            Ok(disk_meta) if disk_meta.len() as usize > MAX_SNAPSHOT_BYTES => {
                (true, String::new(), true)
            }
            Ok(_) => match candidate.canonicalize() {
                Ok(real) if real.starts_with(workspace_root) => match fs::read_to_string(&real) {
                    Ok(disk_text) => {
                        let changed =
                            fingerprint_bytes(disk_text.as_bytes()) != snapshot.base_fingerprint;
                        (true, disk_text, changed)
                    }
                    Err(_) => (true, String::new(), true),
                },
                _ => (true, String::new(), true),
            },
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                (false, String::new(), true)
            }
            Err(_) => (true, String::new(), true),
        };

        entries.push(RecoverableEntry {
            path: snapshot.path,
            file_uri: snapshot.file_uri,
            content: snapshot.content,
            disk_content,
            disk_exists,
            disk_changed,
            revision: snapshot.revision,
            session_id: snapshot.session_id,
            saved_at_ms: snapshot.saved_at_ms,
        });
    }
    entries.sort_by(|left, right| left.path.cmp(&right.path));
    Ok(RecoveryListResult {
        entries,
        corrupted_count,
    })
}

pub fn shutdown(state: &RecoveryState) {
    let session = state
        .active_session
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .take();
    if let (Some(session_id), Ok(base)) = (session, recovery_base_dir()) {
        let _ = retire_session_in(&base, &session_id);
    }
}

#[tauri::command]
#[allow(clippy::too_many_arguments)] // flat Tauri IPC signature
pub fn recovery_save(
    path: String,
    file_uri: String,
    content: String,
    base_content: String,
    revision: u64,
    session_id: String,
    state: State<'_, RecoveryState>,
    workspace: State<'_, WorkspaceState>,
) -> Result<(), String> {
    let root = current_root(&workspace)?;
    let base = recovery_base_dir()?;
    save_snapshot_in(
        &base,
        &root,
        &path,
        &file_uri,
        &content,
        &base_content,
        revision,
        &session_id,
        now_ms(),
    )?;
    *state
        .active_session
        .lock()
        .unwrap_or_else(|error| error.into_inner()) = Some(session_id);
    Ok(())
}

#[tauri::command]
pub fn recovery_clear(path: String, workspace: State<'_, WorkspaceState>) -> Result<(), String> {
    let root = current_root(&workspace)?;
    let base = recovery_base_dir()?;
    clear_snapshot_in(&base, &root, &path)
}

#[tauri::command]
pub fn recovery_list(
    current_session_id: Option<String>,
    workspace: State<'_, WorkspaceState>,
) -> Result<RecoveryListResult, String> {
    let root = current_root(&workspace)?;
    let base = recovery_base_dir()?;
    list_snapshots_in(&base, &root, now_ms(), current_session_id.as_deref())
}

#[tauri::command]
pub fn recovery_retire_session(
    session_id: String,
    state: State<'_, RecoveryState>,
) -> Result<usize, String> {
    let base = recovery_base_dir()?;
    let removed = retire_session_in(&base, &session_id)?;
    let mut guard = state
        .active_session
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    if guard.as_deref() == Some(session_id.as_str()) {
        *guard = None;
    }
    Ok(removed)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(label: &str) -> PathBuf {
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!(
            "ryoko-recovery-{label}-{}-{stamp}",
            std::process::id()
        ));
        fs::create_dir_all(&dir).unwrap();
        dir.canonicalize().unwrap()
    }

    #[test]
    fn saves_lists_detects_disk_changes_and_clears_without_touching_disk() {
        let base = temp_dir("base");
        let ws = temp_dir("ws");
        let file = ws.join("src/main.rs");
        fs::create_dir_all(file.parent().unwrap()).unwrap();
        fs::write(&file, "fn main() {}\n").unwrap();

        save_snapshot_in(
            &base,
            &ws,
            "src/main.rs",
            "file:///ws/src/main.rs",
            "fn main() { println!(\"unsaved\"); }\n",
            "fn main() {}\n",
            3,
            "crashed-session",
            1_000_000,
        )
        .unwrap();

        // Disk file is completely untouched.
        assert_eq!(fs::read_to_string(&file).unwrap(), "fn main() {}\n");

        let listed = list_snapshots_in(&base, &ws, 1_005_000, Some("new-session")).unwrap();
        assert_eq!(listed.corrupted_count, 0);
        assert_eq!(listed.entries.len(), 1);
        assert!(!listed.entries[0].disk_changed);
        assert!(listed.entries[0].disk_exists);

        // Simulate external disk modification after crash.
        fs::write(&file, "fn main() { /* edited externally */ }\n").unwrap();
        let listed = list_snapshots_in(&base, &ws, 1_005_000, Some("new-session")).unwrap();
        assert!(listed.entries[0].disk_changed);
        assert_eq!(
            listed.entries[0].disk_content,
            "fn main() { /* edited externally */ }\n"
        );

        clear_snapshot_in(&base, &ws, "src/main.rs").unwrap();
        let listed = list_snapshots_in(&base, &ws, 1_005_000, Some("new-session")).unwrap();
        assert!(listed.entries.is_empty());

        let _ = fs::remove_dir_all(&base);
        let _ = fs::remove_dir_all(&ws);
    }

    #[test]
    fn isolates_multiple_workspaces_and_handles_corrupt_snapshots() {
        let base = temp_dir("multi-base");
        let ws_a = temp_dir("ws-a");
        let ws_b = temp_dir("ws-b");
        fs::write(ws_a.join("app.ts"), "a").unwrap();
        fs::write(ws_b.join("app.ts"), "b").unwrap();

        save_snapshot_in(
            &base,
            &ws_a,
            "app.ts",
            "file:///a/app.ts",
            "a-edit",
            "a",
            1,
            "s1",
            10_000,
        )
        .unwrap();
        save_snapshot_in(
            &base,
            &ws_b,
            "app.ts",
            "file:///b/app.ts",
            "b-edit",
            "b",
            1,
            "s1",
            10_000,
        )
        .unwrap();

        // Inject a corrupt/truncated snapshot in workspace A.
        let bucket_a = workspace_bucket(&base, &ws_a);
        fs::write(bucket_a.join("corrupt.json"), "{truncated").unwrap();

        let list_a = list_snapshots_in(&base, &ws_a, 11_000, None).unwrap();
        assert_eq!(list_a.entries.len(), 1);
        assert_eq!(list_a.entries[0].content, "a-edit");
        assert_eq!(list_a.corrupted_count, 1);

        let list_b = list_snapshots_in(&base, &ws_b, 11_000, None).unwrap();
        assert_eq!(list_b.entries.len(), 1);
        assert_eq!(list_b.entries[0].content, "b-edit");
        assert_eq!(list_b.corrupted_count, 0);

        let _ = fs::remove_dir_all(&base);
        let _ = fs::remove_dir_all(&ws_a);
        let _ = fs::remove_dir_all(&ws_b);
    }

    #[test]
    fn refuses_sensitive_files_retires_clean_session_and_prunes_expired() {
        let base = temp_dir("policy-base");
        let ws = temp_dir("policy-ws");
        fs::write(ws.join("notes.md"), "old").unwrap();

        assert!(
            save_snapshot_in(
                &base,
                &ws,
                ".env",
                "file:///ws/.env",
                "SECRET=1",
                "",
                1,
                "s1",
                100
            )
            .is_err()
        );
        assert!(
            save_snapshot_in(
                &base,
                &ws,
                ".github/workflows/ci.yml",
                "file:///ws/.github/workflows/ci.yml",
                "jobs:",
                "",
                1,
                "s1",
                100
            )
            .is_err()
        );

        save_snapshot_in(
            &base,
            &ws,
            "notes.md",
            "file:///ws/notes.md",
            "new",
            "old",
            2,
            "clean",
            1_000,
        )
        .unwrap();
        assert_eq!(retire_session_in(&base, "clean").unwrap(), 1);
        assert!(
            list_snapshots_in(&base, &ws, 2_000, None)
                .unwrap()
                .entries
                .is_empty()
        );

        // Expired snapshot (> 7 days) is pruned on list.
        save_snapshot_in(
            &base,
            &ws,
            "notes.md",
            "file:///ws/notes.md",
            "stale",
            "old",
            2,
            "old-crash",
            1_000,
        )
        .unwrap();
        let after_retention = 1_000 + RETENTION_MS + 1;
        assert!(
            list_snapshots_in(&base, &ws, after_retention, None)
                .unwrap()
                .entries
                .is_empty()
        );

        let _ = fs::remove_dir_all(&base);
        let _ = fs::remove_dir_all(&ws);
    }

    #[cfg(unix)]
    #[test]
    fn refuses_symlinked_recovery_directory() {
        use std::os::unix::fs::symlink;
        let base = temp_dir("sym-base");
        let target = temp_dir("sym-target");
        let ws = temp_dir("sym-ws");
        let bucket = workspace_bucket(&base, &ws);
        symlink(&target, &bucket).unwrap();
        assert!(
            save_snapshot_in(
                &base,
                &ws,
                "a.rs",
                "file:///ws/a.rs",
                "x",
                "y",
                1,
                "s1",
                100
            )
            .is_err()
        );
        let _ = fs::remove_dir_all(&base);
        let _ = fs::remove_dir_all(&target);
        let _ = fs::remove_dir_all(&ws);
    }
}
