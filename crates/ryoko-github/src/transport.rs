//! Small injectable HTTP seam.
//!
//! The whole crate talks to GitHub through [`Transport`]. Production uses
//! [`ReqwestTransport`] (no redirects, no proxies configured by this crate, a
//! hard response-size bound). Tests use a scripted transport, so no unit test
//! contacts GitHub, needs a credential or depends on the network.

use async_trait::async_trait;
use serde::de::DeserializeOwned;

use crate::GitHubError;

/// No GitHub response this crate consumes is larger than this. A bigger body is
/// refused instead of buffered.
pub const MAX_RESPONSE_BYTES: usize = 2 * 1024 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HttpMethod {
    Get,
    Post,
}

#[derive(Debug, Clone)]
pub struct HttpRequest {
    pub method: HttpMethod,
    pub url: String,
    pub headers: Vec<(String, String)>,
    pub body: Option<Vec<u8>>,
}

#[derive(Debug, Clone)]
pub struct HttpReply {
    pub status: u16,
    pub body: Vec<u8>,
}

impl HttpReply {
    pub fn json<T: DeserializeOwned>(&self) -> Result<T, GitHubError> {
        serde_json::from_slice(&self.body)
            .map_err(|_| GitHubError::InvalidResponse("GitHub returned invalid JSON"))
    }

    pub fn is_success(&self) -> bool {
        (200..300).contains(&self.status)
    }
}

#[async_trait]
pub trait Transport: Send + Sync {
    async fn send(&self, request: HttpRequest) -> Result<HttpReply, GitHubError>;
}

/// The only transport that reaches the network.
#[derive(Clone)]
pub struct ReqwestTransport {
    client: reqwest::Client,
}

impl ReqwestTransport {
    pub fn new() -> Result<Self, GitHubError> {
        let client = reqwest::Client::builder()
            .connect_timeout(std::time::Duration::from_secs(15))
            .timeout(std::time::Duration::from_secs(60))
            // A redirect must never move a bearer token to another destination.
            .redirect(reqwest::redirect::Policy::none())
            .user_agent(concat!("ryoko/", env!("CARGO_PKG_VERSION")))
            .build()
            .map_err(|_| GitHubError::Transport("could not build the GitHub HTTP client"))?;
        Ok(Self { client })
    }
}

#[async_trait]
impl Transport for ReqwestTransport {
    async fn send(&self, request: HttpRequest) -> Result<HttpReply, GitHubError> {
        let method = match request.method {
            HttpMethod::Get => reqwest::Method::GET,
            HttpMethod::Post => reqwest::Method::POST,
        };
        let mut builder = self.client.request(method, &request.url);
        for (name, value) in &request.headers {
            builder = builder.header(name, value);
        }
        if let Some(body) = request.body {
            builder = builder.body(body);
        }
        let mut response = builder
            .send()
            .await
            .map_err(|_| GitHubError::Transport("GitHub request failed"))?;
        let status = response.status().as_u16();
        let declared = response.content_length().unwrap_or(0);
        if declared > MAX_RESPONSE_BYTES as u64 {
            return Err(GitHubError::InvalidResponse(
                "GitHub response exceeds the size limit",
            ));
        }
        let mut bytes = Vec::new();
        while let Some(chunk) = response
            .chunk()
            .await
            .map_err(|_| GitHubError::Transport("GitHub response could not be read"))?
        {
            if bytes.len().saturating_add(chunk.len()) > MAX_RESPONSE_BYTES {
                return Err(GitHubError::InvalidResponse(
                    "GitHub response exceeds the size limit",
                ));
            }
            bytes.extend_from_slice(&chunk);
        }
        Ok(HttpReply {
            status,
            body: bytes,
        })
    }
}

/// Percent-encodes a form field. Used for the OAuth device-flow bodies, whose
/// values are all plain ASCII; anything else is refused before encoding.
pub fn form_encode(pairs: &[(String, String)]) -> Result<String, GitHubError> {
    fn encode(value: &str) -> Result<String, GitHubError> {
        let mut out = String::with_capacity(value.len());
        for byte in value.bytes() {
            match byte {
                b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                    out.push(byte as char)
                }
                b' ' => out.push('+'),
                b':' | b'/' | b'+' => {
                    out.push('%');
                    out.push_str(&format!("{byte:02X}"));
                }
                _ => {
                    return Err(GitHubError::Unsafe(
                        "form value contains a character this client does not send",
                    ));
                }
            }
        }
        Ok(out)
    }
    let mut encoded = Vec::with_capacity(pairs.len());
    for (key, value) in pairs {
        encoded.push(format!("{}={}", encode(key)?, encode(value)?));
    }
    Ok(encoded.join("&"))
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use std::sync::Mutex;

    /// Replays prepared replies and records the requests it received.
    pub struct ScriptedTransport {
        replies: Mutex<Vec<HttpReply>>,
        requests: Mutex<Vec<HttpRequest>>,
    }

    impl ScriptedTransport {
        pub fn new(replies: Vec<HttpReply>) -> Self {
            Self {
                replies: Mutex::new(replies),
                requests: Mutex::new(Vec::new()),
            }
        }

        pub fn requests(&self) -> Vec<HttpRequest> {
            self.requests.lock().unwrap().clone()
        }
    }

    #[async_trait]
    impl Transport for ScriptedTransport {
        async fn send(&self, request: HttpRequest) -> Result<HttpReply, GitHubError> {
            self.requests.lock().unwrap().push(request);
            let mut replies = self.replies.lock().unwrap();
            if replies.is_empty() {
                return Err(GitHubError::Transport("scripted transport ran out"));
            }
            Ok(replies.remove(0))
        }
    }

    pub fn json_reply(status: u16, value: serde_json::Value) -> HttpReply {
        HttpReply {
            status,
            body: serde_json::to_vec(&value).unwrap(),
        }
    }

    #[test]
    fn form_encoding_keeps_oauth_values_intact() {
        assert_eq!(
            form_encode(&[("scope".into(), "read:user repo".into())]).unwrap(),
            "scope=read%3Auser+repo"
        );
        assert!(form_encode(&[("a".into(), "line\nbreak".into())]).is_err());
    }

    #[tokio::test]
    async fn oversize_responses_are_refused() {
        let reply = HttpReply {
            status: 200,
            body: vec![b'x'; MAX_RESPONSE_BYTES + 1],
        };
        // Structured JSON decoding fails on an over-size body before parsing.
        assert!(reply.json::<serde_json::Value>().is_err());
    }
}
