//! OAuth **device flow** sign-in.
//!
//! Device flow is the correct grant for a desktop binary: there is no client
//! secret to ship, the user approves in their own browser, and the app never
//! opens a loopback listener. The client ID belongs to an OAuth App the
//! operator registers; this crate never hard-codes one and never impersonates
//! another application's client ID.
//!
//! The access token exists only in memory here. Callers persist it through
//! [`crate::CredentialStore`]; it is never part of a status or error message.

use std::sync::Arc;

use serde_json::Value;

use crate::transport::{HttpMethod, HttpRequest, Transport, form_encode};
use crate::{GitHubError, Secret};

pub const DEVICE_CODE_URL: &str = "https://github.com/login/device/code";
pub const ACCESS_TOKEN_URL: &str = "https://github.com/login/oauth/access_token";
/// Enough to read private repositories and push to a branch. No `workflow`
/// scope: the IDE must not be able to rewrite Actions pipelines.
pub const DEFAULT_SCOPES: &str = "read:user repo";
/// The variable that names the operator's OAuth App client ID.
pub const CLIENT_ID_ENV: &str = "RYOKO_GITHUB_CLIENT_ID";

const USER_AGENT: &str = concat!("ryoko/", env!("CARGO_PKG_VERSION"));

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeviceFlowStart {
    /// Opaque device code. Never logged or shown.
    pub device_code: Secret,
    /// Short code the user types at `verification_uri`.
    pub user_code: String,
    pub verification_uri: String,
    /// Seconds until GitHub expires the code.
    pub expires_in: u64,
    /// Minimum seconds between polls.
    pub interval: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PollOutcome {
    Pending,
    SlowDown {
        interval: u64,
    },
    Authorized {
        token: Secret,
        scopes: Vec<String>,
    },
    /// The user pressed "cancel" on GitHub's page.
    Denied,
    /// The device code outlived its window; start again.
    Expired,
}

#[derive(Clone)]
pub struct DeviceFlow {
    transport: Arc<dyn Transport>,
    client_id: String,
    scopes: String,
}

impl std::fmt::Debug for DeviceFlow {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DeviceFlow")
            .field("client_id", &self.client_id)
            .field("scopes", &self.scopes)
            .finish_non_exhaustive()
    }
}

/// A client ID is a short opaque token. Reject anything that could not be a
/// client ID instead of forwarding it into a request body.
pub fn validate_client_id(value: &str) -> Result<&str, GitHubError> {
    let trimmed = value.trim();
    if trimmed.is_empty() || trimmed.len() > 128 {
        return Err(GitHubError::NotConfigured("github_oauth_client_id"));
    }
    if !trimmed
        .bytes()
        .all(|byte| byte.is_ascii_alphanumeric() || byte == b'.' || byte == b'-' || byte == b'_')
    {
        return Err(GitHubError::NotConfigured("github_oauth_client_id"));
    }
    Ok(trimmed)
}

impl DeviceFlow {
    pub fn new(transport: Arc<dyn Transport>, client_id: &str) -> Result<Self, GitHubError> {
        Ok(Self {
            transport,
            client_id: validate_client_id(client_id)?.to_owned(),
            scopes: DEFAULT_SCOPES.to_owned(),
        })
    }

    /// Reads the operator's OAuth App client ID from the environment.
    pub fn from_environment(transport: Arc<dyn Transport>) -> Result<Self, GitHubError> {
        let client_id = std::env::var(CLIENT_ID_ENV).unwrap_or_default();
        Self::new(transport, &client_id)
    }

    pub fn with_scopes(mut self, scopes: &str) -> Self {
        self.scopes = scopes.to_owned();
        self
    }

    pub async fn start(&self) -> Result<DeviceFlowStart, GitHubError> {
        let body = form_encode(&[
            ("client_id".to_owned(), self.client_id.clone()),
            ("scope".to_owned(), self.scopes.clone()),
        ])?;
        let reply = self
            .transport
            .send(HttpRequest {
                method: HttpMethod::Post,
                url: DEVICE_CODE_URL.to_owned(),
                headers: vec![
                    ("Accept".to_owned(), "application/json".to_owned()),
                    (
                        "Content-Type".to_owned(),
                        "application/x-www-form-urlencoded".to_owned(),
                    ),
                    ("User-Agent".to_owned(), USER_AGENT.to_owned()),
                ],
                body: Some(body.into_bytes()),
            })
            .await?;
        if !reply.is_success() {
            return Err(GitHubError::Http {
                status: reply.status,
                context: "device sign-in could not start",
            });
        }
        let value: Value = reply.json()?;
        parse_device_start(&value)
    }

    pub async fn poll(&self, device_code: &Secret) -> Result<PollOutcome, GitHubError> {
        let body = form_encode(&[
            ("client_id".to_owned(), self.client_id.clone()),
            ("device_code".to_owned(), device_code.expose().to_owned()),
            (
                "grant_type".to_owned(),
                "urn:ietf:params:oauth:grant-type:device_code".to_owned(),
            ),
        ])?;
        let reply = self
            .transport
            .send(HttpRequest {
                method: HttpMethod::Post,
                url: ACCESS_TOKEN_URL.to_owned(),
                headers: vec![
                    ("Accept".to_owned(), "application/json".to_owned()),
                    (
                        "Content-Type".to_owned(),
                        "application/x-www-form-urlencoded".to_owned(),
                    ),
                    ("User-Agent".to_owned(), USER_AGENT.to_owned()),
                ],
                body: Some(body.into_bytes()),
            })
            .await?;
        // GitHub answers 200 for every device-flow outcome, including errors.
        if reply.status == 429 {
            return Ok(PollOutcome::SlowDown { interval: 10 });
        }
        if !reply.is_success() {
            return Err(GitHubError::Http {
                status: reply.status,
                context: "device sign-in poll failed",
            });
        }
        let value: Value = reply.json()?;
        parse_poll(&value)
    }
}

pub fn parse_device_start(value: &Value) -> Result<DeviceFlowStart, GitHubError> {
    let device_code = value
        .get("device_code")
        .and_then(Value::as_str)
        .filter(|code| !code.is_empty() && code.len() <= 256)
        .ok_or(GitHubError::InvalidResponse(
            "device sign-in returned no device code",
        ))?;
    let user_code = value
        .get("user_code")
        .and_then(Value::as_str)
        .filter(|code| !code.is_empty() && code.len() <= 32)
        .ok_or(GitHubError::InvalidResponse(
            "device sign-in returned no user code",
        ))?;
    let verification_uri = value
        .get("verification_uri")
        .and_then(Value::as_str)
        .filter(|uri| uri.starts_with("https://github.com/") && uri.len() <= 200)
        .ok_or(GitHubError::InvalidResponse(
            "device sign-in returned an unexpected verification URL",
        ))?;
    let expires_in = value
        .get("expires_in")
        .and_then(Value::as_u64)
        .filter(|seconds| (30..=3600).contains(seconds))
        .ok_or(GitHubError::InvalidResponse(
            "device sign-in returned an unexpected lifetime",
        ))?;
    let interval = value
        .get("interval")
        .and_then(Value::as_u64)
        .filter(|seconds| (1..=60).contains(seconds))
        .unwrap_or(5);
    Ok(DeviceFlowStart {
        device_code: Secret::new(device_code),
        user_code: user_code.to_owned(),
        verification_uri: verification_uri.to_owned(),
        expires_in,
        interval,
    })
}

pub fn parse_poll(value: &Value) -> Result<PollOutcome, GitHubError> {
    match value.get("error").and_then(Value::as_str) {
        Some("authorization_pending") => return Ok(PollOutcome::Pending),
        Some("slow_down") => return Ok(PollOutcome::SlowDown { interval: 10 }),
        Some("access_denied") => return Ok(PollOutcome::Denied),
        Some("expired_token") => return Ok(PollOutcome::Expired),
        Some(other) => {
            // `incorrect_client_credentials`, `device_flow_disabled`,
            // `unsupported_grant_type` and friends are configuration problems,
            // not a user waiting on a browser.
            return Err(GitHubError::Http {
                status: 400,
                context: if other.is_empty() {
                    "device sign-in was refused"
                } else {
                    "device sign-in is not allowed for this OAuth App"
                },
            });
        }
        None => {}
    }
    let token = value
        .get("access_token")
        .and_then(Value::as_str)
        .filter(|token| !token.is_empty() && token.len() <= 512)
        .ok_or(GitHubError::InvalidResponse(
            "device sign-in returned no access token",
        ))?;
    let token_type = value
        .get("token_type")
        .and_then(Value::as_str)
        .unwrap_or("bearer");
    if !token_type.eq_ignore_ascii_case("bearer") {
        return Err(GitHubError::InvalidResponse(
            "device sign-in returned an unexpected token type",
        ));
    }
    let scopes = value
        .get("scope")
        .and_then(Value::as_str)
        .map(|scope| {
            scope
                .split([' ', ','])
                .map(str::trim)
                .filter(|scope| !scope.is_empty())
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default();
    Ok(PollOutcome::Authorized {
        token: Secret::new(token),
        scopes,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::transport::tests::{ScriptedTransport, json_reply};
    use serde_json::json;

    fn start_value() -> Value {
        json!({
            "device_code": "device-code-1",
            "user_code": "ABCD-1234",
            "verification_uri": "https://github.com/login/device",
            "expires_in": 900,
            "interval": 5
        })
    }

    #[test]
    fn device_start_is_parsed_and_bounded() {
        let start = parse_device_start(&start_value()).expect("valid start");
        assert_eq!(start.user_code, "ABCD-1234");
        assert_eq!(start.interval, 5);
        assert_eq!(start.device_code.expose(), "device-code-1");
        assert!(!format!("{start:?}").contains("device-code-1"));

        let mut hostile = start_value();
        hostile["verification_uri"] = json!("https://evil.example/login/device");
        assert!(parse_device_start(&hostile).is_err());

        let mut hostile = start_value();
        hostile["expires_in"] = json!(86400);
        assert!(parse_device_start(&hostile).is_err());
    }

    #[test]
    fn poll_states_are_distinguished() {
        assert_eq!(
            parse_poll(&json!({"error": "authorization_pending"})).unwrap(),
            PollOutcome::Pending
        );
        assert_eq!(
            parse_poll(&json!({"error": "slow_down"})).unwrap(),
            PollOutcome::SlowDown { interval: 10 }
        );
        assert_eq!(
            parse_poll(&json!({"error": "access_denied"})).unwrap(),
            PollOutcome::Denied
        );
        assert_eq!(
            parse_poll(&json!({"error": "expired_token"})).unwrap(),
            PollOutcome::Expired
        );
        assert!(parse_poll(&json!({"error": "device_flow_disabled"})).is_err());
        assert!(parse_poll(&json!({"token_type": "bearer"})).is_err());
    }

    #[test]
    fn authorized_poll_never_exposes_the_token_in_debug() {
        let outcome = parse_poll(&json!({
            "access_token": "gho_secret_value",
            "token_type": "bearer",
            "scope": "repo,read:user"
        }))
        .unwrap();
        let PollOutcome::Authorized { token, scopes } = &outcome else {
            panic!("expected an authorized outcome");
        };
        assert_eq!(token.expose(), "gho_secret_value");
        assert_eq!(scopes, &["repo".to_owned(), "read:user".to_owned()]);
        assert!(!format!("{outcome:?}").contains("gho_secret_value"));
    }

    #[test]
    fn client_ids_are_validated() {
        assert!(validate_client_id("Iv1.0123456789abcdef").is_ok());
        assert!(validate_client_id("").is_err());
        assert!(validate_client_id("client id").is_err());
        assert!(validate_client_id("https://github.com/login").is_err());
    }

    #[tokio::test]
    async fn a_start_request_carries_no_secret_and_asks_for_json() {
        let transport = Arc::new(ScriptedTransport::new(vec![json_reply(200, start_value())]));
        let flow = DeviceFlow::new(transport.clone(), "Iv1.client").unwrap();
        let start = flow.start().await.unwrap();
        assert_eq!(start.user_code, "ABCD-1234");
        let requests = transport.requests();
        assert_eq!(requests.len(), 1);
        assert_eq!(requests[0].url, DEVICE_CODE_URL);
        assert_eq!(
            String::from_utf8(requests[0].body.clone().unwrap()).unwrap(),
            "client_id=Iv1.client&scope=read%3Auser+repo"
        );
    }

    #[tokio::test]
    async fn an_unknown_client_is_refused_without_a_request() {
        let transport = Arc::new(ScriptedTransport::new(vec![json_reply(
            200,
            json!({"error": "incorrect_client_credentials"}),
        )]));
        let flow = DeviceFlow::new(transport.clone(), "Iv1.client").unwrap();
        assert!(flow.poll(&Secret::new("device-code")).await.is_err());
        assert_eq!(transport.requests().len(), 1);
    }
}
