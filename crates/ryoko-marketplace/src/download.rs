//! Secure download and verification logic.

use std::path::PathBuf;
use std::time::Duration;

use sha2::{Digest, Sha256};

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

/// Downloads an artifact and verifies its checksum.
pub async fn download_and_verify(
    client: &reqwest::Client,
    entry: &MarketplaceEntry,
    config: &DownloadConfig,
) -> Result<DownloadResult, MarketplaceError> {
    // Create cache directory if needed
    ensure_cache_dir(config).await?;

    // Download the file to memory first
    let bytes = client
        .get(&entry.asset_url)
        .timeout(Duration::from_secs(config.timeout_secs))
        .send()
        .await
        .map_err(|e| MarketplaceError::network(format!("request failed: {}", e)))?
        .bytes()
        .await
        .map_err(|e| MarketplaceError::network(format!("reading response: {}", e)))?;

    if bytes.len() as u64 > config.max_size_bytes {
        return Err(MarketplaceError::download(format!(
            "exceeds max size of {} bytes (got {})",
            config.max_size_bytes,
            bytes.len()
        )));
    }

    // Compute SHA-256 hash
    let actual_hash = format!("{:x}", Sha256::digest(&bytes));
    let expected_hash = entry.checksum.clone();

    if actual_hash != expected_hash {
        return Err(MarketplaceError::checksum(expected_hash, actual_hash));
    }

    // Write to temp file, then rename to final location
    let temp_path = config.cache_dir.join(format!("{}.tmp", entry.id));
    let final_path = config.cache_dir.join(entry.artifact_filename()?);
    tokio::fs::write(&temp_path, &bytes)
        .await
        .map_err(|e| MarketplaceError::Io(format!("write to temp file: {}", e)))?;

    tokio::fs::rename(&temp_path, &final_path)
        .await
        .map_err(|e| MarketplaceError::Io(format!("rename to final path: {}", e)))?;

    Ok(DownloadResult {
        path: final_path,
        size: bytes.len() as u64,
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
