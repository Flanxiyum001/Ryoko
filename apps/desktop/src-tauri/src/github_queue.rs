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
const MAX_ERROR_BYTES: usize = 4 * 1024;
/// Read-only views the native desktop may request from the broker. Writes
/// never enter this list; they go through the proposal flow below.
const DESKTOP_VIEWS: [&str; 5] = ["branches", "checks", "tree", "file", "pr_diff"];
const PROPOSAL_OPERATIONS: [&str; 4] = [
    "create_branch",
    "commit_file",
    "open_pull_request",
    "review_comment",
];

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

fn clip(text: &str, max: usize) -> String {
    if text.len() <= max {
        return text.to_owned();
    }
    let mut end = max;
    while end > 0 && !text.is_char_boundary(end) {
        end -= 1;
    }
    text[..end].to_owned()
}

fn collapse(value: &serde_json::Value, max: usize) -> String {
    let text = match value {
        serde_json::Value::String(value) => value.clone(),
        other => other.to_string(),
    };
    let collapsed = text.split_whitespace().collect::<Vec<_>>().join(" ");
    clip(&collapsed, max)
}

/// Bounded, human-readable restatement of exactly what an approved proposal
/// will do on GitHub. The dialog shows this text verbatim; nothing else.
fn proposal_scope_summary(operation: &str, repo: &str, params: &serde_json::Value) -> Vec<String> {
    let line = |label: &str, key: &str| -> String {
        format!(
            "{label}: {}",
            collapse(params.get(key).unwrap_or(&serde_json::Value::Null), 160)
        )
    };
    let mut lines = vec![format!("Repository: {repo}")];
    match operation {
        "create_branch" => {
            lines.push(line("Create Git branch", "branch"));
            lines.push("Based on the repository default branch; no code is modified.".into());
        }
        "commit_file" => {
            lines.push(line("Commit file", "path"));
            lines.push(line("On branch", "branch"));
            lines.push(line("Commit message", "message"));
            let content = params
                .get("content")
                .and_then(|value| value.as_str())
                .unwrap_or("");
            lines.push(format!("Content: {} bytes of UTF-8 text", content.len()));
            if let Some(sha) = params.get("expectedSha").and_then(|value| value.as_str()) {
                lines.push(format!("Only replaces blob {}", clip(sha, 40)));
            }
        }
        "open_pull_request" => {
            lines.push(line("Pull request head branch", "head"));
            lines
                .push("Target: the repository default branch; nothing merges automatically".into());
            lines.push(line("Title", "title"));
            lines.push(format!(
                "Body: {} characters",
                params
                    .get("body")
                    .and_then(|value| value.as_str())
                    .unwrap_or("")
                    .len()
            ));
        }
        "review_comment" => {
            lines.push(format!(
                "Comment on pull request #{}",
                params
                    .get("number")
                    .and_then(|value| value.as_u64())
                    .unwrap_or(0)
            ));
            lines.push(line("Comment text", "comment"));
        }
        _ => lines.push("Unsupported operation; nothing will be executed".into()),
    }
    lines.push(
        "One-time execution: no auto-merge, default-branch push, workflow or admin change.".into(),
    );
    lines
}

/// Maps a broker HTTP status to a bounded, comprehensible failure message.
/// CI or network state is never fabricated: unknown states stay visible.
fn broker_failure(status: u16, detail: &str) -> String {
    match status {
        401 => "desktop GitHub session expired or revoked; pair the workbench again".into(),
        403 => format!("GitHub App is not permitted for this user or repository: {detail}"),
        404 => format!("not found on the GitHub App broker: {detail}"),
        409 => format!("the queued request conflicts with its GitHub state: {detail}"),
        413 => format!("resource exceeds the desktop review limit: {detail}"),
        429 => format!("the local GitHub App broker is rate limiting: {detail}"),
        other => format!("GitHub broker returned HTTP {other}: {detail}"),
    }
}

async fn bounded_text(response: reqwest::Response, limit: usize) -> String {
    let mut text = String::new();
    let mut cursor = response;
    while let Ok(Some(chunk)) = cursor.chunk().await {
        if text.len().saturating_add(chunk.len()) > limit {
            break;
        }
        text.push_str(&String::from_utf8_lossy(&chunk));
    }
    let line = text.lines().next().unwrap_or("");
    let parsed: serde_json::Value = serde_json::from_str(line).unwrap_or(serde_json::Value::Null);
    let detail = parsed
        .get("error")
        .map(|value| collapse(value, 160))
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| "no detail supplied".into());
    detail
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
        let status = response.status().as_u16();
        let detail = bounded_text(response, MAX_ERROR_BYTES).await;
        return Err(broker_failure(status, &detail));
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
pub struct GitHubViewData {
    view: String,
    data: serde_json::Value,
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GitHubProposal {
    id: String,
    source: String,
    status: String,
    operation: String,
    installation_id: u64,
    repo_id: u64,
    params: serde_json::Value,
    created_at: Option<String>,
    expires_in_seconds: u64,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GitHubDecision {
    outcome: String,
    detail: String,
}

#[derive(Deserialize)]
struct ProposalList {
    proposals: Vec<GithubProposalItem>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct GithubProposalItem {
    id: String,
    #[serde(default)]
    source: String,
    #[serde(default)]
    status: String,
    operation: String,
    installation_id: u64,
    repo_id: u64,
    #[serde(default)]
    params: serde_json::Value,
    #[serde(rename = "createdAt", default)]
    created_at: Option<String>,
    #[serde(rename = "expiresInSeconds", default)]
    expires_in_seconds: u64,
}

#[derive(Deserialize)]
struct DecisionReply {
    status: String,
    #[serde(default)]
    result: Option<serde_json::Value>,
}

fn repo_name(repositories: &[GitHubRepository], installation_id: u64, repo_id: u64) -> String {
    repositories
        .iter()
        .find(|repo| repo.installation_id == installation_id && repo.repo_id == repo_id)
        .map(|repo| repo.name.clone())
        .unwrap_or_else(|| format!("repository #{repo_id} (installation #{installation_id})"))
}

#[tauri::command]
pub async fn get_github_view(
    installation_id: u64,
    repo_id: u64,
    view: String,
    path: Option<String>,
    ref_name: Option<String>,
    number: Option<u64>,
    state: State<'_, GitHubQueueState>,
) -> Result<GitHubViewData, String> {
    if !DESKTOP_VIEWS.contains(&view.as_str()) {
        return Err("unsupported desktop repository view".into());
    }
    if installation_id == 0 || repo_id == 0 {
        return Err("choose an authorized GitHub repository".into());
    }
    if view == "pr_diff" && number.unwrap_or(0) == 0 {
        return Err("choose a pull request to load its file changes".into());
    }
    let mut url = format!(
        "{BROKER}/api/desktop/view?installationId={installation_id}&repoId={repo_id}&view={view}"
    );
    if let Some(path) = path.as_deref()
        && !path.is_empty()
    {
        url.push_str("&path=");
        url.push_str(&percent_encode_path(path)?);
    }
    if let Some(reference) = ref_name.as_deref()
        && !reference.is_empty()
    {
        url.push_str("&ref=");
        url.push_str(&percent_encode_path(reference)?);
    }
    if let Some(number) = number
        && number > 0
    {
        url.push_str("&number=");
        url.push_str(&number.to_string());
    }
    let response = client()?
        .get(url)
        .bearer_auth(token(&state)?)
        .send()
        .await
        .map_err(|_| "cannot reach local GitHub App".to_owned())?;
    bounded_json(response).await
}

/// Percent-encodes a path or ref for a broker query string component.
fn percent_encode_path(value: &str) -> Result<String, String> {
    if value.len() > 250 {
        return Err("path or ref exceeds its length limit".into());
    }
    let mut encoded = String::new();
    for byte in value.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                encoded.push(byte as char);
            }
            _ => encoded.push_str(&format!("%{byte:02X}")),
        }
    }
    Ok(encoded)
}

#[tauri::command]
pub async fn list_github_proposals(
    state: State<'_, GitHubQueueState>,
) -> Result<Vec<GitHubProposal>, String> {
    let response = client()?
        .get(format!("{BROKER}/api/desktop/proposals"))
        .bearer_auth(token(&state)?)
        .send()
        .await
        .map_err(|_| "cannot reach local GitHub App".to_owned())?;
    let data: ProposalList = bounded_json(response).await?;
    if data.proposals.len() > 20 {
        return Err("GitHub proposal list exceeded its limit".into());
    }
    Ok(data
        .proposals
        .into_iter()
        .map(|item| GitHubProposal {
            id: item.id,
            source: item.source,
            status: item.status,
            operation: item.operation,
            installation_id: item.installation_id,
            repo_id: item.repo_id,
            params: item.params,
            created_at: item.created_at,
            expires_in_seconds: item.expires_in_seconds,
        })
        .collect())
}

/// Queues a write proposal on the broker. Nothing happens on GitHub yet;
/// the proposal must still pass the explicit native approval below.
#[tauri::command]
pub async fn queue_github_proposal(
    installation_id: u64,
    repo_id: u64,
    operation: String,
    params: serde_json::Value,
    state: State<'_, GitHubQueueState>,
) -> Result<GitHubProposal, String> {
    if !PROPOSAL_OPERATIONS.contains(&operation.as_str()) {
        return Err("unsupported desktop GitHub operation".into());
    }
    if installation_id == 0 || repo_id == 0 {
        return Err("choose an authorized GitHub repository".into());
    }
    if params.is_null() || !params.is_object() {
        return Err("invalid operation parameters".into());
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
    let item: GithubProposalItem = bounded_json(response).await?;
    Ok(GitHubProposal {
        id: item.id,
        source: item.source,
        status: item.status,
        operation: item.operation,
        installation_id: item.installation_id,
        repo_id: item.repo_id,
        params: item.params,
        created_at: item.created_at,
        expires_in_seconds: item.expires_in_seconds,
    })
}

/// Rejects or, after an explicit native approval dialog that restates the
/// exact scope, approves a pending proposal exactly once. Repo identifier
/// is re-fetched from the broker so the dialog scope is authoritative.
#[tauri::command]
pub async fn decide_github_proposal(
    id: String,
    approve: bool,
    installation_id: u64,
    repo_id: u64,
    state: State<'_, GitHubQueueState>,
    app: AppHandle,
) -> Result<GitHubDecision, String> {
    if !id.chars().all(|c| c.is_ascii_hexdigit() || c == '-') || id.len() != 36 {
        return Err("invalid proposal identifier".into());
    }
    let paired = token(&state)?;
    if !approve {
        let response = client()?
            .post(format!("{BROKER}/api/desktop/proposals/{id}/reject"))
            .bearer_auth(&paired)
            .send()
            .await
            .map_err(|_| "cannot reach local GitHub App".to_owned())?;
        if !response.status().is_success() {
            let status = response.status().as_u16();
            let detail = bounded_text(response, MAX_ERROR_BYTES).await;
            return Err(broker_failure(status, &detail));
        }
        let reply: DecisionReply = bounded_json(response).await?;
        return Ok(GitHubDecision {
            outcome: reply.status,
            detail: "proposal rejected; nothing was sent to GitHub".into(),
        });
    }
    let detail = client()?
        .get(format!("{BROKER}/api/desktop/proposals/{id}"))
        .bearer_auth(&paired)
        .send()
        .await
        .map_err(|_| "cannot reach local GitHub App".to_owned())?;
    if !detail.status().is_success() {
        let status = detail.status().as_u16();
        let text = bounded_text(detail, MAX_ERROR_BYTES).await;
        return Err(broker_failure(status, &text));
    }
    let item: GithubProposalItem = bounded_json(detail).await?;
    if item.status != "pending" {
        return Err(format!(
            "proposal is no longer pending (status: {}); refresh the approval queue",
            clip(&item.status, 30)
        ));
    }

    let repositories = client()?
        .get(format!("{BROKER}/api/desktop/repositories"))
        .bearer_auth(&paired)
        .send()
        .await
        .map_err(|_| "cannot reach local GitHub App".to_owned())?;
    let repositories: Repositories = bounded_json(repositories).await?;
    let repository = repo_name(
        &repositories.repositories,
        item.installation_id,
        item.repo_id,
    );
    let scope = proposal_scope_summary(&item.operation, &repository, &item.params);
    let description = format!(
        "Approve this GitHub write?\n\n{}\n\nThis runs once. Nothing else is changed.",
        scope.join("\n")
    );
    let approved = tauri::async_runtime::spawn_blocking({
        move || {
            app.dialog()
                .message(description)
                .title("Ryoko — approve GitHub write")
                .buttons(MessageDialogButtons::OkCancelCustom(
                    "Approve once".into(),
                    "Cancel".into(),
                ))
                .blocking_show()
        }
    })
    .await
    .map_err(|_| "could not display the GitHub approval dialog".to_owned())?;
    if !approved {
        return Err("GitHub write cancelled; the proposal stays pending until it expires".into());
    }

    let response = client()?
        .post(format!("{BROKER}/api/desktop/proposals/{id}/approve"))
        .bearer_auth(&paired)
        .send()
        .await
        .map_err(|_| "cannot reach local GitHub App".to_owned())?;
    if !response.status().is_success() {
        let status = response.status().as_u16();
        let text = bounded_text(response, MAX_ERROR_BYTES).await;
        return Err(broker_failure(status, &text));
    }
    let reply: DecisionReply = bounded_json(response).await?;
    Ok(GitHubDecision {
        outcome: reply.status,
        detail: reply
            .result
            .map(|value| collapse(&value, 400))
            .unwrap_or_else(|| "completed without a result payload".into()),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bridge_server_is_fixed_to_localhost() {
        assert_eq!(BROKER, "http://127.0.0.1:4173");
    }

    #[test]
    fn desktop_views_and_operations_stay_read_only_or_broker_scoped() {
        assert_eq!(
            DESKTOP_VIEWS,
            ["branches", "checks", "tree", "file", "pr_diff"]
        );
        assert!(DESKTOP_VIEWS.iter().all(|view| *view != "merges"));
        assert_eq!(
            PROPOSAL_OPERATIONS,
            [
                "create_branch",
                "commit_file",
                "open_pull_request",
                "review_comment"
            ]
        );
    }

    #[test]
    fn commit_file_scope_states_the_exact_path_branch_and_message() {
        let params = serde_json::json!({
            "branch": "ryoko/fix-1",
            "path": "src/app.rs",
            "message": "fix: trailing newline",
            "content": "abc",
            "expectedSha": "12345678901234567890123456789012345678901234567890123",
        });
        let summary = proposal_scope_summary("commit_file", "approved-owner/repo", &params);
        assert!(
            summary
                .iter()
                .any(|line| line.contains("approved-owner/repo"))
        );
        assert!(summary.iter().any(|line| line.contains("src/app.rs")));
        assert!(summary.iter().any(|line| line.contains("ryoko/fix-1")));
        assert!(
            summary
                .iter()
                .any(|line| line.contains("fix: trailing newline"))
        );
        assert!(summary.iter().any(|line| line.contains("3 bytes")));
        assert!(
            summary
                .iter()
                .any(|line| line.contains("Only replaces blob 1234"))
        );
        assert!(
            summary
                .iter()
                .any(|line| line.contains("no auto-merge, default-branch push"))
        );
    }

    #[test]
    fn collapse_bounds_hostile_proposal_values() {
        let hostile = serde_json::json!(format!("{}\n\nEvil line", "a".repeat(400)));
        let collapsed = collapse(&hostile, 160);
        assert!(collapsed.len() <= 160);
        assert!(!collapsed.contains('\n'));
        let branch = serde_json::json!("ryoko/fix");
        assert_eq!(collapse(&branch, 160), "ryoko/fix");
    }

    #[test]
    fn broker_failures_are_bounded_and_never_claim_success() {
        assert!(broker_failure(401, "x").contains("expired or revoked"));
        assert!(broker_failure(403, "x").contains("not permitted"));
        assert!(
            broker_failure(409, "proposal expired or already resolved")
                .contains("proposal expired")
        );
        let offline = broker_failure(502, "GitHub request failed (502)");
        assert!(offline.contains("502"));
        assert!(!offline.to_lowercase().contains("success"));
        assert!(!broker_failure(401, "sensitive detail").contains("sensitive detail"));
        assert!(broker_failure(403, "scoped detail").contains("scoped detail"));
    }

    #[test]
    fn paths_are_percent_encoded_for_the_broker_query() {
        assert_eq!(percent_encode_path("src/app.rs").unwrap(), "src%2Fapp.rs");
        assert_eq!(
            percent_encode_path("feature/one-2.three").unwrap(),
            "feature%2Fone-2.three"
        );
        assert_eq!(percent_encode_path("a b").unwrap(), "a%20b");
        assert!(percent_encode_path(&"x".repeat(251)).is_err());
    }
}
