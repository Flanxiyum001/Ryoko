//! Read-only Tauri-native GitHub queue over the existing localhost App broker.
//! The OAuth token remains inside Node; a short-lived bridge token remains
//! inside this process and is never passed to Monaco or persisted on disk.
use std::sync::Mutex;
use std::time::Duration;

use reqwest::Client;
use serde::{Deserialize, Serialize};
use tauri::{AppHandle, State};
use tauri_plugin_dialog::{DialogExt, MessageDialogButtons};

const BROKER: &str = "http://127.0.0.1:4173";
const MAX_RESPONSE_BYTES: usize = 256 * 1024;

#[derive(Default)]
pub struct GitHubQueueState {
    token: Mutex<Option<String>>,
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GitHubRepository {
    installation_id: u64,
    repo_id: u64,
    name: String,
}

#[derive(Serialize, Deserialize)]
pub struct GitHubQueueItem {
    number: u64,
    title: String,
    html_url: String,
    #[serde(default)]
    draft: bool,
    #[serde(default)]
    state: String,
}

#[derive(Serialize, Deserialize)]
pub struct GitHubQueue {
    repository: String,
    pulls: Vec<GitHubQueueItem>,
    issues: Vec<GitHubQueueItem>,
}

#[derive(Deserialize)]
struct Paired {
    token: String,
}

#[derive(Deserialize)]
struct Repositories {
    repositories: Vec<GitHubRepository>,
}

fn client() -> Result<Client, String> {
    Client::builder()
        .timeout(Duration::from_secs(8))
        .redirect(reqwest::redirect::Policy::none())
        .no_proxy()
        .build()
        .map_err(|_| "could not initialize local GitHub broker connection".to_owned())
}

async fn bounded_json<T: serde::de::DeserializeOwned>(
    mut response: reqwest::Response,
) -> Result<T, String> {
    if !response.status().is_success() {
        return Err(format!(
            "GitHub broker returned HTTP {}. Sign in or pair again.",
            response.status().as_u16()
        ));
    }
    if response
        .content_length()
        .is_some_and(|len| len > MAX_RESPONSE_BYTES as u64)
    {
        return Err("GitHub queue response too large".into());
    }
    let mut bytes = Vec::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|_| "GitHub broker read failed")?
    {
        if bytes.len().saturating_add(chunk.len()) > MAX_RESPONSE_BYTES {
            return Err("GitHub queue response exceeded limit".into());
        }
        bytes.extend_from_slice(&chunk);
    }
    serde_json::from_slice(&bytes).map_err(|_| "invalid response from local GitHub broker".into())
}

fn token(state: &State<'_, GitHubQueueState>) -> Result<String, String> {
    state
        .token
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .clone()
        .ok_or_else(|| "Pair GitHub from the connected browser workbench first".into())
}

#[tauri::command]
pub async fn pair_github_queue(
    code: String,
    state: State<'_, GitHubQueueState>,
) -> Result<(), String> {
    if code.len() != 43
        || !code
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_')
    {
        return Err("invalid single-use desktop pairing code".into());
    }
    let response = client()?
        .post(format!("{BROKER}/api/desktop/exchange"))
        .json(&serde_json::json!({"code":code}))
        .send()
        .await
        .map_err(|_| "cannot reach local GitHub App at 127.0.0.1:4173".to_owned())?;
    let paired: Paired = bounded_json(response).await?;
    if paired.token.len() != 43
        || !paired
            .token
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_')
    {
        return Err("GitHub broker supplied an invalid pairing token".into());
    }
    *state
        .token
        .lock()
        .unwrap_or_else(|error| error.into_inner()) = Some(paired.token);
    Ok(())
}

#[tauri::command]
pub fn disconnect_github_queue(state: State<'_, GitHubQueueState>) {
    *state
        .token
        .lock()
        .unwrap_or_else(|error| error.into_inner()) = None;
}

#[tauri::command]
pub async fn list_github_repositories(
    state: State<'_, GitHubQueueState>,
) -> Result<Vec<GitHubRepository>, String> {
    let response = client()?
        .get(format!("{BROKER}/api/desktop/repositories"))
        .bearer_auth(token(&state)?)
        .send()
        .await
        .map_err(|_| "cannot reach local GitHub App".to_owned())?;
    let data: Repositories = bounded_json(response).await?;
    if data.repositories.len() > 100 {
        return Err("GitHub repository count exceeded limit".into());
    }
    Ok(data.repositories)
}

#[tauri::command]
pub async fn get_github_queue(
    installation_id: u64,
    repo_id: u64,
    state: State<'_, GitHubQueueState>,
) -> Result<GitHubQueue, String> {
    if installation_id == 0 || repo_id == 0 {
        return Err("choose an authorized GitHub repository".into());
    }
    let response = client()?
        .get(format!(
            "{BROKER}/api/desktop/queue?installationId={installation_id}&repoId={repo_id}"
        ))
        .bearer_auth(token(&state)?)
        .send()
        .await
        .map_err(|_| "cannot reach local GitHub App".to_owned())?;
    let data: GitHubQueue = bounded_json(response).await?;
    if data.pulls.len() > 30 || data.issues.len() > 30 {
        return Err("GitHub queue exceeded its item limit".into());
    }
    Ok(data)
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GitHubProposal {
    id: String,
    source: String,
    operation: String,
    status: String,
    params: serde_json::Value,
    created_at: String,
}

#[derive(Deserialize)]
struct ProposalList {
    proposals: Vec<GitHubProposal>,
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GitHubJob {
    issue: u64,
    status: String,
    #[serde(default)]
    number: Option<u64>,
}

#[derive(Deserialize)]
struct JobList {
    jobs: Vec<GitHubJob>,
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GitHubChecks {
    ready: bool,
    #[serde(default)]
    sha: Option<String>,
    reason: String,
    #[serde(default)]
    checks: Option<u64>,
    #[serde(default)]
    statuses: Option<u64>,
}

#[derive(Deserialize)]
struct VerifiedIssue {
    repository: String,
    number: u64,
    title: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct TrustedProposalPreview {
    repository: String,
    operation: String,
    params: serde_json::Value,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GitHubDecision {
    id: String,
    status: String,
    #[serde(default)]
    result: Option<serde_json::Value>,
}

fn checked_repo(installation_id: u64, repo_id: u64) -> Result<(), String> {
    if installation_id == 0 || repo_id == 0 {
        return Err("choose an authorized GitHub App repository".into());
    }
    Ok(())
}

#[tauri::command]
pub async fn list_github_proposals(
    installation_id: u64,
    repo_id: u64,
    state: State<'_, GitHubQueueState>,
) -> Result<Vec<GitHubProposal>, String> {
    checked_repo(installation_id, repo_id)?;
    let response = client()?
        .get(format!(
            "{BROKER}/api/desktop/proposals?installationId={installation_id}&repoId={repo_id}"
        ))
        .bearer_auth(token(&state)?)
        .send()
        .await
        .map_err(|_| "cannot reach local GitHub App".to_owned())?;
    let list: ProposalList = bounded_json(response).await?;
    if list.proposals.len() > 50 {
        return Err("broker returned too many review proposals".into());
    }
    Ok(list.proposals)
}

#[tauri::command]
pub async fn create_github_proposal(
    installation_id: u64,
    repo_id: u64,
    operation: String,
    params: serde_json::Value,
    state: State<'_, GitHubQueueState>,
) -> Result<String, String> {
    checked_repo(installation_id, repo_id)?;
    if ![
        "clone_repository",
        "create_branch",
        "commit_file",
        "open_pull_request",
        "review_comment",
        "mark_ready_for_review",
    ]
    .contains(&operation.as_str())
    {
        return Err("unsupported native GitHub write proposal".into());
    }
    let response = client()?
        .post(format!("{BROKER}/api/desktop/proposals"))
        .bearer_auth(token(&state)?)
        .json(&serde_json::json!({
            "installationId": installation_id,
            "repoId": repo_id,
            "operation": operation,
            "params": params,
        }))
        .send()
        .await
        .map_err(|_| "cannot reach local GitHub App".to_owned())?;
    let result: GitHubDecision = bounded_json(response).await?;
    if result.status != "pending" {
        return Err("broker did not queue a reviewable proposal".into());
    }
    Ok(result.id)
}

#[tauri::command]
pub async fn decide_github_proposal(
    id: String,
    decision: String,
    state: State<'_, GitHubQueueState>,
    app: AppHandle,
) -> Result<serde_json::Value, String> {
    if id.len() != 36
        || !id.bytes().all(|b| b.is_ascii_hexdigit() || b == b'-')
        || !["approve", "reject"].contains(&decision.as_str())
    {
        return Err("invalid GitHub proposal decision".into());
    }
    if decision == "approve" {
        // Fetch immutable proposal details from the trusted broker, not the
        // JavaScript renderer. The model/webview cannot manufacture consent.
        let preview_response = client()?
            .get(format!("{BROKER}/api/desktop/proposals/{id}"))
            .bearer_auth(token(&state)?)
            .send()
            .await
            .map_err(|_| "cannot fetch trusted GitHub approval details".to_owned())?;
        let preview: TrustedProposalPreview = bounded_json(preview_response).await?;
        let summary = serde_json::to_string_pretty(&preview.params)
            .map_err(|_| "cannot display proposal parameters".to_owned())?
            .chars()
            .take(1200)
            .collect::<String>();
        let description = format!(
            "Approve a real GitHub write to {}?\n\nOperation: {}\nParameters (preview):\n{}\n\nReview all fields in the Ryoko inbox. No default branch pushes or automatic merges are allowed.",
            preview.repository, preview.operation, summary
        );
        let confirmed = tauri::async_runtime::spawn_blocking(move || {
            app.dialog()
                .message(description)
                .title("Ryoko — authorize GitHub write")
                .buttons(MessageDialogButtons::OkCancelCustom(
                    "Approve once".to_owned(),
                    "Deny".to_owned(),
                ))
                .blocking_show()
        })
        .await
        .map_err(|_| "could not display OS GitHub approval dialog".to_owned())?;
        if !confirmed {
            return Err("GitHub write denied in the OS approval dialog".to_owned());
        }
    }
    let response = client()?
        .post(format!("{BROKER}/api/desktop/proposals/{id}/{decision}"))
        .bearer_auth(token(&state)?)
        .send()
        .await
        .map_err(|_| "cannot reach local GitHub App".to_owned())?;
    let result: GitHubDecision = bounded_json(response).await?;
    Ok(serde_json::json!({
        "id": result.id, "status": result.status, "result": result.result,
    }))
}

#[tauri::command]
pub async fn get_github_checks(
    installation_id: u64,
    repo_id: u64,
    number: u64,
    state: State<'_, GitHubQueueState>,
) -> Result<GitHubChecks, String> {
    checked_repo(installation_id, repo_id)?;
    if number == 0 {
        return Err("invalid pull request number".into());
    }
    let response = client()?
        .get(format!(
            "{BROKER}/api/desktop/checks?installationId={installation_id}&repoId={repo_id}&number={number}"
        ))
        .bearer_auth(token(&state)?)
        .send()
        .await
        .map_err(|_| "cannot reach local GitHub App".to_owned())?;
    bounded_json(response).await
}

#[tauri::command]
pub async fn get_github_jobs(
    installation_id: u64,
    repo_id: u64,
    state: State<'_, GitHubQueueState>,
) -> Result<Vec<GitHubJob>, String> {
    checked_repo(installation_id, repo_id)?;
    let response = client()?
        .get(format!(
            "{BROKER}/api/desktop/jobs?installationId={installation_id}&repoId={repo_id}"
        ))
        .bearer_auth(token(&state)?)
        .send()
        .await
        .map_err(|_| "cannot reach local GitHub App".to_owned())?;
    let jobs: JobList = bounded_json(response).await?;
    if jobs.jobs.len() > 50 {
        return Err("too many GitHub jobs".into());
    }
    Ok(jobs.jobs)
}

#[tauri::command]
pub async fn start_github_job(
    installation_id: u64,
    repo_id: u64,
    number: u64,
    state: State<'_, GitHubQueueState>,
    app: AppHandle,
) -> Result<GitHubJob, String> {
    checked_repo(installation_id, repo_id)?;
    if number == 0 {
        return Err("invalid issue number".into());
    }
    // A model or compromised renderer cannot initiate cost-incurring remote
    // repository writes by invoking IPC: the OS owns the confirmation.
    let issue_response = client()?
        .get(format!(
            "{BROKER}/api/desktop/issue?installationId={installation_id}&repoId={repo_id}&number={number}"
        ))
        .bearer_auth(token(&state)?)
        .send()
        .await
        .map_err(|_| "cannot inspect the GitHub issue before approval".to_owned())?;
    let issue: VerifiedIssue = bounded_json(issue_response).await?;
    if issue.number != number {
        return Err("broker returned an unexpected issue".into());
    }
    let description = format!(
        "Run the Ryoko engineering agents for {} issue #{}: {}?\n\nThis may incur model costs and push an isolated branch plus a draft PR. It will NEVER merge.",
        issue.repository, issue.number, issue.title
    );
    let confirmed = tauri::async_runtime::spawn_blocking(move || {
        app.dialog()
            .message(description)
            .title("Ryoko — authorize agent run")
            .buttons(MessageDialogButtons::OkCancelCustom(
                "Run agents".to_owned(),
                "Deny".to_owned(),
            ))
            .blocking_show()
    })
    .await
    .map_err(|_| "could not display OS agent-run approval".to_owned())?;
    if !confirmed {
        return Err("agent run denied in the OS approval dialog".into());
    }
    let response = client()?
        .post(format!("{BROKER}/api/desktop/jobs"))
        .bearer_auth(token(&state)?)
        .json(&serde_json::json!({
            "installationId": installation_id, "repoId": repo_id, "number": number,
        }))
        .send()
        .await
        .map_err(|_| "cannot reach local GitHub App".to_owned())?;
    bounded_json(response).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bridge_server_is_fixed_to_localhost() {
        assert_eq!(BROKER, "http://127.0.0.1:4173");
    }
}
