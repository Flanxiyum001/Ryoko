//! Read-only Agent Sessions for the desktop sidebar.
//!
//! Ryoko already persists worktree leases in `.ryoko/state/worktrees.sqlite3`
//! so a restarted daemon can recover published branches. This module exposes a
//! bounded, read-only projection of that state to the shell. It never creates,
//! resumes, merges or abandons a session, and it never executes a path or
//! command supplied by an agent.

use rusqlite::{Connection, OpenFlags};
use serde::Serialize;
use tauri::State;

use crate::sessions_projection::{bounded_text, session_state, session_state_path};
use crate::workspace::{WorkspaceState, current_root};

const MAX_SESSIONS: usize = 40;
const MAX_FILES: usize = 40;
const MAX_SUMMARY_CHARS: usize = 240;
const MAX_FILE_CHARS: usize = 240;

/// A safe projection of one persisted worktree lease.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentSession {
    pub id: String,
    pub agent: String,
    pub state: String,
    pub branch: String,
    pub base_branch: String,
    pub files: Vec<String>,
    pub summary: Option<String>,
    pub updated_ms: u64,
}

#[tauri::command]
pub fn list_agent_sessions(state: State<'_, WorkspaceState>) -> Result<Vec<AgentSession>, String> {
    let root = current_root(&state)?;
    let Some(path) = session_state_path(&root)? else {
        return Ok(Vec::new());
    };
    let connection = Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .map_err(|error| format!("cannot open agent session state: {error}"))?;
    let mut statement = connection
        .prepare(
            "SELECT task_id, agent_id, branch_name, base_branch, state, summary, \
             files_json, updated_at FROM worktree_sessions \
             WHERE state IN ('active', 'published') \
             ORDER BY updated_at DESC, task_id LIMIT ?1",
        )
        .map_err(|error| format!("cannot read agent session state: {error}"))?;
    let rows = statement
        .query_map([MAX_SESSIONS as i64], |row| {
            let state_value: String = row.get(4)?;
            let files_json: String = row.get(6)?;
            let updated_at: i64 = row.get(7)?;
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
                state_value,
                row.get::<_, Option<String>>(5)?,
                files_json,
                updated_at,
            ))
        })
        .map_err(|error| format!("cannot read agent session state: {error}"))?;

    let mut sessions = Vec::new();
    for row in rows {
        let (id, agent, branch, base_branch, state_value, summary, files_json, updated_at) =
            row.map_err(|error| format!("cannot read agent session row: {error}"))?;
        let Some(state) = session_state(&state_value) else {
            continue;
        };
        let files = serde_json::from_str::<Vec<String>>(&files_json)
            .unwrap_or_default()
            .into_iter()
            .take(MAX_FILES)
            .map(|path| bounded_text(&path, MAX_FILE_CHARS))
            .collect();
        sessions.push(AgentSession {
            id: bounded_text(&id, MAX_FILE_CHARS),
            agent: bounded_text(&agent, MAX_FILE_CHARS),
            state: state.to_owned(),
            branch: bounded_text(&branch, MAX_FILE_CHARS),
            base_branch: bounded_text(&base_branch, MAX_FILE_CHARS),
            files,
            summary: summary
                .as_deref()
                .map(|summary| bounded_text(summary, MAX_SUMMARY_CHARS)),
            updated_ms: u64::try_from(updated_at).unwrap_or(0).saturating_mul(1_000),
        });
    }
    Ok(sessions)
}
