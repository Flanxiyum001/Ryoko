//! Read-only Tauri-native GitHub queue over the existing localhost App broker.
//! The OAuth token remains inside Node; a short-lived bridge token remains
//! inside this process and is never passed to Monaco or persisted on disk.
use std::sync::Mutex;
use std::time::Duration;

use reqwest::Client;
use serde::{Deserialize, Serialize};
use tauri::State;

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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bridge_server_is_fixed_to_localhost() {
        assert_eq!(BROKER, "http://127.0.0.1:4173");
    }
}
