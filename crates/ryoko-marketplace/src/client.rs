//! Marketplace HTTP client for registry communication.

use std::time::Duration;

use futures::StreamExt;
use reqwest::Client;
use url::Url;

use crate::{
    DownloadConfig, MarketplaceEntry, MarketplaceError, MarketplaceIndex,
    download::{DownloadResult, marketplace_url_allowed},
};

/// Default registry endpoints
const DEFAULT_INDEX_ENDPOINT: &str = "/v1/index";
#[allow(dead_code)]
const DEFAULT_DOWNLOAD_ENDPOINT: &str = "/v1/download/";

/// Cap the untrusted registry payload before JSON decoding.
const MAX_INDEX_BYTES: usize = 4 * 1024 * 1024;

/// Client for interacting with the Ryoko marketplace.
#[derive(Debug, Clone)]
pub struct MarketplaceClient {
    base_url: Url,
    http_client: Client,
    download_config: DownloadConfig,
}

impl MarketplaceClient {
    /// Create a new marketplace client with the default URL.
    pub fn new() -> Result<Self, MarketplaceError> {
        Self::with_url(crate::DEFAULT_MARKETPLACE_URL)
    }

    /// Create a client with a custom registry URL.
    pub fn with_url(base_url: impl Into<String>) -> Result<Self, MarketplaceError> {
        let base_url_str = base_url.into();

        // Ensure URL ends without trailing slash
        let base_str = base_url_str.trim_end_matches('/');

        let base_url = Url::parse(base_str)
            .map_err(|e| MarketplaceError::InvalidUrl(format!("invalid registry URL: {}", e)))?;

        if !marketplace_url_allowed(&base_url) {
            return Err(MarketplaceError::InvalidUrl(
                "registry URL must use HTTPS, or HTTP with an exact loopback host".into(),
            ));
        }

        let http_client = Client::builder()
            .timeout(Duration::from_secs(30))
            .redirect(reqwest::redirect::Policy::custom(|attempt| {
                if attempt.previous().len() >= 10
                    || !marketplace_url_allowed(attempt.url())
                    || attempt.previous().last().is_some_and(|previous| {
                        previous.scheme() == "https" && attempt.url().scheme() != "https"
                    })
                {
                    attempt.stop()
                } else {
                    attempt.follow()
                }
            }))
            .user_agent("Ryoko-Marketplace/1.0")
            .build()
            .map_err(|e| MarketplaceError::Http(format!("create HTTP client: {}", e)))?;

        Ok(Self {
            base_url,
            http_client,
            download_config: DownloadConfig::default(),
        })
    }

    /// Create a client with a custom download configuration.
    pub fn with_download_config(mut self, config: DownloadConfig) -> Self {
        self.download_config = config;
        self
    }

    /// Fetch the current marketplace index.
    pub async fn fetch_index(&self) -> Result<MarketplaceIndex, MarketplaceError> {
        let url = format!("{}{}", self.base_url, DEFAULT_INDEX_ENDPOINT);

        let response = self
            .http_client
            .get(&url)
            .send()
            .await
            .map_err(|e| MarketplaceError::network(format!("fetch index: {}", e)))?;

        if !response.status().is_success() {
            return Err(MarketplaceError::Registry(format!(
                "HTTP {}: {}",
                response.status(),
                response.status().canonical_reason().unwrap_or("unknown")
            )));
        }

        if response
            .content_length()
            .is_some_and(|bytes| bytes > MAX_INDEX_BYTES as u64)
        {
            return Err(MarketplaceError::Registry(
                "marketplace index exceeds 4 MiB".into(),
            ));
        }

        let mut bytes = Vec::new();
        let mut stream = response.bytes_stream();
        while let Some(chunk) = stream.next().await {
            let chunk =
                chunk.map_err(|e| MarketplaceError::network(format!("read index: {}", e)))?;
            if chunk.len() > MAX_INDEX_BYTES.saturating_sub(bytes.len()) {
                return Err(MarketplaceError::Registry(
                    "marketplace index exceeds 4 MiB".into(),
                ));
            }
            bytes.extend_from_slice(&chunk);
        }

        serde_json::from_slice::<MarketplaceIndex>(&bytes)
            .map_err(|e| MarketplaceError::Parse(format!("parse index JSON: {}", e)))
    }

    /// Search for extensions by term.
    pub async fn search_extensions(
        &self,
        term: &str,
    ) -> Result<Vec<MarketplaceEntry>, MarketplaceError> {
        let index = self.fetch_index().await?;
        Ok(index.search_extensions(term).into_iter().cloned().collect())
    }

    /// Search for skills by term.
    pub async fn search_skills(
        &self,
        term: &str,
    ) -> Result<Vec<MarketplaceEntry>, MarketplaceError> {
        let index = self.fetch_index().await?;
        Ok(index.search_skills(term).into_iter().cloned().collect())
    }

    /// Find a specific item by ID.
    pub async fn find_item(&self, id: &str) -> Result<Option<MarketplaceEntry>, MarketplaceError> {
        let index = self.fetch_index().await?;
        Ok(index.find_entry(id).cloned())
    }

    /// Get an extension by ID.
    pub async fn get_extension(
        &self,
        id: &str,
    ) -> Result<Option<MarketplaceEntry>, MarketplaceError> {
        let index = self.fetch_index().await?;
        Ok(index.extensions.iter().find(|e| e.id == id).cloned())
    }

    /// Get a skill by ID.
    pub async fn get_skill(&self, id: &str) -> Result<Option<MarketplaceEntry>, MarketplaceError> {
        let index = self.fetch_index().await?;
        Ok(index.skills.iter().find(|e| e.id == id).cloned())
    }

    /// List all extensions.
    pub async fn list_extensions(&self) -> Result<Vec<MarketplaceEntry>, MarketplaceError> {
        let index = self.fetch_index().await?;
        Ok(index.extensions.clone())
    }

    /// List all skills.
    pub async fn list_skills(&self) -> Result<Vec<MarketplaceEntry>, MarketplaceError> {
        let index = self.fetch_index().await?;
        Ok(index.skills.clone())
    }

    /// Install an extension by ID.
    /// Returns the download result with path and checksum.
    pub async fn install_extension(&self, id: &str) -> Result<DownloadResult, MarketplaceError> {
        let entry = self
            .get_extension(id)
            .await?
            .ok_or_else(|| MarketplaceError::not_found(id))?;

        let result =
            crate::download::download_and_verify(&self.http_client, &entry, &self.download_config)
                .await?;

        Ok(result)
    }

    /// Install a skill by ID.
    /// Returns the path where it was installed.
    pub async fn install_skill(&self, id: &str) -> Result<std::path::PathBuf, MarketplaceError> {
        let entry = self
            .get_skill(id)
            .await?
            .ok_or_else(|| MarketplaceError::not_found(id))?;

        let result =
            crate::download::download_and_verify(&self.http_client, &entry, &self.download_config)
                .await?;

        Ok(result.path)
    }

    /// Check for updates to installed items.
    pub async fn check_for_updates(
        &self,
        installed: &crate::MarketplaceLock,
    ) -> Result<Vec<crate::UpdateInfo>, MarketplaceError> {
        let index = self.fetch_index().await?;
        let mut updates = Vec::new();

        for entry in index.extensions.iter().chain(index.skills.iter()) {
            if let Some(installed_version) = installed.installed_version(&entry.id, entry.kind)
                && entry.version != *installed_version
            {
                updates.push(crate::UpdateInfo {
                    entry: entry.clone(),
                    current_version: installed_version.clone(),
                    latest_version: entry.version.clone(),
                });
            }
        }

        Ok(updates)
    }

    /// Get the base URL for this client.
    pub fn base_url_str(&self) -> &str {
        self.base_url.as_str()
    }

    /// Get download configuration.
    pub fn download_config(&self) -> &DownloadConfig {
        &self.download_config
    }
}

impl Default for MarketplaceClient {
    fn default() -> Self {
        Self::new().expect("default marketplace URL is valid")
    }
}

/// Information about an available update.
#[derive(Debug, Clone)]
pub struct UpdateInfo {
    pub entry: MarketplaceEntry,
    pub current_version: String,
    pub latest_version: String,
}

impl UpdateInfo {
    /// Returns true if this is an upgrade (not downgrade).
    pub fn is_upgrade(&self) -> bool {
        self.entry.version > self.current_version
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn client_accepts_https_urls() {
        let result = MarketplaceClient::with_url("https://marketplace.ryoko.ai");
        assert!(result.is_ok());
    }

    #[test]
    fn client_rejects_external_cleartext_and_fake_loopback() {
        for registry in [
            "http://example.com",
            "http://127.0.0.1.evil.example",
            "http://localhost.evil.example",
            "https://user:password@example.com",
        ] {
            assert!(MarketplaceClient::with_url(registry).is_err(), "{registry}");
        }
    }

    #[test]
    fn client_rejects_file_urls() {
        let result = MarketplaceClient::with_url("file:///tmp/test.json");
        assert!(result.is_err());
    }

    #[test]
    fn client_accepts_http_urls_in_local_mode() {
        // HTTP is accepted for local development
        let result = MarketplaceClient::with_url("http://localhost:8080");
        assert!(result.is_ok());
    }
}
