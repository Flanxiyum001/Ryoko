//! Best-effort discovery of local model servers on fixed loopback ports.
//!
//! This does not probe the LAN, transmit credentials, follow redirects or
//! enable a remote provider. The user still chooses which model to execute.
use std::time::Duration;

use futures::StreamExt;
use serde::{Deserialize, Serialize};
use serde_json::Value;

const RESPONSE_LIMIT: usize = 64 * 1024;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LocalModel {
    /// Selectable provider prefix: \`ollama\` or \`lmstudio\`.
    pub provider: String,
    pub model: String,
    pub base_url: String,
}

impl LocalModel {
    pub fn selection(&self) -> String {
        format!("{}/{}", self.provider, self.model)
    }
}

/// Scan only well-known loopback endpoints. Missing servers are normal and
/// return an empty result; discovery never changes user configuration.
pub async fn discover_local_models() -> Vec<LocalModel> {
    let client = match reqwest::Client::builder()
        .connect_timeout(Duration::from_millis(700))
        .timeout(Duration::from_secs(2))
        .redirect(reqwest::redirect::Policy::none())
        .no_proxy()
        .build()
    {
        Ok(client) => client,
        Err(_) => return Vec::new(),
    };
    let mut found = Vec::new();
    for (provider, url, base_url) in [
        (
            "ollama",
            "http://127.0.0.1:11434/api/tags",
            "http://127.0.0.1:11434/v1",
        ),
        (
            "lmstudio",
            "http://127.0.0.1:1234/v1/models",
            "http://127.0.0.1:1234/v1",
        ),
    ] {
        let Ok(response) = client.get(url).send().await else {
            continue;
        };
        if !response.status().is_success() {
            continue;
        }
        let mut body = Vec::new();
        let mut stream = response.bytes_stream();
        let mut overflow = false;
        while let Some(chunk) = stream.next().await {
            let Ok(chunk) = chunk else {
                overflow = true;
                break;
            };
            if body.len().saturating_add(chunk.len()) > RESPONSE_LIMIT {
                overflow = true;
                break;
            }
            body.extend_from_slice(&chunk);
        }
        if overflow {
            continue;
        }
        if let Ok(value) = serde_json::from_slice::<Value>(&body) {
            found.extend(parse_models(provider, base_url, &value));
        }
    }
    found
}

fn parse_models(provider: &str, base_url: &str, value: &Value) -> Vec<LocalModel> {
    let entries = match provider {
        "ollama" => value.get("models").and_then(Value::as_array),
        "lmstudio" => value.get("data").and_then(Value::as_array),
        _ => None,
    };
    let Some(entries) = entries else {
        return Vec::new();
    };
    let field = if provider == "ollama" { "name" } else { "id" };
    let mut models = Vec::new();
    for value in entries.iter().take(256) {
        let Some(name) = value.get(field).and_then(Value::as_str) else {
            continue;
        };
        if name.is_empty()
            || name.len() > 128
            || name.chars().any(char::is_control)
            || models.iter().any(|model: &LocalModel| model.model == name)
        {
            continue;
        }
        models.push(LocalModel {
            provider: provider.to_owned(),
            model: name.to_owned(),
            base_url: base_url.to_owned(),
        });
    }
    models
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_ollama_and_lm_studio_responses() {
        let ollama = serde_json::json!({"models": [{"name": "qwen2.5-coder:7b"}, {"name": ""}]});
        let lmstudio = serde_json::json!({"data": [{"id": "local/coder"}, {"id": "local/coder"}]});
        let a = parse_models("ollama", "http://127.0.0.1:11434/v1", &ollama);
        let b = parse_models("lmstudio", "http://127.0.0.1:1234/v1", &lmstudio);
        assert_eq!(a.len(), 1);
        assert_eq!(a[0].selection(), "ollama/qwen2.5-coder:7b");
        assert_eq!(b.len(), 1);
        assert_eq!(b[0].selection(), "lmstudio/local/coder");
    }

    #[test]
    fn refuses_invalid_and_oversized_names() {
        let entries = serde_json::json!({"models": [
            {"name": "safe"},
            {"name": "safe"},
            {"name": "has\nnewline"},
            {"name": "x".repeat(129)},
            {"name": 123}
        ]});
        let models = parse_models("ollama", "http://localhost", &entries);
        assert_eq!(models.len(), 1);
        assert_eq!(models[0].model, "safe");
    }

    #[test]
    fn wrong_provider_or_format_discovers_nothing() {
        assert!(parse_models("unknown", "url", &serde_json::json!({})).is_empty());
        assert!(parse_models("ollama", "url", &serde_json::json!({"models": true})).is_empty());
    }
}
