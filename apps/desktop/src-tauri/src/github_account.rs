//! Native GitHub account for the Ryoko IDE.
//!
//! One sign-in gives the workbench three things it can use directly:
//!
//! - the account's repositories, listed and cloned into a sibling folder and
//!   opened as the workspace;
//! - fetch / fast-forward pull / push for the open checkout;
//! - GitHub Copilot as a selectable model provider.
//!
//! Security rules this module keeps:
//!
//! * The long-lived token lives in memory for the session and, when the host's
//!   Git credential helper can hold it, in that OS-backed store. It is never
//!   written into the workspace, an event log, a rendered status or an IPC
//!   payload — only [`GitHubStatus`] (login, source, persistence) leaves here.
//! * HTTP is performed by `ryoko-github`, whose transport refuses redirects,
//!   bounds every body and never logs a request.
//! * Cloning and syncing re-check the repository against the account's own
//!   list, and Git receives the token through an ephemeral credential helper
//!   and the environment, never through argv.
//! * Copilot is only offered when GitHub reports models for the signed-in
//!   account; the provider catalog is written on explicit user action.

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use ryoko_github::{
    CredentialSource, CredentialStore, DeviceFlow, DeviceFlowStart, GitCredentialStore,
    GitHubClient, GitHubRepository, PollOutcome, ReqwestTransport, StoredCredential, SyncAction,
    SyncOutcome, SyncStatus, Transport,
};
use ryoko_models::{CopilotConfig, CopilotProvider};
use serde::Serialize;
use tauri::State;

use crate::workspace::{WorkspaceInfo, WorkspaceState};

/// How long a sign-in attempt is kept before the user must start again.
const LOGIN_TTL: Duration = Duration::from_secs(15 * 60);
const MAX_REPOSITORIES: usize = 500;

/// State shared by the GitHub commands. Nothing here is serialized.
#[derive(Default)]
pub struct GitHubAccountState {
    session: Mutex<Option<StoredCredential>>,
    pending: Mutex<Option<PendingLogin>>,
}

struct PendingLogin {
    flow: DeviceFlow,
    start: DeviceFlowStart,
    started: Instant,
}

impl PendingLogin {
    fn expired(&self) -> bool {
        self.started.elapsed() > LOGIN_TTL
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GitHubStatus {
    pub connected: bool,
    pub login: String,
    /// `environment`, `credential_store`, `session` or `none`.
    pub source: String,
    /// Whether the token is held by the host credential store (survives a
    /// restart) rather than only this session.
    pub persisted: bool,
    /// Whether an OAuth App client ID is configured, which device sign-in needs.
    pub sign_in_configured: bool,
    /// Whether the host has any GitHub credential already stored.
    pub stored_credential_available: bool,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GitHubLoginStart {
    pub user_code: String,
    pub verification_uri: String,
    pub expires_in: u64,
    pub interval: u64,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GitHubLoginPoll {
    /// `pending`, `connected`, `denied` or `expired`.
    pub state: String,
    pub status: GitHubStatus,
    pub message: String,
}

fn transport() -> Result<Arc<dyn Transport>, String> {
    ReqwestTransport::new()
        .map(|transport| Arc::new(transport) as Arc<dyn Transport>)
        .map_err(|error| error.to_string())
}

fn source_name(source: CredentialSource) -> &'static str {
    match source {
        CredentialSource::Environment => "environment",
        CredentialSource::CredentialStore => "credential_store",
        CredentialSource::Session => "session",
    }
}

fn lock<T>(value: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    value.lock().unwrap_or_else(|error| error.into_inner())
}

fn session(state: &State<'_, GitHubAccountState>) -> Option<StoredCredential> {
    lock(&state.session).clone()
}

/// The current token for callers that build a model provider. Never rendered.
pub fn copilot_token(state: &State<'_, GitHubAccountState>) -> Option<String> {
    session(state).map(|credential| credential.token.expose().to_owned())
}

fn store() -> GitCredentialStore {
    GitCredentialStore::new()
}

fn status_of(
    credential: Option<&StoredCredential>,
    sign_in_configured: bool,
    stored_available: bool,
) -> GitHubStatus {
    GitHubStatus {
        connected: credential.is_some(),
        login: credential
            .map(|credential| credential.login.clone())
            .unwrap_or_default(),
        source: credential
            .map(|credential| source_name(credential.source).to_owned())
            .unwrap_or_else(|| "none".to_owned()),
        persisted: credential
            .is_some_and(|credential| credential.source == CredentialSource::CredentialStore),
        sign_in_configured,
        stored_credential_available: stored_available,
    }
}

fn sign_in_configured() -> bool {
    std::env::var(ryoko_github::auth::CLIENT_ID_ENV)
        .map(|value| !value.trim().is_empty())
        .unwrap_or(false)
}

/// Reads the status without contacting GitHub.
#[tauri::command]
pub async fn github_status(state: State<'_, GitHubAccountState>) -> Result<GitHubStatus, String> {
    let current = session(&state);
    let stored_available = if current.is_some() {
        true
    } else {
        match store().load().await {
            Ok(found) => found.is_some(),
            Err(_) => false,
        }
    };
    Ok(status_of(
        current.as_ref(),
        sign_in_configured(),
        stored_available,
    ))
}

/// Uses a GitHub credential the host already has: `RYOKO_GITHUB_TOKEN`, or a
/// token Git's own credential helper stored for github.com (which is what an
/// existing `git push` over HTTPS would use).
#[tauri::command]
pub async fn github_connect_existing(
    state: State<'_, GitHubAccountState>,
) -> Result<GitHubStatus, String> {
    let store = store();
    let credential = ryoko_github::store::load_preferred(&store)
        .await
        .map_err(|error| error.to_string())?
        .ok_or_else(|| {
            "no stored GitHub credential was found; sign in, or store one with Git first".to_owned()
        })?;
    let client = GitHubClient::new(transport()?, credential.token.clone());
    let user = client.user().await.map_err(|error| match error {
        ryoko_github::GitHubError::Unauthorized => {
            "the stored GitHub credential was rejected; sign in again".to_owned()
        }
        other => other.to_string(),
    })?;
    let discovered_source = credential.source;
    let credential = StoredCredential {
        login: user.login,
        ..credential
    };
    if discovered_source != CredentialSource::CredentialStore {
        // Best effort: an environment token becomes a stored credential so the
        // next session starts connected. A helper that stores nothing is not an
        // error the user needs to see here.
        let _ = store.save(&credential).await;
    }
    *lock(&state.session) = Some(credential.clone());
    Ok(status_of(Some(&credential), sign_in_configured(), true))
}

/// Begins an OAuth device-flow sign-in. The user approves in their browser;
/// nothing listens on a local port and no client secret is involved.
#[tauri::command]
pub async fn github_sign_in_start(
    state: State<'_, GitHubAccountState>,
) -> Result<GitHubLoginStart, String> {
    let flow = DeviceFlow::from_environment(transport()?).map_err(|_| {
        format!(
            "GitHub sign-in needs an OAuth App client ID in {}",
            ryoko_github::auth::CLIENT_ID_ENV
        )
    })?;
    let start = flow.start().await.map_err(|error| error.to_string())?;
    let response = GitHubLoginStart {
        user_code: start.user_code.clone(),
        verification_uri: start.verification_uri.clone(),
        expires_in: start.expires_in,
        interval: start.interval,
    };
    *lock(&state.pending) = Some(PendingLogin {
        flow,
        start,
        started: Instant::now(),
    });
    Ok(response)
}

/// Polls the pending sign-in. Repeated calls are safe; a finished or expired
/// attempt reports its outcome and is dropped.
#[tauri::command]
pub async fn github_sign_in_poll(
    state: State<'_, GitHubAccountState>,
) -> Result<GitHubLoginPoll, String> {
    let pending = lock(&state.pending).take();
    let Some(pending) = pending else {
        return Err("no GitHub sign-in is in progress; start one first".to_owned());
    };
    if pending.expired() {
        return Ok(GitHubLoginPoll {
            state: "expired".to_owned(),
            status: status_of(session(&state).as_ref(), sign_in_configured(), false),
            message: "The sign-in code expired. Start again.".to_owned(),
        });
    }
    let outcome = pending.flow.poll(&pending.start.device_code).await;
    let outcome = match outcome {
        Ok(outcome) => outcome,
        Err(error) => {
            // Keep the attempt alive only for a transport failure, where a
            // retry is meaningful.
            if matches!(error, ryoko_github::GitHubError::Transport(_)) {
                *lock(&state.pending) = Some(pending);
            }
            return Err(error.to_string());
        }
    };
    match outcome {
        PollOutcome::Pending => {
            *lock(&state.pending) = Some(pending);
            Ok(GitHubLoginPoll {
                state: "pending".to_owned(),
                status: status_of(session(&state).as_ref(), sign_in_configured(), false),
                message: "Waiting for approval in the browser.".to_owned(),
            })
        }
        PollOutcome::SlowDown { interval } => {
            *lock(&state.pending) = Some(pending);
            Ok(GitHubLoginPoll {
                state: "pending".to_owned(),
                status: status_of(session(&state).as_ref(), sign_in_configured(), false),
                message: format!("GitHub asked for slower polling; next check in {interval}s."),
            })
        }
        PollOutcome::Denied => Ok(GitHubLoginPoll {
            state: "denied".to_owned(),
            status: status_of(session(&state).as_ref(), sign_in_configured(), false),
            message: "Sign-in was cancelled on GitHub.".to_owned(),
        }),
        PollOutcome::Expired => Ok(GitHubLoginPoll {
            state: "expired".to_owned(),
            status: status_of(session(&state).as_ref(), sign_in_configured(), false),
            message: "The sign-in code expired. Start again.".to_owned(),
        }),
        PollOutcome::Authorized { token, scopes } => {
            let client = GitHubClient::new(transport()?, token.clone());
            let user = client.user().await.map_err(|error| error.to_string())?;
            let credential = StoredCredential {
                token,
                login: user.login,
                scopes,
                source: CredentialSource::Session,
            };
            // Persist through the host helper when it can hold it; a rejected
            // write leaves a session-only credential rather than failing the
            // sign-in the user just completed.
            let persisted = store().save(&credential).await.is_ok();
            let credential = StoredCredential {
                source: if persisted {
                    CredentialSource::CredentialStore
                } else {
                    CredentialSource::Session
                },
                ..credential
            };
            *lock(&state.session) = Some(credential.clone());
            Ok(GitHubLoginPoll {
                state: "connected".to_owned(),
                status: status_of(Some(&credential), true, persisted),
                message: if persisted {
                    format!("Signed in as {}.", credential.login)
                } else {
                    format!(
                        "Signed in as {} for this session only: the host credential helper \
                         stored nothing.",
                        credential.login
                    )
                },
            })
        }
    }
}

#[tauri::command]
pub async fn github_sign_out(state: State<'_, GitHubAccountState>) -> Result<GitHubStatus, String> {
    *lock(&state.session) = None;
    *lock(&state.pending) = None;
    // A helper that holds nothing to erase is not a failure to sign out.
    let _ = store().clear().await;
    Ok(status_of(None, sign_in_configured(), false))
}

async fn client_for(state: &State<'_, GitHubAccountState>) -> Result<GitHubClient, String> {
    let credential = session(state).ok_or_else(|| "connect a GitHub account first".to_owned())?;
    Ok(GitHubClient::new(transport()?, credential.token))
}

/// Every repository the account owns or collaborates on, most recently
/// updated first, bounded by the crate.
#[tauri::command]
pub async fn github_repositories(
    state: State<'_, GitHubAccountState>,
) -> Result<Vec<GitHubRepository>, String> {
    let client = client_for(&state).await?;
    let mut repositories = client.repositories().await.map_err(|error| match error {
        ryoko_github::GitHubError::Unauthorized => {
            "GitHub rejected this session; sign in again".to_owned()
        }
        other => other.to_string(),
    })?;
    if repositories.len() > MAX_REPOSITORIES {
        repositories.truncate(MAX_REPOSITORIES);
    }
    Ok(repositories)
}

/// Clones one of the account's repositories into `parent/<name>`.
///
/// The repository is looked up in the account's own list rather than trusting
/// a URL from the frontend, and an existing directory is never overwritten. The
/// caller opens the returned folder through the editor's own guarded flow, so a
/// clone never discards unsaved buffers behind the user's back.
#[tauri::command]
pub async fn github_clone_repository(
    repository: String,
    parent: String,
    state: State<'_, GitHubAccountState>,
) -> Result<WorkspaceInfo, String> {
    let requested = repository.trim();
    if requested.is_empty() || requested.len() > 200 {
        return Err("choose a repository to clone".to_owned());
    }
    let destination = std::path::PathBuf::from(parent.trim());
    if !destination.is_absolute() {
        return Err("choose an existing folder for the clone".to_owned());
    }
    let credential = session(&state).ok_or_else(|| "connect a GitHub account first".to_owned())?;
    let client = GitHubClient::new(transport()?, credential.token.clone());
    let repositories = client
        .repositories()
        .await
        .map_err(|error| error.to_string())?;
    let selected = repositories
        .into_iter()
        .find(|candidate| candidate.full_name == requested)
        .ok_or_else(|| format!("{requested} is not a repository this account can access"))?;
    let cloned = ryoko_github::sync::clone_repository(&destination, &selected, Some(&credential))
        .await
        .map_err(|error| error.to_string())?;
    let name = cloned
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| selected.name.clone());
    Ok(WorkspaceInfo {
        root: cloned.display().to_string(),
        name,
    })
}

/// Branch, upstream, ahead/behind and dirty state for the open checkout.
#[tauri::command]
pub async fn git_sync_status(
    _state: State<'_, GitHubAccountState>,
    workspace: State<'_, WorkspaceState>,
) -> Result<SyncStatus, String> {
    let root = crate::workspace::current_root(&workspace)?;
    ryoko_github::sync::status(&root)
        .await
        .map_err(|error| error.to_string())
}

/// Fetch, fast-forward pull or push the open checkout.
#[tauri::command]
pub async fn git_sync(
    action: String,
    state: State<'_, GitHubAccountState>,
    workspace: State<'_, WorkspaceState>,
) -> Result<SyncOutcome, String> {
    let action = match action.trim() {
        "fetch" => SyncAction::Fetch,
        "pull" => SyncAction::Pull,
        "push" => SyncAction::Push,
        _ => return Err("sync action must be fetch, pull or push".to_owned()),
    };
    let root = crate::workspace::current_root(&workspace)?;
    let credential = session(&state);
    ryoko_github::sync::run(&root, action, credential.as_ref())
        .await
        .map_err(|error| error.to_string())
}

/// Models GitHub Copilot offers the signed-in account. An empty list means the
/// account has no Copilot seat, so the UI hides the Copilot controls.
#[tauri::command]
pub async fn github_copilot_models(
    state: State<'_, GitHubAccountState>,
) -> Result<Vec<String>, String> {
    let token = copilot_token(&state).ok_or_else(|| "connect a GitHub account first".to_owned())?;
    let provider = CopilotProvider::new(CopilotConfig::new("github-copilot", token))
        .map_err(|error| error.to_string())?;
    provider.models().await.map_err(|error| error.to_string())
}

/// Writes a validated provider catalog and default model so the agent, inline
/// edit and review paths use GitHub Copilot. This is the only place the shell
/// changes model settings, and it only runs on an explicit user action.
#[tauri::command]
pub async fn github_use_copilot(
    model: String,
    state: State<'_, GitHubAccountState>,
    workspace: State<'_, WorkspaceState>,
) -> Result<String, String> {
    let model = model.trim();
    if model.is_empty()
        || model.len() > 120
        || !model
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-' | b'_'))
    {
        return Err("choose a Copilot model from the list".to_owned());
    }
    let token = copilot_token(&state).ok_or_else(|| "connect a GitHub account first".to_owned())?;
    let provider = CopilotProvider::new(CopilotConfig::new("github-copilot", token))
        .map_err(|error| error.to_string())?;
    let available = provider.models().await.map_err(|error| error.to_string())?;
    if !available.iter().any(|candidate| candidate == model) {
        return Err("that model is not available to this Copilot account".to_owned());
    }
    let root = crate::workspace::current_root(&workspace)?;
    let catalog = serde_json::json!({
        "default_provider": "github-copilot",
        "providers": [{
            "id": "github-copilot",
            "base_url": "https://api.githubcopilot.com",
            "api_key_env": "RYOKO_GITHUB_TOKEN",
            "wire": "copilot"
        }]
    });
    let written = crate::settings::save_providers(&root, &catalog.to_string())?;
    let qualified = format!("github-copilot/{model}");
    crate::settings::save_default_model(&root, &qualified)?;
    Ok(format!(
        "Copilot model {qualified} saved for this workspace. The token stays in the signed-in \
         session. Catalog: {written}"
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_disconnected_status_has_no_login() {
        let status = status_of(None, true, false);
        assert!(!status.connected);
        assert_eq!(status.source, "none");
        assert!(status.login.is_empty());
    }

    #[test]
    fn a_stored_credential_reports_persistence() {
        let credential = StoredCredential {
            token: ryoko_github::Secret::new("gho_x"),
            login: "octocat".to_owned(),
            scopes: vec![],
            source: CredentialSource::CredentialStore,
        };
        let status = status_of(Some(&credential), true, true);
        assert!(status.connected && status.persisted);
        assert_eq!(status.source, "credential_store");
        assert_eq!(status.login, "octocat");
    }
}
