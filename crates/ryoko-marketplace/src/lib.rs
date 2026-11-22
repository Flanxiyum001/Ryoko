//! Ryoko Marketplace
//!
//! A marketplace for extensions (MCP connectors) and skills (WASI plugins).
//!
//! # Architecture
//!
//! - **MarketplaceClient**: HTTP client for the registry API
//! - **ExtensionManifest**: Metadata for MCP connectors
//! - **SkillManifest**: Metadata for WASI plugins (extends PluginManifest)
//! - **MarketplaceIndex**: Response from registry listing
//!
//! # Usage
//!
//! ```no_run
//! use ryoko_marketplace::MarketplaceClient;
//!
//! #[tokio::main]
//! async fn main() -> Result<(), Box<dyn std::error::Error>> {
//!     let client = MarketplaceClient::new();
//!
//!     // List available extensions
//!     let extensions = client.list_extensions().await?;
//!
//!     // Search for skills
//!     let skills = client.search("security").await?;
//!
//!     // Install an extension
//!     client.install_extension("github-fetch").await?;
//!
//!     Ok(())
//! }
//! ```

mod client;
mod download;
mod errors;
mod types;

pub use client::{MarketplaceClient, UpdateInfo};
pub use download::DownloadConfig;
pub use errors::MarketplaceError;
pub use types::{
    DownloadResult, ExtensionManifest, InstallationRecord, MarketplaceEntry, MarketplaceIndex,
    MarketplaceLock, MarketplaceType, MarketplaceVersion, SkillKind, SkillManifest,
};

// Re-export from ryoko-core and ryoko-scripting
pub use ryoko_core::connectors::ConnectorConfig;
pub use ryoko_scripting::PluginManifest;

/// Default marketplace registry URL
pub const DEFAULT_MARKETPLACE_URL: &str = "https://marketplace.ryoko.ai";

/// Maximum download size: 64MB
pub const MAX_DOWNLOAD_BYTES: u64 = 64 * 1024 * 1024;

/// Default cache directory
pub const DEFAULT_CACHE_DIR: &str = ".ryoko/cache";

/// Default marketplace lock file
pub const MARKETPLACE_LOCK_FILE: &str = ".ryoko/marketplace.lock.json";
