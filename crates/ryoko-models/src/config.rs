//! User-configurable OpenAI-compatible provider profiles.
//!
//! Provider files contain only environment variable *names*, never credentials.
//! Every profile is validated before construction; arbitrary network destinations
//! must be treated as trusted user input, not as model-generated configuration.
use std::collections::HashSet;
use std::net::IpAddr;
use std::sync::Arc;

use serde::{Deserialize, Serialize};

use crate::{ModelError, ModelRouter, OpenAiCompatConfig, OpenAiCompatProvider};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProviderProfile {
    pub id: String,
    /// Base URL, e.g. https://api.example.com/v1 or http://127.0.0.1:11434/v1.
    pub base_url: String,
    /// Environment variable holding the secret; never put the key in this file.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub api_key_env: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub chat_path: Option<String>,
    /// Allows cleartext HTTP to non-loopback hosts without credentials.
    /// Use only on a trusted network; TLS is mandatory when sending API keys.
    #[serde(default)]
    pub allow_insecure_http: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProviderCatalog {
    pub default_provider: String,
    pub providers: Vec<ProviderProfile>,
}

fn invalid(message: impl Into<String>) -> ModelError {
    ModelError::Configuration(message.into())
}

fn valid_id(id: &str) -> bool {
    !id.is_empty()
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

fn valid_env(name: &str) -> bool {
    let mut chars = name.bytes();
    matches!(chars.next(), Some(b'A'..=b'Z' | b'_'))
        && chars.all(|b| b.is_ascii_uppercase() || b.is_ascii_digit() || b == b'_')
}

fn is_loopback(host: &str) -> bool {
    host.eq_ignore_ascii_case("localhost")
        || host
            .trim_matches(|ch| ch == '[' || ch == ']')
            .parse::<IpAddr>()
            .is_ok_and(|ip| ip.is_loopback())
}

impl ProviderProfile {
    pub fn validate(&self) -> Result<(), ModelError> {
        if !valid_id(&self.id) {
            return Err(invalid(
                "provider id must contain only letters, digits, '-' or '_'",
            ));
        }
        let url = reqwest::Url::parse(&self.base_url)
            .map_err(|_| invalid(format!("invalid base URL for provider {}", self.id)))?;
        if !matches!(url.scheme(), "http" | "https")
            || url.host_str().is_none()
            || !url.username().is_empty()
            || url.password().is_some()
            || url.query().is_some()
            || url.fragment().is_some()
        {
            return Err(invalid(format!(
                "provider {} requires an HTTP(S) URL without credentials, query or fragment",
                self.id
            )));
        }
        if let Some(env_name) = &self.api_key_env {
            if !valid_env(env_name) {
                return Err(invalid(format!(
                    "provider {} has an invalid api_key_env",
                    self.id
                )));
            }
        }
        if url.scheme() == "http" {
            let loopback = url.host_str().is_some_and(is_loopback);
            if !loopback && !self.allow_insecure_http {
                return Err(invalid(format!(
                    "provider {} must use HTTPS outside loopback (or explicitly opt in for keyless HTTP)",
                    self.id
                )));
            }
            if !loopback && self.api_key_env.is_some() {
                return Err(invalid(format!(
                    "provider {} cannot transmit a key over non-loopback HTTP",
                    self.id
                )));
            }
        }
        if let Some(path) = &self.chat_path {
            if !path.starts_with('/')
                || path.starts_with("//")
                || path.contains('?')
                || path.contains('#')
                || path.contains('\\')
                || path.split('/').any(|part| part == "." || part == "..")
            {
                return Err(invalid(format!(
                    "provider {} has an invalid chat_path",
                    self.id
                )));
            }
        }
        Ok(())
    }

    fn to_provider(&self) -> Result<OpenAiCompatProvider, ModelError> {
        self.validate()?;
        let mut config = OpenAiCompatConfig::new(&self.id, &self.base_url);
        if let Some(path) = &self.chat_path {
            config.chat_path = path.clone();
        }
        if let Some(var) = &self.api_key_env {
            let key = std::env::var(var).map_err(|_| {
                invalid(format!(
                    "provider {} requires the environment variable {var}",
                    self.id
                ))
            })?;
            if key.trim().is_empty() {
                return Err(invalid(format!(
                    "provider {} has an empty API key environment variable",
                    self.id
                )));
            }
            config = config.with_api_key(key);
        }
        OpenAiCompatProvider::new(config)
    }
}

impl ProviderCatalog {
    pub fn parse_json(json: &str) -> Result<Self, ModelError> {
        let catalog: Self = serde_json::from_str(json)
            .map_err(|err| invalid(format!("invalid provider catalog JSON: {err}")))?;
        catalog.validate()?;
        Ok(catalog)
    }

    pub fn validate(&self) -> Result<(), ModelError> {
        if !valid_id(&self.default_provider) {
            return Err(invalid("invalid default_provider"));
        }
        if self.providers.is_empty() {
            return Err(invalid("provider catalog cannot be empty"));
        }
        let mut seen = HashSet::new();
        for profile in &self.providers {
            profile.validate()?;
            if !seen.insert(profile.id.as_str()) {
                return Err(invalid(format!("duplicate provider id {}", profile.id)));
            }
        }
        if !seen.contains(self.default_provider.as_str()) {
            return Err(invalid("default_provider is not present in providers"));
        }
        Ok(())
    }

    pub fn build_router(&self) -> Result<ModelRouter, ModelError> {
        self.validate()?;
        let mut router = ModelRouter::new(&self.default_provider);
        for profile in &self.providers {
            router.register(Arc::new(profile.to_provider()?));
        }
        Ok(router)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn example() -> ProviderCatalog {
        ProviderCatalog {
            default_provider: "local".into(),
            providers: vec![ProviderProfile {
                id: "local".into(),
                base_url: "http://127.0.0.1:11434/v1".into(),
                api_key_env: None,
                chat_path: None,
                allow_insecure_http: false,
            }],
        }
    }

    #[test]
    fn accepts_local_keyless_provider() {
        let catalog = example();
        catalog.validate().unwrap();
        let router = catalog.build_router().unwrap();
        assert_eq!(router.default_provider(), "local");
        assert_eq!(router.providers().count(), 1);
    }

    #[test]
    fn supports_ipv6_loopback() {
        let mut catalog = example();
        catalog.providers[0].base_url = "http://[::1]:1234/v1".into();
        assert!(catalog.validate().is_ok());
    }

    #[test]
    fn rejects_cleartext_remote_key_and_requires_explicit_opt_in() {
        let mut catalog = example();
        catalog.providers[0].base_url = "http://intranet.example/v1".into();
        assert!(catalog.validate().is_err());
        catalog.providers[0].allow_insecure_http = true;
        assert!(catalog.validate().is_ok());
        catalog.providers[0].api_key_env = Some("RYOKO_TEST_KEY".into());
        assert!(catalog.validate().is_err());
    }

    #[test]
    fn rejects_embedded_secrets_and_bad_paths() {
        let mut catalog = example();
        catalog.providers[0].base_url = "https://user:password@example.com/v1".into();
        assert!(catalog.validate().is_err());
        catalog.providers[0].base_url = "https://example.com/v1".into();
        catalog.providers[0].chat_path = Some("//evil.example/chat".into());
        assert!(catalog.validate().is_err());
        catalog.providers[0].chat_path = Some("/chat/../completions".into());
        assert!(catalog.validate().is_err());
    }

    #[test]
    fn rejects_unknown_fields_and_duplicate_ids() {
        assert!(
            ProviderCatalog::parse_json(
                r#"{"default_provider":"local","providers":[],"api_key":"secret"}"#
            )
            .is_err()
        );
        let mut catalog = example();
        catalog.providers.push(catalog.providers[0].clone());
        assert!(catalog.validate().is_err());
    }

    #[test]
    fn errors_do_not_echo_secret_env_values() {
        let catalog = example();
        let json = serde_json::to_string(&catalog).unwrap();
        assert!(!json.contains("api_key\":\""));
        assert!(ProviderCatalog::parse_json(&json).is_ok());
    }
}
