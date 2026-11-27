//! Desktop commands for Ryoko Review and the agent swarm.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use ryoko_harness::review::{self, ReviewReport};
use ryoko_harness::swarm::push_branch;
use ryoko_harness::{GitSwarmWorkspace, Swarm, SwarmConfig, SwarmEvent};
use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Emitter, State};

use crate::agent_harness::AgentHarnessState;
use crate::fast_context::FastContextState;
use crate::workspace::{WorkspaceState, current_root};

#[derive(Deserialize)]
#[serde(tag = "source", rename_all = "snake_case")]
pub enum ReviewSource {
    /// Staged edits from a finished agent run.
    Agent {
        #[serde(rename = "requestId")]
        request_id: String,
    },
    /// Uncommitted changes in the workspace (`git diff HEAD`).
    WorkingTree,
}

#[tauri::command]
pub async fn ryoko_review(
    source: ReviewSource,
    workspace: State<'_, WorkspaceState>,
    harness: State<'_, AgentHarnessState>,
    fast_context: State<'_, FastContextState>,
) -> Result<ReviewReport, String> {
    let root = current_root(&workspace)?;
    let diff = match source {
        ReviewSource::Agent { request_id } => {
            let (_, changes) = harness
                .staged_changes(&request_id)
                .ok_or("those agent changes are no longer available")?;
            review::diff_changes(&changes)
        }
        ReviewSource::WorkingTree => {
            let output = tokio::process::Command::new("git")
                .args(["diff", "--no-color", "--no-ext-diff", "HEAD"])
                .current_dir(&root)
                .stdin(std::process::Stdio::null())
                .output()
                .await
                .map_err(|error| format!("could not run git: {error}"))?;
            if !output.status.success() {
                return Err("this folder is not a Git repository with a commit yet".into());
            }
            String::from_utf8_lossy(&output.stdout).into_owned()
        }
    };
    if diff.trim().is_empty() {
        return Err("there are no changes to review".into());
    }
    let (model, router) = crate::model_runtime::load_router(&root, None)?;
    let index = fast_context.index(&root).ok();
    review::review_diff(&router, &model, &diff, index.as_deref())
        .await
        .map_err(|error| error.to_string())
}

#[derive(Default)]
pub struct SwarmState {
    running: Arc<AtomicBool>,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct SwarmEventPayload {
    request_id: String,
    event: SwarmEvent,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct SwarmDonePayload {
    request_id: String,
    report: ryoko_harness::SwarmReport,
}

#[tauri::command]
pub async fn swarm_start(
    request_id: String,
    goal: String,
    app: AppHandle,
    workspace: State<'_, WorkspaceState>,
    state: State<'_, SwarmState>,
) -> Result<(), String> {
    if request_id.is_empty() || request_id.len() > 80 || request_id.chars().any(char::is_control) {
        return Err("invalid swarm request ID".into());
    }
    let goal = goal.trim().to_owned();
    if goal.is_empty() || goal.chars().count() > 8000 {
        return Err("the task must contain 1–8000 characters".into());
    }
    let root = current_root(&workspace)?;
    if !root.join(".git").exists() {
        return Err("parallel tasks need a Git repository; initialise Git first".into());
    }
    let (model, router) = crate::model_runtime::load_router(&root, None)?;
    if state.running.swap(true, Ordering::SeqCst) {
        return Err("a swarm is already running".into());
    }
    let running = state.running.clone();
    let events_app = app.clone();
    let events_id = request_id.clone();
    let swarm = Swarm::new(
        router,
        Arc::new(GitSwarmWorkspace::new(&root)),
        SwarmConfig::new(model),
    )
    .on_event(move |event| {
        let _ = events_app.emit(
            "ryoko-swarm-event",
            SwarmEventPayload {
                request_id: events_id.clone(),
                event,
            },
        );
    });
    tauri::async_runtime::spawn(async move {
        let report = swarm.run(&goal).await;
        running.store(false, Ordering::SeqCst);
        let _ = app.emit("ryoko-swarm-done", SwarmDonePayload { request_id, report });
    });
    Ok(())
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PushResult {
    pull_request_url: Option<String>,
}

/// Pushes one swarm branch. Only called from a user's click.
#[tauri::command]
pub async fn swarm_push(
    branch: String,
    base: String,
    title: String,
    body: String,
    workspace: State<'_, WorkspaceState>,
) -> Result<PushResult, String> {
    if !branch.starts_with("ryoko/") {
        return Err("only Ryoko task branches can be pushed from here".into());
    }
    let root = current_root(&workspace)?;
    let title: String = title.chars().take(200).collect();
    let body: String = body.chars().take(20_000).collect();
    Ok(PushResult {
        pull_request_url: push_branch(&root, &branch, &base, &title, &body).await?,
    })
}
