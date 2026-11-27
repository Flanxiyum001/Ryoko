//! Desktop bridge for `ryoko-harness`.
//!
//! One agent run at a time per window. Progress streams to the UI as
//! `ryoko-harness-event`; command approvals are asked with
//! `ryoko-harness-approval` and answered through [`harness_answer_approval`].
//! Staged edits are kept here until the user applies or discards them. Apply
//! refuses any file that changed on disk since the agent read it.

use std::collections::HashMap;
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use ryoko_harness::{
    ApprovalGate, CommandRequest, Harness, HarnessConfig, HarnessEvent, PlanItem, ProposedChange,
    StopCause,
};
use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Emitter, State};
use tokio::sync::oneshot;

use crate::workspace::{WorkspaceState, current_root};

const MAX_GOAL_CHARS: usize = 8_000;
const APPROVAL_TIMEOUT: Duration = Duration::from_secs(600);

#[derive(Default)]
struct Inner {
    running: Option<(String, Arc<AtomicBool>)>,
    approvals: HashMap<String, oneshot::Sender<bool>>,
    staged: Option<Staged>,
}

struct Staged {
    request_id: String,
    root: PathBuf,
    changes: Vec<ProposedChange>,
}

#[derive(Default)]
pub struct AgentHarnessState {
    inner: Arc<Mutex<Inner>>,
    approval_ids: Arc<AtomicU64>,
}

impl AgentHarnessState {
    /// The staged changes of a finished run, for Ryoko Review.
    pub(crate) fn staged_changes(
        &self,
        request_id: &str,
    ) -> Option<(PathBuf, Vec<ProposedChange>)> {
        let inner = self.inner.lock().expect("harness state");
        inner
            .staged
            .as_ref()
            .filter(|staged| staged.request_id == request_id)
            .map(|staged| (staged.root.clone(), staged.changes.clone()))
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HarnessStartArgs {
    request_id: String,
    goal: String,
    #[serde(default)]
    model: Option<String>,
    #[serde(default)]
    allow_commands: bool,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct EventPayload {
    request_id: String,
    event: HarnessEvent,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct ApprovalPayload {
    request_id: String,
    approval_id: String,
    program: String,
    args: Vec<String>,
    reason: String,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct ChangePayload {
    path: String,
    original: Option<String>,
    content: String,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct DonePayload {
    request_id: String,
    final_text: String,
    stop: StopCause,
    turns: usize,
    plan: Vec<PlanItem>,
    changes: Vec<ChangePayload>,
    estimated_cost_usd: f64,
}

fn valid_request_id(id: &str) -> bool {
    !id.is_empty() && id.len() <= 80 && !id.chars().any(char::is_control)
}

struct DesktopApprovals {
    app: AppHandle,
    inner: Arc<Mutex<Inner>>,
    ids: Arc<AtomicU64>,
    request_id: String,
}

#[async_trait]
impl ApprovalGate for DesktopApprovals {
    async fn approve_command(&self, request: &CommandRequest) -> bool {
        let approval_id = format!("approval-{}", self.ids.fetch_add(1, Ordering::SeqCst));
        let (sender, receiver) = oneshot::channel();
        self.inner
            .lock()
            .expect("harness state")
            .approvals
            .insert(approval_id.clone(), sender);
        let asked = self.app.emit(
            "ryoko-harness-approval",
            ApprovalPayload {
                request_id: self.request_id.clone(),
                approval_id: approval_id.clone(),
                program: request.program.clone(),
                args: request.args.clone(),
                reason: request.reason.clone(),
            },
        );
        let approved = asked.is_ok()
            && matches!(
                tokio::time::timeout(APPROVAL_TIMEOUT, receiver).await,
                Ok(Ok(true))
            );
        self.inner
            .lock()
            .expect("harness state")
            .approvals
            .remove(&approval_id);
        approved
    }
}

#[tauri::command]
pub async fn harness_start(
    args: HarnessStartArgs,
    app: AppHandle,
    workspace: State<'_, WorkspaceState>,
    state: State<'_, AgentHarnessState>,
) -> Result<(), String> {
    if !valid_request_id(&args.request_id) {
        return Err("invalid agent request ID".into());
    }
    let goal = args.goal.trim().to_owned();
    if goal.is_empty() || goal.chars().count() > MAX_GOAL_CHARS {
        return Err("the task must contain 1–8000 characters".into());
    }
    let root = current_root(&workspace)?;
    let (model, router) = crate::model_runtime::load_router(&root, args.model.as_deref())?;
    let mut config = HarnessConfig::new(model);
    config.allow_commands = args.allow_commands;
    config.scope = Some(args.request_id.clone());

    let cancel = {
        let mut inner = state.inner.lock().expect("harness state");
        if inner.running.is_some() {
            return Err("an agent is already working; stop it first".into());
        }
        let cancel = Arc::new(AtomicBool::new(false));
        inner.running = Some((args.request_id.clone(), cancel.clone()));
        inner.staged = None;
        cancel
    };

    let events_app = app.clone();
    let events_id = args.request_id.clone();
    let harness = match Harness::new(&root, router.clone(), config) {
        Ok(harness) => harness,
        Err(error) => {
            state.inner.lock().expect("harness state").running = None;
            return Err(error.to_string());
        }
    };
    let harness = harness
        .with_approvals(Arc::new(DesktopApprovals {
            app: app.clone(),
            inner: state.inner.clone(),
            ids: state.approval_ids.clone(),
            request_id: args.request_id.clone(),
        }))
        .on_event(move |event| {
            let _ = events_app.emit(
                "ryoko-harness-event",
                EventPayload {
                    request_id: events_id.clone(),
                    event,
                },
            );
        });
    let handle = harness.cancel_handle();
    let inner = state.inner.clone();
    let request_id = args.request_id;
    tauri::async_runtime::spawn(async move {
        // Link the stored cancel flag to the harness's own handle.
        let watcher = {
            let (cancel, handle) = (cancel.clone(), handle.clone());
            tauri::async_runtime::spawn(async move {
                while !cancel.load(Ordering::SeqCst) {
                    tokio::time::sleep(Duration::from_millis(150)).await;
                }
                handle.store(true, Ordering::SeqCst);
            })
        };
        let outcome = harness.run(&goal).await;
        watcher.abort();
        let changes = outcome
            .changes
            .iter()
            .map(|change| ChangePayload {
                path: change.path.clone(),
                original: change.original.clone(),
                content: change.content.clone(),
            })
            .collect();
        {
            let mut guard = inner.lock().expect("harness state");
            guard.running = None;
            guard.approvals.clear();
            guard.staged = Some(Staged {
                request_id: request_id.clone(),
                root,
                changes: outcome.changes.clone(),
            });
        }
        let _ = app.emit(
            "ryoko-harness-done",
            DonePayload {
                request_id,
                final_text: outcome.final_text,
                stop: outcome.stop,
                turns: outcome.turns,
                plan: outcome.plan,
                changes,
                estimated_cost_usd: router.ledger().snapshot().total.cost_usd,
            },
        );
    });
    Ok(())
}

#[tauri::command]
pub fn harness_cancel(state: State<'_, AgentHarnessState>) {
    let mut inner = state.inner.lock().expect("harness state");
    if let Some((_, cancel)) = &inner.running {
        cancel.store(true, Ordering::SeqCst);
    }
    for (_, sender) in inner.approvals.drain() {
        let _ = sender.send(false);
    }
}

#[tauri::command]
pub fn harness_answer_approval(
    approval_id: String,
    allow: bool,
    state: State<'_, AgentHarnessState>,
) -> Result<(), String> {
    let sender = state
        .inner
        .lock()
        .expect("harness state")
        .approvals
        .remove(&approval_id)
        .ok_or("this approval request has expired")?;
    let _ = sender.send(allow);
    Ok(())
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ApplyReport {
    applied: Vec<String>,
}

#[tauri::command]
pub fn harness_apply(
    request_id: String,
    paths: Vec<String>,
    state: State<'_, AgentHarnessState>,
) -> Result<ApplyReport, String> {
    let mut inner = state.inner.lock().expect("harness state");
    let staged = inner
        .staged
        .as_mut()
        .filter(|staged| staged.request_id == request_id)
        .ok_or("these changes are no longer available; run the agent again")?;
    let applied = apply_changes(&staged.root, &staged.changes, &paths)?;
    staged
        .changes
        .retain(|change| !applied.contains(&change.path));
    Ok(ApplyReport { applied })
}

#[tauri::command]
pub fn harness_discard(request_id: String, state: State<'_, AgentHarnessState>) {
    let mut inner = state.inner.lock().expect("harness state");
    if inner
        .staged
        .as_ref()
        .is_some_and(|staged| staged.request_id == request_id)
    {
        inner.staged = None;
    }
}

/// Writes the selected staged changes. Every file is checked before any is
/// written, so a conflict leaves the workspace untouched.
fn apply_changes(
    root: &Path,
    changes: &[ProposedChange],
    paths: &[String],
) -> Result<Vec<String>, String> {
    let root = root
        .canonicalize()
        .map_err(|error| format!("workspace is unavailable: {error}"))?;
    let mut plan = Vec::new();
    for path in paths {
        let change = changes
            .iter()
            .find(|change| &change.path == path)
            .ok_or_else(|| format!("`{path}` is not one of the staged changes"))?;
        let target = safe_target(&root, &change.path)?;
        let current = match std::fs::read(&target) {
            Ok(bytes) => Some(String::from_utf8_lossy(&bytes).into_owned()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
            Err(error) => return Err(format!("cannot read `{path}`: {error}")),
        };
        if current != change.original {
            return Err(format!(
                "`{path}` changed after the agent read it; nothing was applied. Run the agent again."
            ));
        }
        plan.push((target, change));
    }
    let mut applied = Vec::new();
    for (target, change) in plan {
        if let Some(parent) = target.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|error| format!("cannot create folder for `{}`: {error}", change.path))?;
            let parent = parent
                .canonicalize()
                .map_err(|error| format!("cannot resolve `{}`: {error}", change.path))?;
            if !parent.starts_with(&root) {
                return Err(format!("`{}` resolves outside the workspace", change.path));
            }
        }
        std::fs::write(&target, &change.content)
            .map_err(|error| format!("cannot write `{}`: {error}", change.path))?;
        applied.push(change.path.clone());
    }
    Ok(applied)
}

fn safe_target(root: &Path, path: &str) -> Result<PathBuf, String> {
    let relative = Path::new(path);
    let ok = !path.is_empty()
        && !path.contains('\\')
        && !relative.is_absolute()
        && relative.components().all(|component| {
            matches!(component, Component::Normal(name) if !name.to_string_lossy().starts_with('.'))
        });
    if !ok {
        return Err(format!("`{path}` is not a safe workspace path"));
    }
    let target = root.join(relative);
    if target.is_symlink() {
        return Err(format!(
            "`{path}` is a symbolic link; refusing to write through it"
        ));
    }
    Ok(target)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn change(path: &str, original: Option<&str>, content: &str) -> ProposedChange {
        ProposedChange {
            path: path.into(),
            original: original.map(str::to_owned),
            content: content.into(),
        }
    }

    #[test]
    fn applies_selected_changes_and_creates_folders() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.rs"), "old").unwrap();
        let changes = [
            change("a.rs", Some("old"), "new"),
            change("src/b.rs", None, "fresh"),
            change("c.rs", None, "skipped"),
        ];
        let applied =
            apply_changes(dir.path(), &changes, &["a.rs".into(), "src/b.rs".into()]).unwrap();
        assert_eq!(applied, ["a.rs", "src/b.rs"]);
        assert_eq!(
            std::fs::read_to_string(dir.path().join("a.rs")).unwrap(),
            "new"
        );
        assert_eq!(
            std::fs::read_to_string(dir.path().join("src/b.rs")).unwrap(),
            "fresh"
        );
        assert!(!dir.path().join("c.rs").exists());
    }

    #[test]
    fn refuses_conflicts_without_writing_anything() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.rs"), "edited by the user").unwrap();
        let changes = [
            change("new.rs", None, "x"),
            change("a.rs", Some("old"), "new"),
        ];
        let error =
            apply_changes(dir.path(), &changes, &["new.rs".into(), "a.rs".into()]).unwrap_err();
        assert!(error.contains("changed after the agent read it"));
        assert!(!dir.path().join("new.rs").exists());
    }

    #[test]
    fn rejects_unsafe_paths() {
        let dir = tempfile::tempdir().unwrap();
        for bad in ["../x.rs", "/etc/x", ".git/config", "a\\b"] {
            let changes = [change(bad, None, "x")];
            assert!(
                apply_changes(dir.path(), &changes, &[bad.into()]).is_err(),
                "{bad}"
            );
        }
    }
}
