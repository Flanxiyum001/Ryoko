//! Secure download and verification logic.

use std::path::PathBuf;
use std::time::Duration;

use futures::StreamExt;
use sha2::{Digest, Sha256};
use tokio::io::AsyncWriteExt;
use url::Url;

use crate::{MAX_DOWNLOAD_BYTES, MarketplaceEntry, MarketplaceError};

/// Allow TLS for public artifacts; permit cleartext only for an exact loopback host.
pub(crate) fn marketplace_url_allowed(url: &Url) -> bool {
    if !url.username().is_empty() || url.password().is_some() {
        return false;
    }
    url.scheme() == "https"
        || (url.scheme() == "http"
            && matches!(url.host_str(), Some("localhost" | "127.0.0.1" | "[::1]")))
}

fn valid_checksum(checksum: &str) -> bool {
    checksum.len() == 64
        && checksum
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

/// Configuration for downloads.
#[derive(Debug, Clone)]
pub struct DownloadConfig {
    /// Request timeout
    pub timeout_secs: u64,
    /// Maximum download size (default: 64MB)
    pub max_size_bytes: u64,
    /// Cache directory
    pub cache_dir: PathBuf,
}

impl Default for DownloadConfig {
    fn default() -> Self {
        Self {
            timeout_secs: 30,
            max_size_bytes: MAX_DOWNLOAD_BYTES,
            cache_dir: PathBuf::from(crate::DEFAULT_CACHE_DIR),
        }
    }
}

impl DownloadConfig {
    pub fn new(cache_dir: PathBuf) -> Self {
        Self {
            cache_dir,
            ..Default::default()
        }
    }
}

/// Result of a download operation.
#[derive(Debug)]
#[allow(dead_code)]
pub struct DownloadResult {
    /// Path to the downloaded file
    pub path: PathBuf,
    /// Actual size in bytes
    pub size: u64,
    /// SHA-256 hash of the downloaded content
    pub checksum: String,
}

/// Result wrapper that handles both extension and skill downloads.
#[derive(Debug)]
pub enum DownloadResultWrapper {
    Extension(DownloadResult),
    Skill(PathBuf),
}

/// Ensures the cache directory exists.
pub async fn ensure_cache_dir(config: &DownloadConfig) -> Result<(), MarketplaceError> {
    tokio::fs::create_dir_all(&config.cache_dir)
        .await
        .map_err(|e| MarketplaceError::Io(format!("create cache dir: {}", e)))?;
    Ok(())
}

/// Downloads an artifact and verifies its checksum using streaming to avoid buffering.
pub async fn download_and_verify(
    client: &reqwest::Client,
    entry: &MarketplaceEntry,
    config: &DownloadConfig,
) -> Result<DownloadResult, MarketplaceError> {
    // Validate registry-controlled metadata before starting a network request or file write.
    let artifact_name = entry.artifact_filename()?;
    if !valid_checksum(&entry.checksum) {
        return Err(MarketplaceError::InvalidManifest(
            "artifact checksum must be 64 lowercase hexadecimal SHA-256 characters".into(),
        ));
    }
    let url = Url::parse(&entry.asset_url)
        .map_err(|e| MarketplaceError::InvalidUrl(format!("invalid asset URL: {}", e)))?;
    if !marketplace_url_allowed(&url) {
        return Err(MarketplaceError::InvalidUrl(
            "artifact URL must use HTTPS, or HTTP with an exact loopback host".into(),
        ));
    }
    ensure_cache_dir(config).await?;

    // Download the file with streaming
    let response = client
        .get(&entry.asset_url)
        .timeout(Duration::from_secs(config.timeout_secs))
        .send()
        .await
        .map_err(|e| MarketplaceError::network(format!("request failed: {}", e)))?;

    if !response.status().is_success() {
        return Err(MarketplaceError::download(format!(
            "HTTP {}: {}",
            response.status(),
            response.status().canonical_reason().unwrap_or("unknown")
        )));
    }

    // The MarketplaceClient also rejects insecure redirects *before* following them.
    // Repeat validation here for clients supplied directly to this helper.
    let final_url = response.url();
    if !marketplace_url_allowed(final_url)
        || (url.scheme() == "https" && final_url.scheme() != "https")
    {
        return Err(MarketplaceError::InvalidUrl(
            "artifact redirect downgraded transport security".into(),
        ));
    }

    if response
        .content_length()
        .is_some_and(|size| size > config.max_size_bytes)
    {
        return Err(MarketplaceError::download(
            "response exceeds maximum download size",
        ));
    }

    // Stream download with size limiting and hash computation
    let temp_path =
        config
            .cache_dir
            .join(format!(".ryoko-{}-{}.tmp", entry.id, uuid::Uuid::new_v4()));

    let mut file = tokio::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&temp_path)
        .await
        .map_err(|e| MarketplaceError::Io(format!("create temp file: {}", e)))?;

    let mut hasher = Sha256::new();
    let mut total_bytes: u64 = 0;
    let mut stream = response.bytes_stream();

    while let Some(chunk) = stream.next().await {
        let chunk = match chunk {
            Ok(value) => value,
            Err(error) => {
                drop(file);
                let _ = tokio::fs::remove_file(&temp_path).await;
                return Err(MarketplaceError::network(format!(
                    "stream error: {}",
                    error
                )));
            }
        };
        let bytes = chunk.len() as u64;
        total_bytes += bytes;

        if total_bytes > config.max_size_bytes {
            drop(file);
            let _ = tokio::fs::remove_file(&temp_path).await;
            return Err(MarketplaceError::download(format!(
                "exceeds max size of {} bytes (got {})",
                config.max_size_bytes, total_bytes
            )));
        }

        if let Err(error) = file.write_all(&chunk).await {
            drop(file);
            let _ = tokio::fs::remove_file(&temp_path).await;
            return Err(MarketplaceError::Io(format!(
                "write to temp file: {}",
                error
            )));
        }

        hasher.update(&chunk);
    }

    drop(file);

    // Compute and verify SHA-256 hash
    let actual_hash = format!("{:x}", hasher.finalize());
    let expected_hash = entry.checksum.clone();

    if actual_hash != expected_hash {
        let _ = tokio::fs::remove_file(&temp_path).await;
        return Err(MarketplaceError::checksum(expected_hash, actual_hash));
    }

    let final_path = config.cache_dir.join(artifact_name);
    if let Err(error) = tokio::fs::rename(&temp_path, &final_path).await {
        let _ = tokio::fs::remove_file(&temp_path).await;
        return Err(MarketplaceError::Io(format!(
            "rename to final path: {}",
            error
        )));
    }

    Ok(DownloadResult {
        path: final_path,
        size: total_bytes,
        checksum: actual_hash,
    })
}

/// Verifies a local file's checksum.
#[allow(dead_code)]
pub fn verify_checksum(path: &std::path::Path, expected: &str) -> Result<String, MarketplaceError> {
    let mut file =
        std::fs::File::open(path).map_err(|e| MarketplaceError::Io(format!("open file: {}", e)))?;

    let mut hasher = Sha256::new();
    let mut buffer = vec![0u8; 8192];
    let mut total_bytes = 0u64;

    loop {
        let n = std::io::Read::read(&mut file, &mut buffer)
            .map_err(|e| MarketplaceError::Io(format!("read file: {}", e)))?;
        if n == 0 {
            break;
        }
        total_bytes += n as u64;
        if total_bytes > MAX_DOWNLOAD_BYTES {
            return Err(MarketplaceError::Download(format!(
                "file exceeds max size of {} bytes",
                MAX_DOWNLOAD_BYTES
            )));
        }
        hasher.update(&buffer[..n]);
    }

    let actual = format!("{:x}", hasher.finalize());
    if actual != expected {
        return Err(MarketplaceError::checksum(expected.to_string(), actual));
    }

    Ok(actual)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::TempDir;

    #[test]
    fn verifies_checksum_correctly() {
        let content = b"test content for checksum";
        let expected: String = format!("{:x}", Sha256::digest(content));

        let dir = TempDir::new().unwrap();
        let path = dir.path().join("test.wasm");
        let mut file = std::fs::File::create(&path).unwrap();
        file.write_all(content).unwrap();
        drop(file);

        let result = verify_checksum(&path, &expected);
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), expected);
    }

    #[test]
    fn url_security_rejects_fake_loopback_and_embedded_credentials() {
        for raw in [
            "http://127.0.0.1.evil.example/asset.wasm",
            "http://localhost.evil.example/asset.wasm",
            "http://10.0.0.1/asset.wasm",
            "http://example.org/asset.wasm",
            "https://user:password@example.org/asset.wasm",
            "file:///tmp/asset.wasm",
        ] {
            assert!(!marketplace_url_allowed(&Url::parse(raw).unwrap()), "{raw}");
        }
        for raw in [
            "https://example.org/asset.wasm",
            "http://127.0.0.1/asset.wasm",
            "http://localhost:8080/asset.wasm",
        ] {
            assert!(marketplace_url_allowed(&Url::parse(raw).unwrap()), "{raw}");
        }
    }

    #[test]
    fn rejects_malformed_registry_checksums_before_download() {
        assert!(valid_checksum(&"a".repeat(64)));
        assert!(!valid_checksum(&"A".repeat(64)));
        assert!(!valid_checksum(&"z".repeat(64)));
        assert!(!valid_checksum(&"a".repeat(63)));
    }

    #[test]
    fn rejects_bad_checksum() {
        let content = b"test content";
        let bad_hash = "0".repeat(64);

        let dir = TempDir::new().unwrap();
        let path = dir.path().join("test.wasm");
        std::fs::write(&path, content).unwrap();

        let result = verify_checksum(&path, &bad_hash);
        assert!(result.is_err());
    }
}
