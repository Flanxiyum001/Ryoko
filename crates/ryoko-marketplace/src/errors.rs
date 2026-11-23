//! Marketplace error types.

use thiserror::Error;

#[derive(Debug, Error)]
pub enum MarketplaceError {
    #[error("HTTP request failed: {0}")]
    Http(String),

    #[error("Network error: {0}")]
    Network(String),

    #[error("Invalid URL: {0}")]
    InvalidUrl(String),

    #[error("Download failed: {0}")]
    Download(String),

    #[error("Checksum verification failed: expected {expected}, got {actual}")]
    Checksum { expected: String, actual: String },

    #[error("Invalid manifest: {0}")]
    InvalidManifest(String),

    #[error("Version conflict: {0}")]
    VersionConflict(String),

    #[error("Installation failed: {0}")]
    Installation(String),

    #[error("IO error: {0}")]
    Io(String),

    #[error("Parse error: {0}")]
    Parse(String),

    #[error("Registry error: {0}")]
    Registry(String),

    #[error("Authentication required: {0}")]
    AuthRequired(String),

    #[error("Not found: {0}")]
    NotFound(String),

    #[error("Dependency not found: {0}")]
    MissingDependency(String),

    #[error("Incompatible version: {0}")]
    IncompatibleVersion(String),
}

impl MarketplaceError {
    pub fn http(msg: impl Into<String>) -> Self {
        Self::Http(msg.into())
    }

    pub fn network(msg: impl Into<String>) -> Self {
        Self::Network(msg.into())
    }

    pub fn download(msg: impl Into<String>) -> Self {
        Self::Download(msg.into())
    }

    pub fn checksum(expected: impl Into<String>, actual: impl Into<String>) -> Self {
        Self::Checksum {
            expected: expected.into(),
            actual: actual.into(),
        }
    }

    pub fn not_found(id: &str) -> Self {
        Self::NotFound(format!("{} not found in marketplace", id))
    }
}

impl From<MarketplaceError> for String {
    fn from(err: MarketplaceError) -> Self {
        err.to_string()
    }
}
