//! Marketplace manifest and index types.
//!
//! These types mirror and extend the patterns used in ryoko-core and ryoko-scripting.

use serde::{Deserialize, Serialize};

// Re-export ConnectorConfig
pub use crate::ConnectorConfig;

/// The type of marketplace item.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MarketplaceType {
    /// MCP connector extension
    Extension,
    /// WASI plugin skill
    Skill,
}

impl MarketplaceType {
    /// Returns the label for clap display.
    pub fn label(self) -> &'static str {
        match self {
            Self::Extension => "extension",
            Self::Skill => "skill",
        }
    }
}

impl std::fmt::Display for MarketplaceType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.label())
    }
}

impl clap::ValueEnum for MarketplaceType {
    fn value_variants<'a>() -> &'a [Self] {
        &[Self::Extension, Self::Skill]
    }

    fn to_possible_value(&self) -> Option<clap::builder::PossibleValue> {
        Some(clap::builder::PossibleValue::new(self.label()))
    }
}

/// A skill type - what the skill does.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SkillKind {
    /// Analyzes code and returns findings
    Analyzer,
    /// Generates or revises code
    Generator,
    /// Provides linting/style feedback
    Linter,
    /// Extracts context/symbols
    Context,
    /// Performs security analysis
    Security,
    /// Custom/unspecified
    Custom,
}

impl SkillKind {
    pub fn label(self) -> &'static str {
        match self {
            Self::Analyzer => "analyzer",
            Self::Generator => "generator",
            Self::Linter => "linter",
            Self::Context => "context",
            Self::Security => "security-analyzer",
            Self::Custom => "custom",
        }
    }
}

/// Marketplace entry metadata (public info from registry).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MarketplaceEntry {
    pub id: String,
    pub name: String,
    pub description: String,
    pub version: String,
    pub author: String,
    pub homepage: Option<String>,
    pub repository: Option<String>,
    pub license: Option<String>,
    pub kind: MarketplaceType,
    pub tags: Vec<String>,
    pub downloads: u64,
    pub compatible_with: Vec<String>,
    /// Direct download URL for the artifact
    pub asset_url: String,
    /// SHA-256 hash of the artifact (lowercase hex)
    pub checksum: String,
    /// When this entry was last updated
    pub updated_at: chrono::DateTime<chrono::Utc>,
}

impl MarketplaceEntry {
    /// Returns the file to save the artifact as (includes extension based on type).
    pub fn artifact_filename(&self) -> String {
        let extension = match self.kind {
            MarketplaceType::Extension => "json",
            MarketplaceType::Skill => "wasm",
        };
        format!("{}.{}", self.id, extension)
    }

    /// Returns true if this is compatible with the given version.
    pub fn is_compatible_with(&self, version: &str) -> bool {
        self.compatible_with.is_empty()
            || self.compatible_with.iter().any(|v| version.starts_with(v))
    }
}

/// Index response from the marketplace registry.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MarketplaceIndex {
    pub registry_version: u32,
    pub generated_at: chrono::DateTime<chrono::Utc>,
    pub extensions: Vec<MarketplaceEntry>,
    pub skills: Vec<MarketplaceEntry>,
}

impl MarketplaceIndex {
    /// Get entry by ID across all types.
    pub fn find_entry(&self, id: &str) -> Option<&MarketplaceEntry> {
        self.extensions
            .iter()
            .find(|e| e.id == id)
            .or_else(|| self.skills.iter().find(|e| e.id == id))
    }

    /// Get extensions matching a search term.
    pub fn search_extensions(&self, term: &str) -> Vec<&MarketplaceEntry> {
        let lower = term.to_ascii_lowercase();
        self.extensions
            .iter()
            .filter(|e| {
                e.id.to_ascii_lowercase().contains(&lower)
                    || e.name.to_ascii_lowercase().contains(&lower)
                    || e.description.to_ascii_lowercase().contains(&lower)
                    || e.tags
                        .iter()
                        .any(|t| t.to_ascii_lowercase().contains(&lower))
            })
            .collect()
    }

    /// Get skills matching a search term.
    pub fn search_skills(&self, term: &str) -> Vec<&MarketplaceEntry> {
        let lower = term.to_ascii_lowercase();
        self.skills
            .iter()
            .filter(|e| {
                e.id.to_ascii_lowercase().contains(&lower)
                    || e.name.to_ascii_lowercase().contains(&lower)
                    || e.description.to_ascii_lowercase().contains(&lower)
                    || e.tags
                        .iter()
                        .any(|t| t.to_ascii_lowercase().contains(&lower))
            })
            .collect()
    }

    /// Get entries by type.
    pub fn by_type(&self, kind: MarketplaceType) -> Vec<&MarketplaceEntry> {
        match kind {
            MarketplaceType::Extension => self.extensions.iter().collect(),
            MarketplaceType::Skill => self.skills.iter().collect(),
        }
    }
}

/// Extension manifest - similar to connector config with download info.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExtensionManifest {
    pub id: String,
    pub name: String,
    pub description: String,
    pub version: String,
    pub author: String,
    pub homepage: Option<String>,
    /// MCP connector configuration
    pub connector: ConnectorConfig,
    /// Optional installation notes
    pub install_notes: Option<String>,
}

/// Skill manifest - extends PluginManifest with marketplace fields.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SkillManifest {
    /// Base manifest fields (inherited from PluginManifest)
    #[serde(flatten)]
    pub base: ryoko_scripting::PluginManifest,

    /// Skill category
    pub skill_kind: SkillKind,

    /// Author information
    pub author: String,

    /// Homepage for documentation/examples
    pub homepage: Option<String>,

    /// Tags for discovery
    pub tags: Vec<String>,

    /// Entry point function name (for skill invocation)
    #[serde(default)]
    pub entry_point: Option<String>,

    /// Required capabilities (subset of agent capabilities)
    #[serde(default)]
    pub requires: Vec<String>,
}

impl SkillManifest {
    /// Create a new skill manifest for agent plugins.
    pub fn new_agent_skill(
        id: impl Into<String>,
        name: impl Into<String>,
        description: impl Into<String>,
    ) -> Self {
        Self {
            base: ryoko_scripting::PluginManifest {
                schema: ryoko_scripting::PLUGIN_SCHEMA,
                id: id.into(),
                name: name.into(),
                version: "0.1.0".into(),
                kind: ryoko_scripting::PluginKind::Agent,
                description: description.into(),
                wasm_sha256: None,
            },
            skill_kind: SkillKind::Generator,
            author: String::new(),
            homepage: None,
            tags: Vec::new(),
            entry_point: None,
            requires: Vec::new(),
        }
    }
}

/// Installation record tracking what's installed and when.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InstallationRecord {
    pub id: String,
    pub kind: MarketplaceType,
    pub version: String,
    pub installed_at: chrono::DateTime<chrono::Utc>,
    pub source_url: String,
    /// Path to the installed artifact
    pub install_path: String,
}

/// Lock file for installed marketplace items.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct MarketplaceLock {
    pub extensions: std::collections::HashMap<String, String>,
    pub skills: std::collections::HashMap<String, String>,
    pub installed: Vec<InstallationRecord>,
    pub last_checked: chrono::DateTime<chrono::Utc>,
}

/// Version comparison helper for marketplace entries.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct MarketplaceVersion {
    parts: Vec<u64>,
}

impl MarketplaceVersion {
    pub fn parse(version: &str) -> Option<Self> {
        let mut parts = Vec::new();
        for part in version.split('.') {
            match part.parse::<u64>() {
                Ok(n) => parts.push(n),
                Err(_) => return None,
            }
        }
        if parts.is_empty() {
            return None;
        }
        Some(Self { parts })
    }
}

impl std::fmt::Display for MarketplaceVersion {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        for (i, part) in self.parts.iter().enumerate() {
            if i > 0 {
                write!(f, ".")?;
            }
            write!(f, "{}", part)?;
        }
        Ok(())
    }
}

impl MarketplaceLock {
    /// Load from the lock file path.
    pub fn load() -> Result<Self, crate::MarketplaceError> {
        let path = Self::lock_file_path();
        if !path.exists() {
            return Ok(Self::default());
        }

        let content = std::fs::read_to_string(&path)
            .map_err(|e| crate::MarketplaceError::Io(format!("read lock file: {}", e)))?;
        serde_json::from_str(&content)
            .map_err(|e| crate::MarketplaceError::Parse(format!("parse lock file: {}", e)))
    }

    /// Save to the lock file path.
    pub fn save(&self) -> Result<(), crate::MarketplaceError> {
        let path = Self::lock_file_path();
        let parent = path
            .parent()
            .ok_or_else(|| crate::MarketplaceError::Io("lock file has no parent".into()))?;

        std::fs::create_dir_all(parent)
            .map_err(|e| crate::MarketplaceError::Io(format!("create lock dir: {}", e)))?;

        let content = serde_json::to_string_pretty(self)
            .map_err(|e| crate::MarketplaceError::Parse(format!("serialize lock file: {}", e)))?;

        std::fs::write(&path, content)
            .map_err(|e| crate::MarketplaceError::Io(format!("write lock file: {}", e)))?;

        Ok(())
    }

    /// Get the installed version of an item.
    pub fn installed_version(&self, id: &str, kind: MarketplaceType) -> Option<&String> {
        match kind {
            MarketplaceType::Extension => self.extensions.get(id),
            MarketplaceType::Skill => self.skills.get(id),
        }
    }

    fn lock_file_path() -> std::path::PathBuf {
        let mut path = std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."));
        path.push(crate::MARKETPLACE_LOCK_FILE);
        path
    }
}
