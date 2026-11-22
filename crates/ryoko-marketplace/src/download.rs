//! Secure download and verification logic.

use std::path::PathBuf;
use std::time::Duration;

use futures::StreamExt;
use sha2::{Digest, Sha256};
use tokio::io::AsyncWriteExt;
use url::Url;

use crate::{MAX_DOWNLOAD_BYTES, MarketplaceEntry, MarketplaceError};

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
    // Create cache directory if needed
    ensure_cache_dir(config).await?;

    // Validate URL before requesting
    let url = Url::parse(&entry.asset_url)
        .map_err(|e| MarketplaceError::InvalidUrl(format!("invalid asset URL: {}", e)))?;
    let scheme = url.scheme();
    if scheme != "https" && scheme != "http" {
        return Err(MarketplaceError::InvalidUrl(format!(
            "asset URL must use HTTPS or HTTP: {}",
            scheme
        )));
    }
    if scheme == "http"
        && !url
            .host_str()
            .map(|h| h.starts_with("127.0.0.1") || h == "localhost")
            .unwrap_or(false)
    {
        return Err(MarketplaceError::InvalidUrl(
            "HTTP URLs only allowed for localhost".into(),
        ));
    }

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

    // Check for redirects (reqwest follows them by default, so we need to check)
    let final_url = response.url().clone();
    if final_url != url {
        // Verify the redirect didn't go to an unsafe location
        let final_scheme = final_url.scheme();
        if final_scheme != "https" && final_scheme != "http" {
            return Err(MarketplaceError::InvalidUrl(format!(
                "redirect to unsafe URL scheme: {}",
                final_scheme
            )));
        }
        if final_scheme == "http"
            && !final_url
                .host_str()
                .map(|h| h.starts_with("127.0.0.1") || h == "localhost")
                .unwrap_or(false)
        {
            return Err(MarketplaceError::InvalidUrl(
                "redirect to non-localhost HTTP URL".into(),
            ));
        }
    }

    // Stream download with size limiting and hash computation
    let temp_path = config.cache_dir.join(format!("{}.tmp", entry.id));

    let mut file = tokio::fs::File::create(&temp_path)
        .await
        .map_err(|e| MarketplaceError::Io(format!("create temp file: {}", e)))?;

    let mut hasher = Sha256::new();
    let mut total_bytes: u64 = 0;
    let mut stream = response.bytes_stream();

    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|e| MarketplaceError::network(format!("stream error: {}", e)))?;
        let bytes = chunk.len() as u64;
        total_bytes += bytes;

        if total_bytes > config.max_size_bytes {
            let _ = tokio::fs::remove_file(&temp_path).await;
            return Err(MarketplaceError::download(format!(
                "exceeds max size of {} bytes (got {})",
                config.max_size_bytes, total_bytes
            )));
        }

        file.write_all(&chunk)
            .await
            .map_err(|e| MarketplaceError::Io(format!("write to temp file: {}", e)))?;

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

    let final_path = config.cache_dir.join(entry.artifact_filename()?);
    tokio::fs::rename(&temp_path, &final_path)
        .await
        .map_err(|e| MarketplaceError::Io(format!("rename to final path: {}", e)))?;

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
