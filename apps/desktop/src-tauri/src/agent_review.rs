//! Read-only review of files on published, isolated agent branches.
//! Nothing is cherry-picked, merged, staged or written by this command.

use std::path::Path;
use std::process::Command;

use rusqlite::{Connection, OpenFlags, params};
use serde::Serialize;
use tauri::State;

use crate::paths::safe_join;
use crate::sessions_projection::session_state_path;
use crate::workspace::{WorkspaceState, current_root};

const MAX_REVIEW_BYTES: u64 = 256 * 1024;

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentCandidate {
    pub path: String,
    pub branch: String,
    pub content: String,
}

fn valid_branch(branch: &str) -> bool {
    branch.starts_with("ryoko/")
        && branch.len() <= 200
        && branch.chars().all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '/' | '-' | '_' | '.'))
        && !branch.contains("..")
}

fn git_blob(root: &Path, branch: &str, path: &str) -> Result<String, String> {
    if !valid_branch(branch) {
        return Err("agent branch is not a managed Ryoko branch".into());
    }
    let spec = format!("{branch}:{path}");
    let size = Command::new("git")
        .arg("-C").arg(root)
        .arg("cat-file").arg("-s").arg(&spec)
        .output()
        .map_err(|error| format!("cannot inspect proposal: {error}"))?;
    if !size.status.success() {
        return Err("the proposed file is not present on the published branch".into());
    }
    let bytes: u64 = String::from_utf8_lossy(&size.stdout).trim().parse()
        .map_err(|_| "invalid Git blob size")?;
    if bytes > MAX_REVIEW_BYTES {
        return Err("proposal file exceeds the 256 KiB review limit".into());
    }
    let result = Command::new("git")
        .arg("-C").arg(root)
        .arg("show").arg(&spec)
        .output()
        .map_err(|error| format!("cannot read proposal: {error}"))?;
    if !result.status.success() || result.stdout.len() as u64 > MAX_REVIEW_BYTES {
        return Err("could not read the proposed file".into());
    }
    String::from_utf8(result.stdout)
        .map_err(|_| "binary proposals are not editable in text review".into())
}

fn candidate_at(root: &Path, task_id: &str, path: &str) -> Result<AgentCandidate, String> {
    if task_id.len() > 240 || task_id.is_empty() {
        return Err("invalid agent session".into());
    }
    safe_join(root, path)?;
    let db = session_state_path(root)?
        .ok_or_else(|| "no agent session store is available".to_owned())?;
    let connection = Connection::open_with_flags(
        db, OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    ).map_err(|error| format!("cannot open agent session database: {error}"))?;
    let (branch, files_json): (String, String) = connection.query_row(
        "SELECT branch_name, files_json FROM worktree_sessions WHERE task_id=?1 AND state='published'",
        params![task_id],
        |row| Ok((row.get(0)?, row.get(1)?)),
    ).map_err(|_| "only published agent sessions can be reviewed".to_owned())?;
    let files: Vec<String> = serde_json::from_str(&files_json)
        .map_err(|_| "invalid agent file manifest".to_owned())?;
    if !files.iter().any(|entry| entry == path) {
        return Err("file is not listed in this agent proposal".into());
    }
    let content = git_blob(root, &branch, path)?;
    Ok(AgentCandidate { path: path.to_owned(), branch, content })
}

#[tauri::command]
pub async fn read_agent_candidate(
    task_id: String,
    path: String,
    state: State<'_, WorkspaceState>,
) -> Result<AgentCandidate, String> {
    let root = current_root(&state)?;
    tauri::async_runtime::spawn_blocking(move || candidate_at(&root, &task_id, &path))
        .await
        .map_err(|_| "agent review worker failed".to_owned())?
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_managed_branches_are_accepted() {
        assert!(valid_branch("ryoko/backend/task-123"));
        for invalid in ["main", "-bad", "ryoko/../main", "ryoko/a^b", "ryoko/a:b", "ryoko/a b"] {
            assert!(!valid_branch(invalid), "{invalid}");
        }
    }

    #[test]
    fn cannot_review_outside_workspace_or_without_a_session() {
        let root = std::env::temp_dir();
        assert!(candidate_at(&root, "task", "../secrets").is_err());
        assert!(candidate_at(&root, "task", "src/main.rs").is_err());
    }
}
