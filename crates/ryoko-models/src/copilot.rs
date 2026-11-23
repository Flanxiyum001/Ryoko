//! GitHub Copilot chat-completions adapter.
//!
//! Copilot is OpenAI-compatible at the wire level but not at the auth level:
//! a long-lived GitHub token is exchanged for a short-lived Copilot bearer
//! (about 30 minutes) at `api.github.com/copilot_internal/v2/token`, and the
//! exchange also names the API host for the account's plan. This adapter owns
//! that exchange, caches the bearer and refreshes it shortly before expiry.
//!
//! Two token kinds are accepted:
//!
//! - A **fine-grained personal access token** with the account permission
//!   `Copilot requests` (`github_pat_...`). GitHub documents this as the way a
//!   non-editor client authenticates; it is sent to Copilot directly.
//! - A **GitHub OAuth/App user token** (`gho_...` / `ghu_...`), which is
//!   exchanged as described above.
//!
//! Known limitation, stated rather than hidden: the token-exchange endpoint and
//! the editor-identity headers are not part of a published GitHub API contract.
//! GitHub can change them. When the exchange is refused, the error names the
//! documented fine-grained-token alternative instead of guessing.
//!
//! No Copilot token is logged, persisted or placed in an error message.

use std::sync::Arc;

use async_trait::async_trait;
use serde_json::Value;
use tokio::sync::Mutex;

use crate::openai::{decode_sse, wire_body};
use crate::types::{
    ChatRequest, ModelError, ModelProvider, ModelStream, Pricing, ProviderCapabilities,
};

pub const TOKEN_URL: &str = "https://api.github.com/copilot_internal/v2/token";
pub const DEFAULT_API_BASE: &str = "https://api.githubcopilot.com";
/// Refresh this long before GitHub says the bearer expires.
pub const REFRESH_MARGIN_SECS: i64 = 60;
const MAX_MODELS: usize = 200;
const MAX_MODEL_ID: usize = 120;

/// Headers Copilot's gateway requires in addition to the OpenAI shape. These
/// are the values GitHub's own client sends; see the module documentation.
pub const EDITOR_VERSION: &str = "vscode/1.104.1";
pub const EDITOR_PLUGIN_VERSION: &str = "copilot-chat/0.26.7";
pub const INTEGRATION_ID: &str = "vscode-chat";

#[derive(Clone)]
pub struct CopilotConfig {
    pub id: String,
    /// The GitHub credential: a `github_pat_` fine-grained token, or an OAuth
    /// user token that will be exchanged for a Copilot bearer.
    pub github_token: String,
    /// Defaults to [`DEFAULT_API_BASE`] until the exchange reports the host for
    /// this account's plan.
    pub api_base: Option<String>,
}

impl std::fmt::Debug for CopilotConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CopilotConfig")
            .field("id", &self.id)
            .field("github_token", &"<redacted>")
            .field("api_base", &self.api_base)
            .finish()
    }
}

impl CopilotConfig {
    pub fn new(id: impl Into<String>, github_token: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            github_token: github_token.into(),
            api_base: None,
        }
    }
}

#[derive(Clone)]
struct CachedToken {
    token: String,
    expires_at: i64,
    api_base: String,
}

impl std::fmt::Debug for CachedToken {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CachedToken")
            .field("token", &"<redacted>")
            .field("expires_at", &self.expires_at)
            .field("api_base", &self.api_base)
            .finish()
    }
}

impl std::fmt::Debug for CopilotBearer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CopilotBearer")
            .field("token", &"<redacted>")
            .field("api_base", &self.api_base)
            .finish()
    }
}

struct CopilotBearer {
    token: String,
    api_base: String,
}

/// A Copilot bearer needs an exchange unless it is a fine-grained PAT, which
/// GitHub documents as usable directly.
pub fn is_fine_grained_pat(token: &str) -> bool {
    token.starts_with("github_pat_")
}

fn valid_api_base(value: &str) -> Option<String> {
    let url = reqwest::Url::parse(value).ok()?;
    if url.scheme() != "https"
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return None;
    }
    let host = url.host_str()?;
    if host.is_empty() || host.len() > 253 {
        return None;
    }
    Some(format!("https://{host}"))
}

/// `endpoints.api` is a bare host such as `api.githubcopilot.com`.
fn api_base_from_exchange(value: &Value) -> Option<String> {
    let host = value
        .get("endpoints")
        .and_then(|endpoints| endpoints.get("api"))
        .and_then(Value::as_str)?;
    let host = host.trim();
    if host.is_empty() || host.len() > 253 {
        return None;
    }
    if host.contains("://") {
        return valid_api_base(host);
    }
    valid_api_base(&format!("https://{host}"))
}

fn parse_token_exchange(value: &Value) -> Result<CachedToken, ModelError> {
    let token = value
        .get("token")
        .and_then(Value::as_str)
        .filter(|token| !token.is_empty() && token.len() <= 4096)
        .ok_or_else(|| ModelError::Decode("Copilot token exchange returned no token".to_owned()))?;
    let expires_at = value
        .get("expires_at")
        .and_then(Value::as_i64)
        .filter(|expires| *expires > 0)
        .ok_or_else(|| {
            ModelError::Decode("Copilot token exchange returned no expiry".to_owned())
        })?;
    let api_base = api_base_from_exchange(value).unwrap_or_else(|| DEFAULT_API_BASE.to_owned());
    Ok(CachedToken {
        token: token.to_owned(),
        expires_at,
        api_base,
    })
}

pub fn needs_refresh(expires_at: i64, now: i64) -> bool {
    now >= expires_at - REFRESH_MARGIN_SECS
}

/// Model ids the account may use. Anything outside a conservative identifier
/// set is dropped rather than shown in a picker.
pub fn parse_models(value: &Value) -> Vec<String> {
    let entries = value.get("data").and_then(Value::as_array);
    let mut models = Vec::new();
    for entry in entries.into_iter().flatten() {
        let Some(id) = entry.get("id").and_then(Value::as_str) else {
            continue;
        };
        if id.is_empty()
            || id.len() > MAX_MODEL_ID
            || !id
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-' | b'_'))
        {
            continue;
        }
        if !models.iter().any(|existing| existing == id) {
            models.push(id.to_owned());
        }
        if models.len() >= MAX_MODELS {
            break;
        }
    }
    models
}

pub struct CopilotProvider {
    config: CopilotConfig,
    client: reqwest::Client,
    cache: Arc<Mutex<Option<CachedToken>>>,
}

impl std::fmt::Debug for CopilotProvider {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CopilotProvider")
            .field("config", &self.config)
            .finish_non_exhaustive()
    }
}

impl CopilotProvider {
    pub fn new(config: CopilotConfig) -> Result<Self, ModelError> {
        if config.github_token.trim().is_empty() {
            return Err(ModelError::Configuration(
                "GitHub Copilot needs a GitHub token with the Copilot Requests permission"
                    .to_owned(),
            ));
        }
        if let Some(base) = &config.api_base {
            if valid_api_base(base).is_none() {
                return Err(ModelError::Configuration(
                    "Copilot API base must be an HTTPS GitHub host".to_owned(),
                ));
            }
        }
        let client = reqwest::Client::builder()
            .connect_timeout(std::time::Duration::from_secs(20))
            .timeout(std::time::Duration::from_secs(180))
            .redirect(reqwest::redirect::Policy::none())
            .user_agent(concat!("ryoko/", env!("CARGO_PKG_VERSION")))
            .build()?;
        Ok(Self {
            config,
            client,
            cache: Arc::new(Mutex::new(None)),
        })
    }

    fn provider(&self) -> &str {
        &self.config.id
    }

    async fn exchange(&self) -> Result<CachedToken, ModelError> {
        let response = self
            .client
            .get(TOKEN_URL)
            .header(
                "Authorization",
                format!("token {}", self.config.github_token),
            )
            .header("Accept", "application/json")
            .header("Editor-Version", EDITOR_VERSION)
            .header("Editor-Plugin-Version", EDITOR_PLUGIN_VERSION)
            .header("Copilot-Integration-Id", INTEGRATION_ID)
            .header("X-GitHub-Api-Version", "2022-11-28")
            .send()
            .await?;
        let status = response.status();
        if !status.is_success() {
            // The body can echo request details; keep the status only.
            return Err(ModelError::Configuration(format!(
                "GitHub refused this token for Copilot (HTTP {}). A fine-grained personal \
                 access token with the `Copilot requests` permission is the documented option.",
                status.as_u16()
            )));
        }
        let value: Value = response.json().await.map_err(|_| {
            ModelError::Decode("Copilot token exchange returned invalid JSON".to_owned())
        })?;
        parse_token_exchange(&value)
    }

    /// The bearer and API host to use, reusing a cached one until it is close
    /// to expiry.
    async fn bearer(&self) -> Result<CopilotBearer, ModelError> {
        if is_fine_grained_pat(&self.config.github_token) {
            let api_base = self
                .config
                .api_base
                .clone()
                .and_then(|base| valid_api_base(&base))
                .unwrap_or_else(|| DEFAULT_API_BASE.to_owned());
            return Ok(CopilotBearer {
                token: self.config.github_token.clone(),
                api_base,
            });
        }
        let now = unix_now();
        {
            let cache = self.cache.lock().await;
            if let Some(cached) = cache.as_ref() {
                if !needs_refresh(cached.expires_at, now) {
                    return Ok(CopilotBearer {
                        token: cached.token.clone(),
                        api_base: cached.api_base.clone(),
                    });
                }
            }
        }
        let exchanged = self.exchange().await?;
        let bearer = CopilotBearer {
            token: exchanged.token.clone(),
            api_base: exchanged.api_base.clone(),
        };
        *self.cache.lock().await = Some(exchanged);
        Ok(bearer)
    }

    fn request(&self, bearer: &CopilotBearer, path: &str) -> reqwest::RequestBuilder {
        self.client
            .post(format!("{}{path}", bearer.api_base))
            .header("Authorization", format!("Bearer {}", bearer.token))
            .header("Content-Type", "application/json")
            .header("Editor-Version", EDITOR_VERSION)
            .header("Editor-Plugin-Version", EDITOR_PLUGIN_VERSION)
            .header("Copilot-Integration-Id", INTEGRATION_ID)
            .header("X-GitHub-Api-Version", "2022-11-28")
            .header("Openai-Intent", "conversation-panel")
    }

    /// Models this account may use. An empty list means the account has no
    /// Copilot seat or the plan exposes no models to this client.
    pub async fn models(&self) -> Result<Vec<String>, ModelError> {
        let bearer = self.bearer().await?;
        let response = self
            .client
            .get(format!("{}/models", bearer.api_base))
            .header("Authorization", format!("Bearer {}", bearer.token))
            .header("Accept", "application/json")
            .header("Editor-Version", EDITOR_VERSION)
            .header("Editor-Plugin-Version", EDITOR_PLUGIN_VERSION)
            .header("Copilot-Integration-Id", INTEGRATION_ID)
            .header("X-GitHub-Api-Version", "2022-11-28")
            .send()
            .await?;
        if !response.status().is_success() {
            return Err(ModelError::Http {
                provider: self.provider().to_owned(),
                status: response.status().as_u16(),
                body: "GitHub Copilot model list was refused".to_owned(),
            });
        }
        let value: Value = response.json().await.map_err(|_| {
            ModelError::Decode("Copilot model list returned invalid JSON".to_owned())
        })?;
        Ok(parse_models(&value))
    }
}

#[async_trait]
impl ModelProvider for CopilotProvider {
    fn id(&self) -> &str {
        &self.config.id
    }

    fn capabilities(&self) -> ProviderCapabilities {
        ProviderCapabilities {
            supports_streaming: true,
            supports_tools: true,
            supports_system_prompt: true,
            max_context_tokens: 128_000,
        }
    }

    fn pricing(&self, _model: &str) -> Option<Pricing> {
        None
    }

    async fn stream(&self, request: ChatRequest) -> Result<ModelStream, ModelError> {
        let bearer = self.bearer().await?;
        let response = self
            .request(&bearer, "/chat/completions")
            .json(&wire_body(&request, true))
            .send()
            .await?;
        let status = response.status();
        if !status.is_success() {
            // A rejected bearer must not poison the cache for the next call.
            if status.as_u16() == 401 {
                *self.cache.lock().await = None;
            }
            return Err(ModelError::Http {
                provider: self.provider().to_owned(),
                status: status.as_u16(),
                body: "Copilot error details omitted to protect credentials".to_owned(),
            });
        }
        Ok(decode_sse(response, self.provider().to_owned()))
    }
}

fn unix_now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_secs() as i64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    use serde_json::json;

    #[test]
    fn the_exchange_response_supplies_the_token_expiry_and_host() {
        let value = json!({
            "token": "tid=abc123",
            "expires_at": 1_800_000_000_i64,
            "endpoints": {"api": "api.githubcopilot.com"}
        });
        let parsed = parse_token_exchange(&value).unwrap();
        assert_eq!(parsed.api_base, "https://api.githubcopilot.com");
        assert_eq!(parsed.expires_at, 1_800_000_000);
        assert!(!format!("{parsed:?}").contains("tid=abc123"));

        let enterprise = json!({
            "token": "tid=abc123",
            "expires_at": 1_800_000_000_i64,
            "endpoints": {"api": "https://api.business.githubcopilot.com"}
        });
        assert_eq!(
            parse_token_exchange(&enterprise).unwrap().api_base,
            "https://api.business.githubcopilot.com"
        );
    }

    #[test]
    fn hostile_exchange_values_are_refused() {
        for value in [
            json!({"expires_at": 1_800_000_000_i64, "endpoints": {"api": "api.githubcopilot.com"}}),
            json!({"token": "", "expires_at": 1_800_000_000_i64}),
            json!({"token": "tid=x"}),
            json!({"token": "tid=x", "expires_at": 0}),
        ] {
            assert!(parse_token_exchange(&value).is_err(), "{value}");
        }
        // A plaintext or off-site host is ignored in favour of the default.
        let hostile = json!({
            "token": "tid=x",
            "expires_at": 1_800_000_000_i64,
            "endpoints": {"api": "http://evil.example"}
        });
        assert_eq!(
            parse_token_exchange(&hostile).unwrap().api_base,
            DEFAULT_API_BASE
        );
    }

    #[test]
    fn refresh_happens_before_expiry() {
        assert!(!needs_refresh(1_000, 900));
        assert!(needs_refresh(1_000, 940));
        assert!(needs_refresh(1_000, 1_000));
    }

    #[test]
    fn model_ids_are_deduplicated_and_bounded() {
        let value = json!({"data": [
            {"id": "gpt-4o"},
            {"id": "gpt-4o"},
            {"id": "claude-sonnet-4"},
            {"id": "bad id"},
            {"id": "x".repeat(MAX_MODEL_ID + 1)},
            {"no_id": true}
        ]});
        assert_eq!(parse_models(&value), vec!["gpt-4o", "claude-sonnet-4"]);
        assert!(parse_models(&json!({})).is_empty());
    }

    #[test]
    fn fine_grained_tokens_skip_the_exchange() {
        assert!(is_fine_grained_pat("github_pat_11ABC"));
        assert!(!is_fine_grained_pat("gho_abc"));
        assert!(!is_fine_grained_pat("ghp_classic"));
    }

    #[tokio::test]
    async fn an_empty_token_is_refused_before_any_request() {
        let error = CopilotProvider::new(CopilotConfig::new("github-copilot", "  "))
            .expect_err("empty token");
        assert!(error.to_string().contains("Copilot Requests"));
    }

    #[test]
    fn diagnostics_never_render_the_github_token() {
        let provider = CopilotProvider::new(CopilotConfig::new(
            "github-copilot",
            "github_pat_super_secret",
        ))
        .unwrap();
        let rendered = format!("{provider:?}");
        assert!(!rendered.contains("github_pat_super_secret"));
    }
}
