//! Read access to the signed-in account and its repositories.
//!
//! Only the endpoints the IDE needs are implemented, every field is validated
//! before it reaches the frontend, and refuse-to-accept is the default: an
//! unknown or off-site URL is dropped rather than rendered.

use std::sync::Arc;

use serde::{Deserialize, Serialize};

use crate::transport::{HttpMethod, HttpRequest, Transport};
use crate::{GitHubError, Secret};

pub const API_ROOT: &str = "https://api.github.com";
const API_VERSION: &str = "2022-11-28";
const PER_PAGE: u32 = 100;
/// A hard ceiling so a pathological account cannot flood the picker.
pub const MAX_REPOSITORIES: usize = 500;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GitHubUser {
    pub login: String,
    pub name: String,
    pub avatar_url: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GitHubRepository {
    pub id: u64,
    pub full_name: String,
    pub name: String,
    pub owner: String,
    pub default_branch: String,
    pub private: bool,
    pub html_url: String,
    pub clone_url: String,
    pub description: String,
    pub updated_at: String,
}

#[derive(Deserialize)]
struct WireUser {
    login: String,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    avatar_url: Option<String>,
}

#[derive(Deserialize)]
struct WireRepository {
    id: u64,
    full_name: String,
    name: String,
    default_branch: String,
    #[serde(default)]
    private: bool,
    html_url: String,
    clone_url: String,
    #[serde(default)]
    description: Option<String>,
    #[serde(default)]
    updated_at: String,
    owner: WireOwner,
}

#[derive(Deserialize)]
struct WireOwner {
    login: String,
}

fn bounded(value: &str, limit: usize) -> bool {
    !value.is_empty() && value.len() <= limit && !value.contains(['\0', '\n', '\r'])
}

fn valid_name(value: &str) -> bool {
    bounded(value, 100)
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-' | b'_'))
}

/// `https://github.com/<owner>/<repo>` URLs only, with no query or fragment.
fn valid_github_url(value: &str, allow_clone_suffix: bool) -> Option<String> {
    let url = reqwest::Url::parse(value).ok()?;
    if url.scheme() != "https"
        || url.host_str() != Some("github.com")
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return None;
    }
    let path = url.path().trim_start_matches('/');
    let path = if allow_clone_suffix {
        path.strip_suffix(".git").unwrap_or(path)
    } else {
        path
    };
    let mut parts = path.split('/');
    let owner = parts.next()?;
    let repository = parts.next()?;
    if parts.next().is_some() || !valid_name(owner) || !valid_name(repository) {
        return None;
    }
    Some(format!("https://github.com/{owner}/{repository}"))
}

/// Avatars come from GitHub's asset host with a cache-busting query string.
fn valid_avatar_url(value: &str) -> Option<String> {
    let url = reqwest::Url::parse(value).ok()?;
    if url.scheme() != "https"
        || url.host_str() != Some("avatars.githubusercontent.com")
        || !url.username().is_empty()
        || url.password().is_some()
        || value.len() > 300
    {
        return None;
    }
    Some(value.to_owned())
}

fn repository_from_wire(value: WireRepository) -> Option<GitHubRepository> {
    let full_name = format!("{}/{}", value.owner.login, value.name);
    if !valid_name(&value.owner.login) || !valid_name(&value.name) {
        return None;
    }
    let html_url = valid_github_url(&value.html_url, false)?;
    let clone_url = valid_github_url(&value.clone_url, true)?;
    // `full_name`, the owner and the name must agree, or the row is not the
    // repository it claims to be.
    if value.full_name != full_name || html_url != format!("https://github.com/{full_name}") {
        return None;
    }
    if !bounded(&value.default_branch, 200) {
        return None;
    }
    let description = value
        .description
        .unwrap_or_default()
        .chars()
        .take(500)
        .collect::<String>();
    Some(GitHubRepository {
        id: value.id,
        full_name,
        name: value.name,
        owner: value.owner.login,
        default_branch: value.default_branch,
        private: value.private,
        html_url,
        clone_url,
        description,
        updated_at: value.updated_at.chars().take(64).collect(),
    })
}

pub struct GitHubClient {
    transport: Arc<dyn Transport>,
    token: Secret,
}

impl std::fmt::Debug for GitHubClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GitHubClient").finish_non_exhaustive()
    }
}

impl GitHubClient {
    pub fn new(transport: Arc<dyn Transport>, token: Secret) -> Self {
        Self { transport, token }
    }

    async fn get(
        &self,
        path: &str,
        context: &'static str,
    ) -> Result<crate::HttpReply, GitHubError> {
        if self.token.is_empty() {
            return Err(GitHubError::Unauthorized);
        }
        let reply = self
            .transport
            .send(HttpRequest {
                method: HttpMethod::Get,
                url: format!("{API_ROOT}{path}"),
                headers: vec![
                    (
                        "Accept".to_owned(),
                        "application/vnd.github+json".to_owned(),
                    ),
                    (
                        "Authorization".to_owned(),
                        format!("Bearer {}", self.token.expose()),
                    ),
                    ("X-GitHub-Api-Version".to_owned(), API_VERSION.to_owned()),
                ],
                body: None,
            })
            .await?;
        match reply.status {
            200..=299 => Ok(reply),
            401 => Err(GitHubError::Unauthorized),
            403 | 404 => Err(GitHubError::Forbidden),
            status => Err(GitHubError::Http { status, context }),
        }
    }

    pub async fn user(&self) -> Result<GitHubUser, GitHubError> {
        let reply = self.get("/user", "reading the signed-in account").await?;
        let wire: WireUser = reply.json()?;
        if !valid_name(&wire.login) {
            return Err(GitHubError::InvalidResponse("GitHub login was not usable"));
        }
        Ok(GitHubUser {
            login: wire.login,
            name: wire
                .name
                .unwrap_or_default()
                .chars()
                .take(200)
                .collect::<String>(),
            avatar_url: wire.avatar_url.as_deref().and_then(valid_avatar_url),
        })
    }

    /// One page of repositories the account owns or collaborates on.
    pub async fn repositories_page(&self, page: u32) -> Result<Vec<GitHubRepository>, GitHubError> {
        if page == 0 {
            return Err(GitHubError::Unsafe("repository page starts at 1"));
        }
        let reply = self
            .get(
                &format!(
                    "/user/repos?per_page={PER_PAGE}&page={page}&sort=updated&affiliation=owner,collaborator,organization_member"
                ),
                "listing repositories",
            )
            .await?;
        let wire: Vec<WireRepository> = reply.json()?;
        Ok(wire.into_iter().filter_map(repository_from_wire).collect())
    }

    /// Every page up to [`MAX_REPOSITORIES`], most recently updated first.
    pub async fn repositories(&self) -> Result<Vec<GitHubRepository>, GitHubError> {
        let mut all = Vec::new();
        let mut page = 1;
        loop {
            let batch = self.repositories_page(page).await?;
            let short_page = batch.len() < PER_PAGE as usize;
            all.extend(batch);
            if short_page || all.len() >= MAX_REPOSITORIES {
                all.truncate(MAX_REPOSITORIES);
                return Ok(all);
            }
            page += 1;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::transport::tests::{ScriptedTransport, json_reply};
    use serde_json::json;
    use std::sync::Arc;

    fn repo_json(full_name: &str) -> serde_json::Value {
        let (owner, name) = full_name.split_once('/').unwrap();
        json!({
            "id": 42,
            "full_name": full_name,
            "name": name,
            "owner": {"login": owner},
            "default_branch": "main",
            "private": true,
            "html_url": format!("https://github.com/{full_name}"),
            "clone_url": format!("https://github.com/{full_name}.git"),
            "description": "demo",
            "updated_at": "2026-11-23T00:00:00Z"
        })
    }

    #[test]
    fn repository_metadata_is_validated() {
        let parsed = repository_from_wire(
            serde_json::from_value::<WireRepository>(repo_json("acme/app")).unwrap(),
        )
        .expect("valid repository");
        assert_eq!(parsed.full_name, "acme/app");
        assert_eq!(parsed.clone_url, "https://github.com/acme/app");
        assert!(parsed.private);

        let mut hostile = repo_json("acme/app");
        hostile["html_url"] = json!("https://github.com.evil.example/acme/app");
        assert!(
            repository_from_wire(serde_json::from_value::<WireRepository>(hostile).unwrap())
                .is_none()
        );

        let mut hostile = repo_json("acme/app");
        hostile["name"] = json!("../escape");
        assert!(
            repository_from_wire(serde_json::from_value::<WireRepository>(hostile).unwrap())
                .is_none()
        );
    }

    #[tokio::test]
    async fn repositories_are_paged_and_bounded() {
        let page = (0..100)
            .map(|index| repo_json(&format!("acme/repo-{index}")))
            .collect::<Vec<_>>();
        let transport = Arc::new(ScriptedTransport::new(vec![
            json_reply(200, json!(page)),
            json_reply(200, json!([repo_json("acme/last")])),
        ]));
        let client = GitHubClient::new(transport.clone(), Secret::new("gho_test"));
        let repositories = client.repositories().await.unwrap();
        assert_eq!(repositories.len(), 101);
        assert_eq!(repositories.last().unwrap().name, "last");
        let requests = transport.requests();
        assert_eq!(requests.len(), 2);
        assert!(
            requests[1].url.ends_with(
                "page=2&sort=updated&affiliation=owner,collaborator,organization_member"
            )
        );
        assert!(
            requests[0]
                .headers
                .contains(&("Authorization".to_owned(), "Bearer gho_test".to_owned()))
        );
    }

    #[tokio::test]
    async fn unauthorized_and_forbidden_are_reported_without_the_body() {
        let transport = Arc::new(ScriptedTransport::new(vec![json_reply(
            401,
            json!({"message": "Bad credentials"}),
        )]));
        let client = GitHubClient::new(transport, Secret::new("gho_bad"));
        let error = client.user().await.unwrap_err();
        assert!(error.needs_sign_in());
        assert!(!error.to_string().contains("gho_bad"));

        let transport = Arc::new(ScriptedTransport::new(vec![json_reply(
            403,
            json!({"message": "Resource not accessible"}),
        )]));
        let client = GitHubClient::new(transport, Secret::new("gho_limited"));
        assert!(matches!(
            client.user().await.unwrap_err(),
            GitHubError::Forbidden
        ));
    }

    #[tokio::test]
    async fn an_empty_token_never_reaches_the_transport() {
        let transport = Arc::new(ScriptedTransport::new(vec![]));
        let client = GitHubClient::new(transport.clone(), Secret::new("  "));
        assert!(matches!(
            client.user().await,
            Err(GitHubError::Unauthorized)
        ));
        assert!(transport.requests().is_empty());
    }
}
