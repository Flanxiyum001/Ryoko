//! Low-latency fill-in-the-middle completions for editor ghost text.
//!
//! This is intentionally narrower than the chat router: it only talks to a
//! loopback model runtime (Ollama by default), sends no conversation history,
//! and returns bounded streamed text. Provider-specific wire handling stays in
//! this module; callers only see chunks of candidate text.

use std::pin::Pin;

use futures::{Stream, StreamExt};
use serde_json::{Value, json};

use crate::types::ModelError;

const MAX_PREFIX_BYTES: usize = 16_000;
const MAX_SUFFIX_BYTES: usize = 8_000;
const MAX_COMPLETION_BYTES: usize = 8_000;

/// Connection settings for a local FIM model.
#[derive(Debug, Clone)]
pub struct FimConfig {
    /// Loopback origin such as `http://127.0.0.1:11434`.
    pub base_url: String,
    pub model: String,
    pub max_tokens: u32,
}

impl FimConfig {
    pub fn new(base_url: impl Into<String>, model: impl Into<String>) -> Self {
        Self {
            base_url: base_url.into(),
            model: model.into(),
            max_tokens: 96,
        }
    }

    pub fn with_max_tokens(mut self, max_tokens: u32) -> Self {
        self.max_tokens = max_tokens.clamp(1, 512);
        self
    }
}

/// Streaming FIM client using Ollama's generate endpoint with a suffix.
pub struct FimClient {
    config: FimConfig,
    client: reqwest::Client,
}

impl FimClient {
    pub fn new(config: FimConfig) -> Result<Self, ModelError> {
        let url = reqwest::Url::parse(config.base_url.trim())
            .map_err(|error| ModelError::Configuration(format!("invalid FIM base URL: {error}")))?;
        if !is_loopback_origin(&url) {
            return Err(ModelError::Configuration(
                "FIM base URL must be a loopback Ollama or compatible endpoint".to_owned(),
            ));
        }
        if config.model.trim().is_empty() || config.model.len() > 200 {
            return Err(ModelError::Configuration(
                "FIM model must be a non-empty model name".to_owned(),
            ));
        }
        let client = reqwest::Client::builder()
            .connect_timeout(std::time::Duration::from_millis(800))
            .timeout(std::time::Duration::from_secs(8))
            .redirect(reqwest::redirect::Policy::none())
            .user_agent(concat!("ryoko-fim/", env!("CARGO_PKG_VERSION")))
            .build()
            .map_err(|error| ModelError::Configuration(error.to_string()))?;
        Ok(Self { config, client })
    }

    /// Streams candidate text chunks. The caller is responsible for discarding
    /// stale generations when a newer cursor request starts.
    pub async fn stream(
        &self,
        prefix: &str,
        suffix: &str,
    ) -> Result<Pin<Box<dyn Stream<Item = Result<String, ModelError>> + Send>>, ModelError> {
        if prefix.len() > MAX_PREFIX_BYTES || suffix.len() > MAX_SUFFIX_BYTES {
            return Err(ModelError::Configuration(
                "FIM context exceeds the editor limit".to_owned(),
            ));
        }
        let endpoint = format!(
            "{}/api/generate",
            self.config.base_url.trim_end_matches('/')
        );
        let body = json!({
            "model": self.config.model,
            "prompt": prefix,
            "suffix": suffix,
            "stream": true,
            "options": {
                "temperature": 0.1,
                "num_predict": self.config.max_tokens,
                "stop": ["\n\n"]
            }
        });
        let response = self
            .client
            .post(endpoint)
            .json(&body)
            .send()
            .await
            .map_err(ModelError::Transport)?;
        if !response.status().is_success() {
            let status = response.status().as_u16();
            let body = response.text().await.unwrap_or_default();
            return Err(ModelError::Http {
                provider: "fim".to_owned(),
                status,
                body: bound(&body, 2_000),
            });
        }

        let bytes = response.bytes_stream();
        Ok(Box::pin(async_stream::try_stream! {
            tokio::pin!(bytes);
            let mut buffer = Vec::new();
            let mut emitted = 0usize;
            while let Some(chunk) = bytes.next().await {
                let chunk = chunk.map_err(ModelError::Transport)?;
                buffer.extend_from_slice(&chunk);
                while let Some(newline) = buffer.iter().position(|byte| *byte == b'\n') {
                    let line = buffer.drain(..=newline).collect::<Vec<_>>();
                    if let Some(text) = parse_stream_line(&line)? {
                        if emitted.saturating_add(text.len()) > MAX_COMPLETION_BYTES {
                            return;
                        }
                        emitted += text.len();
                        yield text;
                    }
                }
            }
            if !buffer.is_empty() {
                if let Some(text) = parse_stream_line(&buffer)? {
                    if emitted.saturating_add(text.len()) <= MAX_COMPLETION_BYTES {
                        yield text;
                    }
                }
            }
        }))
    }
}

fn is_loopback_origin(url: &reqwest::Url) -> bool {
    if !matches!(url.scheme(), "http" | "https") {
        return false;
    }
    match url.host_str() {
        Some(name) if name.eq_ignore_ascii_case("localhost") => true,
        Some(name) => name
            .trim_matches(['[', ']'])
            .parse::<std::net::IpAddr>()
            .is_ok_and(|address| address.is_loopback()),
        None => false,
    }
}

fn parse_stream_line(line: &[u8]) -> Result<Option<String>, ModelError> {
    let line = String::from_utf8_lossy(line);
    let line = line.trim();
    if line.is_empty() {
        return Ok(None);
    }
    let value: Value = serde_json::from_str(line)
        .map_err(|error| ModelError::Decode(format!("invalid FIM stream frame: {error}")))?;
    if let Some(error) = value.get("error").and_then(Value::as_str) {
        return Err(ModelError::Provider {
            provider: "fim".to_owned(),
            message: bound(error, 500),
        });
    }
    if value.get("done").and_then(Value::as_bool).unwrap_or(false) {
        return Ok(None);
    }
    let text = value
        .get("response")
        .and_then(Value::as_str)
        .unwrap_or_default();
    if text.is_empty() {
        Ok(None)
    } else {
        Ok(Some(text.to_owned()))
    }
}

fn bound(value: &str, max: usize) -> String {
    value.chars().take(max).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn streams_only_response_text() {
        assert_eq!(
            parse_stream_line(br#"{"response":"fn main","done":false}"#).unwrap(),
            Some("fn main".to_owned())
        );
        assert_eq!(
            parse_stream_line(br#"{"response":"","done":true}"#).unwrap(),
            None
        );
        assert!(parse_stream_line(br#"{"error":"model missing"}"#).is_err());
        assert!(parse_stream_line(b"not json").is_err());
    }

    #[test]
    fn only_loopback_origins_are_accepted() {
        for allowed in [
            "http://127.0.0.1:11434",
            "http://localhost:11434",
            "http://[::1]:11434",
        ] {
            let url = reqwest::Url::parse(allowed).unwrap();
            assert!(is_loopback_origin(&url), "{allowed}");
        }
        for rejected in ["https://example.com", "http://192.168.1.2:11434"] {
            let url = reqwest::Url::parse(rejected).unwrap();
            assert!(!is_loopback_origin(&url), "{rejected}");
        }
    }

    #[test]
    fn rejects_remote_endpoints_and_empty_models() {
        assert!(FimClient::new(FimConfig::new("https://example.com", "qwen")).is_err());
        assert!(FimClient::new(FimConfig::new("http://127.0.0.1:11434", "  ")).is_err());
    }
}
