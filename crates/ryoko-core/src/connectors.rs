//! Secret-free connector registry for external MCP servers.
//!
//! A connector is a reviewed description of one external MCP server: how to
//! start it, which environment variables its credentials come from, and exactly
//! which tools the host allows. The registry never stores a secret value, and an
//! entry without an allowlist mounts nothing.
//!
//! Mounting, namespacing, approval and budgets stay in [`crate::mcp`]; the CLI
//! owns the process transport. This module only parses, validates and reports.
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// Workspace-relative location of the connector registry.
pub const CONNECTORS_PATH: &str = ".ryoko/connectors.json";
/// Schema version written into every registry.
pub const CONNECTORS_VERSION: u32 = 1;
/// Tool-call cap used when a connector does not set `max_calls_per_run`.
pub const DEFAULT_CONNECTOR_MAX_CALLS: u32 = 25;
/// Maximum number of connectors in one registry.
pub const MAX_CONNECTORS: usize = 32;
/// Maximum number of tools one connector may allow.
pub const MAX_CONNECTOR_TOOLS: usize = 64;
/// Maximum number of environment variables one connector may declare.
pub const MAX_CONNECTOR_ENV: usize = 16;
const MAX_NAME: usize = 80;
const MAX_ARGUMENTS: usize = 64;
const MAX_TOOL_NAME: usize = 128;
const NAMESPACE_SEPARATOR: &str = "__";

#[derive(Debug, thiserror::Error)]
pub enum ConnectorError {
    #[error("invalid connector configuration: {0}")]
    Invalid(String),
    #[error("connector registry I/O error: {0}")]
    Io(String),
    #[error("connector `{0}` is not configured")]
    NotFound(String),
    #[error("connector `{0}` is already configured")]
    Duplicate(String),
    #[error("connector `{0}` is missing required environment variable(s): {1}")]
    MissingEnv(String, String),
}

/// How the host reaches the MCP server.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ConnectorTransport {
    /// A reviewed local executable plus its arguments.
    Stdio {
        command: String,
        #[serde(default)]
        args: Vec<String>,
    },
    /// Server-sent events endpoint. HTTPS outside loopback, like provider URLs.
    Sse { url: String },
    /// Streamable HTTP endpoint. HTTPS outside loopback, like provider URLs.
    StreamableHttp { url: String },
}

impl ConnectorTransport {
    pub fn kind(&self) -> &'static str {
        match self {
            Self::Stdio { .. } => "stdio",
            Self::Sse { .. } => "sse",
            Self::StreamableHttp { .. } => "streamable_http",
        }
    }

    /// Endpoint URL for the network transports.
    pub fn url(&self) -> Option<&str> {
        match self {
            Self::Stdio { .. } => None,
            Self::Sse { url } | Self::StreamableHttp { url } => Some(url),
        }
    }

    fn validate(&self) -> Result<(), ConnectorError> {
        match self {
            Self::Stdio { command, args } => {
                if command.trim().is_empty() || command.len() > 512 {
                    return Err(ConnectorError::Invalid(
                        "stdio transport needs a non-empty command".into(),
                    ));
                }
                if command.chars().any(char::is_control) {
                    return Err(ConnectorError::Invalid(
                        "stdio command contains control characters".into(),
                    ));
                }
                if args.len() > MAX_ARGUMENTS {
                    return Err(ConnectorError::Invalid(format!(
                        "stdio transport allows at most {MAX_ARGUMENTS} arguments"
                    )));
                }
                for argument in args {
                    if argument.len() > 4096 || argument.chars().any(char::is_control) {
                        return Err(ConnectorError::Invalid(
                            "stdio argument is too long or contains control characters".into(),
                        ));
                    }
                }
                Ok(())
            }
            Self::Sse { url } | Self::StreamableHttp { url } => validate_url(url),
        }
    }
}

/// Mirrors the provider rule: HTTPS everywhere, HTTP only on loopback, and
/// never a credential embedded in the URL. A connector URL ends up in a file a
/// human reviews, so it must not be able to carry a token.
fn validate_url(value: &str) -> Result<(), ConnectorError> {
    let Some((scheme, authority)) = split_authority(value) else {
        return Err(ConnectorError::Invalid(format!(
            "connector URL must be HTTP(S) without credentials, query or fragment: {value}"
        )));
    };
    if scheme != "https" && !(scheme == "http" && is_loopback(&authority)) {
        return Err(ConnectorError::Invalid(
            "connector URL must use HTTPS outside loopback".into(),
        ));
    }
    Ok(())
}

/// Splits `scheme://host[:port][/path]` after rejecting userinfo, queries and
/// fragments. The scheme and host are returned lowercased for comparison.
fn split_authority(value: &str) -> Option<(String, String)> {
    if value.len() > 2048 || value.chars().any(char::is_control) {
        return None;
    }
    let (scheme, rest) = value.split_once("://")?;
    if !matches!(scheme, "http" | "https") || rest.contains(['?', '#', '@', '\\']) {
        return None;
    }
    let authority = rest.split('/').next()?;
    if authority.is_empty() {
        return None;
    }
    let host = if let Some(bracketed) = authority.strip_prefix('[') {
        let (address, remainder) = bracketed.split_once(']')?;
        if !remainder.is_empty() && !remainder.starts_with(':') {
            return None;
        }
        if remainder.trim_start_matches(':').contains(':') {
            return None;
        }
        address
    } else {
        let host = authority.split(':').next()?;
        if authority.matches(':').count() > 1 {
            return None;
        }
        host
    };
    if host.is_empty() || host.chars().any(char::is_whitespace) {
        return None;
    }
    Some((scheme.to_owned(), host.to_owned()))
}

fn is_loopback(host: &str) -> bool {
    host.eq_ignore_ascii_case("localhost")
        || host
            .trim_matches(|ch| ch == '[' || ch == ']')
            .parse::<std::net::IpAddr>()
            .is_ok_and(|ip| ip.is_loopback())
}

/// One connector entry. Field names are the on-disk contract.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConnectorConfig {
    /// Stable identifier, also the tool namespace prefix.
    pub id: String,
    /// Display name shown by the CLI and desktop catalog.
    pub name: String,
    /// Display category such as `"Source Control"` or `"Database"`.
    pub category: String,
    /// One-line explanation shown before enabling the connector.
    #[serde(default)]
    pub description: String,
    pub transport: ConnectorTransport,
    /// Credential environment variable names, never their values.
    #[serde(default)]
    pub required_env: Vec<String>,
    /// Tools this connector may call. Empty means nothing may be called.
    #[serde(default)]
    pub allowed_tools: Vec<String>,
    /// Tools known to mutate remote state. They are tagged in the catalog and
    /// must be added to `allowed_tools` before they can run.
    #[serde(default)]
    pub write_tools: Vec<String>,
    #[serde(default)]
    pub enabled: bool,
    /// Per-connector tool-call cap for one run.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_calls_per_run: Option<u32>,
}

impl ConnectorConfig {
    /// Validates one entry without touching the environment or the network.
    pub fn validate(&self) -> Result<(), ConnectorError> {
        if !valid_connector_id(&self.id) {
            return Err(ConnectorError::Invalid(format!(
                "id `{}` must be 1-64 lowercase letters, digits, '-' or '_' and must not contain `__`",
                self.id
            )));
        }
        for (label, value) in [("name", &self.name), ("category", &self.category)] {
            if value.trim().is_empty() || value.len() > MAX_NAME {
                return Err(ConnectorError::Invalid(format!(
                    "connector `{}` needs a non-empty {label} under {MAX_NAME} bytes",
                    self.id
                )));
            }
            if value.chars().any(char::is_control) {
                return Err(ConnectorError::Invalid(format!(
                    "connector `{}` {label} contains control characters",
                    self.id
                )));
            }
        }
        if self.description.len() > 1024 || self.description.chars().any(char::is_control) {
            return Err(ConnectorError::Invalid(format!(
                "connector `{}` description is too long or contains control characters",
                self.id
            )));
        }
        self.transport.validate().map_err(|error| match error {
            ConnectorError::Invalid(message) => {
                ConnectorError::Invalid(format!("connector `{}`: {message}", self.id))
            }
            other => other,
        })?;
        if self.required_env.len() > MAX_CONNECTOR_ENV {
            return Err(ConnectorError::Invalid(format!(
                "connector `{}` declares more than {MAX_CONNECTOR_ENV} environment variables",
                self.id
            )));
        }
        let mut env_names = std::collections::BTreeSet::new();
        for name in &self.required_env {
            if !valid_env_name(name) {
                return Err(ConnectorError::Invalid(format!(
                    "connector `{}` has an invalid environment variable name `{name}`",
                    self.id
                )));
            }
            if !env_names.insert(name) {
                return Err(ConnectorError::Invalid(format!(
                    "connector `{}` repeats environment variable `{name}`",
                    self.id
                )));
            }
        }
        validate_tools(&self.id, &self.allowed_tools, "allowed_tools")?;
        validate_tools(&self.id, &self.write_tools, "write_tools")?;
        if let Some(limit) = self.max_calls_per_run
            && limit == 0
        {
            return Err(ConnectorError::Invalid(format!(
                "connector `{}` max_calls_per_run must be greater than zero",
                self.id
            )));
        }
        Ok(())
    }

    /// Tool-call cap used when the connector does not override it.
    pub fn call_limit(&self) -> u64 {
        u64::from(
            self.max_calls_per_run
                .unwrap_or(DEFAULT_CONNECTOR_MAX_CALLS),
        )
    }

    pub fn is_write_tool(&self, tool: &str) -> bool {
        self.write_tools.iter().any(|entry| entry == tool)
    }

    /// Every tool the connector advertises to a configuration UI.
    pub fn catalog(&self) -> Vec<ConnectorTool> {
        let mut tools = Vec::new();
        for tool in self.allowed_tools.iter().chain(self.write_tools.iter()) {
            if tools
                .iter()
                .any(|entry: &ConnectorTool| &entry.name == tool)
            {
                continue;
            }
            tools.push(ConnectorTool {
                name: tool.clone(),
                write: self.is_write_tool(tool),
                allowed: self.allowed_tools.contains(tool),
            });
        }
        tools
    }
}

fn validate_tools(id: &str, tools: &[String], field: &str) -> Result<(), ConnectorError> {
    if tools.len() > MAX_CONNECTOR_TOOLS {
        return Err(ConnectorError::Invalid(format!(
            "connector `{id}` lists more than {MAX_CONNECTOR_TOOLS} tools in {field}"
        )));
    }
    let mut seen = std::collections::BTreeSet::new();
    for tool in tools {
        if tool.is_empty()
            || tool.len() > MAX_TOOL_NAME
            || tool.chars().any(char::is_control)
            || tool.chars().any(char::is_whitespace)
        {
            return Err(ConnectorError::Invalid(format!(
                "connector `{id}` has an invalid tool name in {field}"
            )));
        }
        if !seen.insert(tool) {
            return Err(ConnectorError::Invalid(format!(
                "connector `{id}` repeats tool `{tool}` in {field}"
            )));
        }
    }
    Ok(())
}

/// One tool offered by a connector, tagged for a configuration UI.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConnectorTool {
    pub name: String,
    /// Mutates remote state. Enabling it is a deliberate, reviewed choice.
    pub write: bool,
    /// Currently in the connector's allowlist.
    pub allowed: bool,
}

pub fn valid_connector_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 64
        && !id.contains(NAMESPACE_SEPARATOR)
        && id
            .bytes()
            .next()
            .is_some_and(|byte| byte.is_ascii_lowercase())
        && id.bytes().all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-' || byte == b'_'
        })
}

/// Environment names are passed to child processes, so they follow the CLI's
/// existing `--allow-env` rule exactly.
pub fn valid_env_name(name: &str) -> bool {
    let mut bytes = name.bytes();
    matches!(bytes.next(), Some(b'A'..=b'Z' | b'_'))
        && bytes.all(|byte| byte.is_ascii_uppercase() || byte.is_ascii_digit() || byte == b'_')
}

/// Environment passed to one connector child process.
///
/// Only the names the connector declares are read, and a missing value is an
/// error rather than an empty string the server would treat as configured.
pub fn resolve_required_env(
    id: &str,
    required: &[String],
    lookup: impl Fn(&str) -> Option<String>,
) -> Result<BTreeMap<String, String>, ConnectorError> {
    let mut resolved = BTreeMap::new();
    let mut missing = Vec::new();
    for name in required {
        if !valid_env_name(name) {
            return Err(ConnectorError::Invalid(format!(
                "connector `{id}` declares an invalid environment variable `{name}`"
            )));
        }
        match lookup(name) {
            Some(value) if !value.trim().is_empty() => {
                resolved.insert(name.clone(), value);
            }
            _ => missing.push(name.clone()),
        }
    }
    if !missing.is_empty() {
        return Err(ConnectorError::MissingEnv(
            id.to_owned(),
            missing.join(", "),
        ));
    }
    Ok(resolved)
}

/// Offline readiness of one connector.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConnectorStatus {
    /// Enabled with an allowlist and every credential variable set.
    Ready,
    /// Configured but switched off.
    Disabled,
    /// Enabled, but at least one declared variable is unset or empty.
    MissingEnv,
    /// Enabled, but nothing may be called until tools are chosen.
    NoTools,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConnectorReadiness {
    pub status: ConnectorStatus,
    /// Declared variables that are unset or empty, sorted as declared.
    pub missing_env: Vec<String>,
}

impl ConnectorReadiness {
    pub fn ready(&self) -> bool {
        self.status == ConnectorStatus::Ready
    }
}

/// Offline check, in the same spirit as `ryoko doctor`: it reads environment
/// variable presence only and never spawns the server.
pub fn readiness(
    config: &ConnectorConfig,
    lookup: impl Fn(&str) -> Option<String>,
) -> ConnectorReadiness {
    let missing_env = config
        .required_env
        .iter()
        .filter(|name| lookup(name).is_none_or(|value| value.trim().is_empty()))
        .cloned()
        .collect::<Vec<_>>();
    let status = if !config.enabled {
        ConnectorStatus::Disabled
    } else if !missing_env.is_empty() {
        ConnectorStatus::MissingEnv
    } else if config.allowed_tools.is_empty() {
        ConnectorStatus::NoTools
    } else {
        ConnectorStatus::Ready
    };
    ConnectorReadiness {
        status,
        missing_env,
    }
}

/// A connector file: a version plus validated entries.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConnectorRegistry {
    #[serde(default = "default_version")]
    pub version: u32,
    #[serde(default)]
    pub connectors: Vec<ConnectorConfig>,
}

fn default_version() -> u32 {
    CONNECTORS_VERSION
}

impl Default for ConnectorRegistry {
    fn default() -> Self {
        Self {
            version: CONNECTORS_VERSION,
            connectors: Vec::new(),
        }
    }
}

impl ConnectorRegistry {
    /// Path of the workspace registry for `root`.
    pub fn path(root: &Path) -> PathBuf {
        root.join(CONNECTORS_PATH)
    }

    /// Loads the registry at `root/.ryoko/connectors.json`, treating a missing
    /// file as an empty registry. A malformed file is an error, never ignored.
    pub fn load(root: &Path) -> Result<Self, ConnectorError> {
        let path = Self::path(root);
        if !path.exists() {
            return Ok(Self::default());
        }
        let text = std::fs::read_to_string(&path)
            .map_err(|error| ConnectorError::Io(format!("{}: {error}", path.display())))?;
        Self::from_json(&text)
    }

    pub fn from_json(text: &str) -> Result<Self, ConnectorError> {
        let registry: Self = serde_json::from_str(text)
            .map_err(|error| ConnectorError::Invalid(error.to_string()))?;
        registry.validate()?;
        Ok(registry)
    }

    pub fn to_json_pretty(&self) -> Result<String, ConnectorError> {
        let mut text = serde_json::to_string_pretty(self)
            .map_err(|error| ConnectorError::Invalid(error.to_string()))?;
        text.push('\n');
        Ok(text)
    }

    pub fn validate(&self) -> Result<(), ConnectorError> {
        if self.version != CONNECTORS_VERSION {
            return Err(ConnectorError::Invalid(format!(
                "unsupported connector registry version {} (expected {CONNECTORS_VERSION})",
                self.version
            )));
        }
        if self.connectors.len() > MAX_CONNECTORS {
            return Err(ConnectorError::Invalid(format!(
                "a registry holds at most {MAX_CONNECTORS} connectors"
            )));
        }
        let mut ids = std::collections::BTreeSet::new();
        for connector in &self.connectors {
            connector.validate()?;
            if !ids.insert(connector.id.as_str()) {
                return Err(ConnectorError::Invalid(format!(
                    "duplicate connector id `{}`",
                    connector.id
                )));
            }
        }
        Ok(())
    }

    pub fn get(&self, id: &str) -> Option<&ConnectorConfig> {
        self.connectors.iter().find(|config| config.id == id)
    }

    pub fn get_mut(&mut self, id: &str) -> Option<&mut ConnectorConfig> {
        self.connectors.iter_mut().find(|config| config.id == id)
    }

    /// Enabled connectors, in file order.
    pub fn enabled(&self) -> Vec<&ConnectorConfig> {
        self.connectors
            .iter()
            .filter(|config| config.enabled)
            .collect()
    }

    /// Adds one connector, refusing an id that already exists.
    pub fn add(&mut self, connector: ConnectorConfig) -> Result<(), ConnectorError> {
        connector.validate()?;
        if self.get(&connector.id).is_some() {
            return Err(ConnectorError::Duplicate(connector.id.clone()));
        }
        if self.connectors.len() >= MAX_CONNECTORS {
            return Err(ConnectorError::Invalid(format!(
                "a registry holds at most {MAX_CONNECTORS} connectors"
            )));
        }
        self.connectors.push(connector);
        Ok(())
    }

    /// Replaces the entry with the same id, keeping file order.
    pub fn upsert(&mut self, connector: ConnectorConfig) -> Result<(), ConnectorError> {
        connector.validate()?;
        match self.get_mut(&connector.id) {
            Some(existing) => *existing = connector,
            None => self.add(connector)?,
        }
        Ok(())
    }

    pub fn set_enabled(&mut self, id: &str, enabled: bool) -> Result<(), ConnectorError> {
        let connector = self
            .get_mut(id)
            .ok_or_else(|| ConnectorError::NotFound(id.to_owned()))?;
        connector.enabled = enabled;
        Ok(())
    }

    /// Replaces the allowlist of one connector, keeping each name once.
    pub fn set_allowed_tools(
        &mut self,
        id: &str,
        tools: impl IntoIterator<Item = String>,
    ) -> Result<(), ConnectorError> {
        let connector = self
            .get_mut(id)
            .ok_or_else(|| ConnectorError::NotFound(id.to_owned()))?;
        let mut allowed = Vec::new();
        for tool in tools {
            if !allowed.contains(&tool) {
                allowed.push(tool);
            }
        }
        connector.allowed_tools = allowed;
        connector.validate()
    }

    /// Writes the registry atomically beneath `<root>/.ryoko/`.
    pub fn save(&self, root: &Path) -> Result<PathBuf, ConnectorError> {
        self.save_to(&Self::path(root))
    }

    /// Writes the registry atomically to `path`.
    ///
    /// The directory and the file are inspected for symlinks first so a
    /// repository cannot redirect the write outside the workspace.
    pub fn save_to(&self, path: &Path) -> Result<PathBuf, ConnectorError> {
        self.validate()?;
        let path = path.to_path_buf();
        let directory = path
            .parent()
            .ok_or_else(|| ConnectorError::Io("connector path has no parent".into()))?;
        std::fs::create_dir_all(directory)
            .map_err(|error| ConnectorError::Io(format!("{}: {error}", directory.display())))?;
        for candidate in [directory, path.as_path()] {
            if std::fs::symlink_metadata(candidate)
                .map(|metadata| metadata.file_type().is_symlink())
                .unwrap_or(false)
            {
                return Err(ConnectorError::Io(format!(
                    "refusing to write through a symlink: {}",
                    candidate.display()
                )));
            }
        }
        let staged = path.with_extension("json.tmp");
        std::fs::write(&staged, self.to_json_pretty()?)
            .map_err(|error| ConnectorError::Io(format!("{}: {error}", staged.display())))?;
        std::fs::rename(&staged, &path)
            .map_err(|error| ConnectorError::Io(format!("{}: {error}", path.display())))?;
        Ok(path)
    }
}

/// Built-in presets shipped with the repository. They are disabled templates;
/// a preset becomes a connector only when it is copied into a registry.
pub fn builtin_presets() -> ConnectorRegistry {
    ConnectorRegistry::from_json(include_str!("../../../configs/connectors.example.json"))
        .expect("the built-in connector catalogue is validated by its tests")
}

/// Finds one built-in preset by id.
pub fn builtin_preset(id: &str) -> Option<ConnectorConfig> {
    builtin_presets()
        .connectors
        .into_iter()
        .find(|connector| connector.id == id)
}

/// Connector display categories used by the example catalogue.
pub const CATEGORIES: &[&str] = &[
    "Source Control",
    "Cloud & Infra",
    "Project Management",
    "Database",
    "Workspace",
    "Custom",
];

#[cfg(test)]
mod tests {
    use super::*;

    fn github() -> ConnectorConfig {
        builtin_preset("github").expect("github preset")
    }

    #[test]
    fn the_shipped_catalogue_is_valid_and_disabled() {
        let registry = builtin_presets();
        assert_eq!(registry.version, CONNECTORS_VERSION);
        let ids = registry
            .connectors
            .iter()
            .map(|connector| connector.id.as_str())
            .collect::<Vec<_>>();
        assert_eq!(
            ids,
            vec!["github", "cloudflare", "filesystem", "fetch", "custom"]
        );
        assert!(
            registry
                .connectors
                .iter()
                .all(|connector| !connector.enabled),
            "a shipped preset must never arrive enabled"
        );
        for connector in &registry.connectors {
            assert!(
                !connector.category.is_empty(),
                "{} needs a category",
                connector.id
            );
            assert!(
                CATEGORIES.contains(&connector.category.as_str()),
                "{} has an unknown category",
                connector.id
            );
            assert!(
                !connector
                    .required_env
                    .iter()
                    .any(|name| name.contains("TOKEN=")),
                "presets hold variable names, never values"
            );
        }
    }

    #[test]
    fn presets_default_to_read_only_tools_with_write_tools_tagged() {
        let github = github();
        for write in &github.write_tools {
            assert!(
                !github.allowed_tools.contains(write),
                "`{write}` must not be allowed by default"
            );
        }
        assert!(github.is_write_tool("create_issue"));
        assert!(!github.is_write_tool("list_issues"));
        let catalog = github.catalog();
        let create = catalog
            .iter()
            .find(|tool| tool.name == "create_issue")
            .expect("write tool is listed");
        assert!(create.write);
        assert!(!create.allowed);
        let list = catalog
            .iter()
            .find(|tool| tool.name == "list_issues")
            .expect("read tool is listed");
        assert!(!list.write);
        assert!(list.allowed);
    }

    #[test]
    fn a_registry_round_trips_through_json() {
        let mut registry = ConnectorRegistry::default();
        registry.add(github()).unwrap();
        let text = registry.to_json_pretty().unwrap();
        let parsed = ConnectorRegistry::from_json(&text).unwrap();
        assert_eq!(parsed, registry);
        assert!(text.contains("GITHUB_PERSONAL_ACCESS_TOKEN"));
        assert!(!text.contains("ghp_"));
    }

    #[test]
    fn unknown_fields_are_ignored_but_bad_values_are_not() {
        let text = r#"{
            "version": 1,
            "connectors": [{
                "id": "github",
                "name": "GitHub",
                "category": "Source Control",
                "transport": {"kind": "stdio", "command": "npx", "args": ["-y", "server"]},
                "required_env": ["GITHUB_PERSONAL_ACCESS_TOKEN"],
                "allowed_tools": ["list_issues"],
                "write_tools": ["create_issue"],
                "enabled": true
            }]
        }"#;
        let registry = ConnectorRegistry::from_json(text).unwrap();
        assert_eq!(registry.connectors[0].call_limit(), 25);
        assert!(registry.connectors[0].enabled);

        let duplicated = text.replace(
            "\"allowed_tools\": [\"list_issues\"]",
            "\"allowed_tools\": [\"list_issues\", \"list_issues\"]",
        );
        assert!(ConnectorRegistry::from_json(&duplicated).is_err());
    }

    #[test]
    fn connector_ids_must_be_namespace_safe() {
        for id in ["github", "my-connector", "db_2"] {
            assert!(valid_connector_id(id), "{id}");
        }
        for id in [
            "",
            "GitHub",
            "github__tools",
            "1github",
            "git hub",
            "git.hub",
        ] {
            assert!(!valid_connector_id(id), "{id}");
        }
        let mut connector = github();
        connector.id = "github__tools".into();
        assert!(connector.validate().is_err());
    }

    #[test]
    fn environment_names_are_validated_and_never_values() {
        for name in ["GITHUB_PERSONAL_ACCESS_TOKEN", "_X", "A1"] {
            assert!(valid_env_name(name), "{name}");
        }
        for name in ["", "github_token", "PATH=/tmp", "A-B", "1TOKEN"] {
            assert!(!valid_env_name(name), "{name}");
        }
        let mut connector = github();
        connector.required_env = vec!["github_token".into()];
        assert!(connector.validate().is_err());
    }

    #[test]
    fn only_declared_variables_are_resolved_for_a_child() {
        let required = vec!["GITHUB_PERSONAL_ACCESS_TOKEN".to_string()];
        let resolved = resolve_required_env("github", &required, |name| {
            assert_eq!(name, "GITHUB_PERSONAL_ACCESS_TOKEN");
            Some("token-value".to_string())
        })
        .unwrap();
        assert_eq!(resolved.len(), 1);
        assert!(resolved.contains_key("GITHUB_PERSONAL_ACCESS_TOKEN"));
        // The host environment is not consulted for anything else.
        assert!(!resolved.contains_key("HOME"));
        assert!(!resolved.contains_key("AWS_SECRET_ACCESS_KEY"));
    }

    #[test]
    fn a_missing_or_empty_variable_fails_closed_and_names_itself() {
        let required = vec![
            "CLOUDFLARE_API_TOKEN".to_string(),
            "CLOUDFLARE_ACCOUNT_ID".to_string(),
        ];
        let error = resolve_required_env("cloudflare", &required, |name| {
            (name == "CLOUDFLARE_ACCOUNT_ID").then(|| "  ".to_string())
        })
        .expect_err("an empty value is not a configured credential");
        let message = error.to_string();
        assert!(message.contains("CLOUDFLARE_API_TOKEN"), "{message}");
        assert!(message.contains("CLOUDFLARE_ACCOUNT_ID"), "{message}");
    }

    #[test]
    fn readiness_reports_disabled_missing_and_ready() {
        let mut connector = github();
        let disabled = readiness(&connector, |_| Some("set".into()));
        assert_eq!(disabled.status, ConnectorStatus::Disabled);
        assert!(disabled.missing_env.is_empty());

        connector.enabled = true;
        let missing = readiness(&connector, |_| None);
        assert_eq!(missing.status, ConnectorStatus::MissingEnv);
        assert_eq!(missing.missing_env, vec!["GITHUB_PERSONAL_ACCESS_TOKEN"]);

        let ready = readiness(&connector, |_| Some("set".into()));
        assert_eq!(ready.status, ConnectorStatus::Ready);
        assert!(ready.ready());

        connector.allowed_tools.clear();
        let no_tools = readiness(&connector, |_| Some("set".into()));
        assert_eq!(no_tools.status, ConnectorStatus::NoTools);
    }

    #[test]
    fn transport_urls_require_https_outside_loopback_and_hold_no_secrets() {
        let mut connector = github();
        connector.transport = ConnectorTransport::Sse {
            url: "http://127.0.0.1:8787/sse".into(),
        };
        connector.validate().unwrap();

        connector.transport = ConnectorTransport::StreamableHttp {
            url: "https://mcp.example.com/mcp".into(),
        };
        connector.validate().unwrap();

        for rejected in [
            "http://mcp.example.com/sse",
            "https://user:pass@mcp.example.com/sse",
            "https://mcp.example.com/sse?token=abc",
            "https://mcp.example.com/sse#frag",
            "file:///etc/passwd",
        ] {
            connector.transport = ConnectorTransport::Sse {
                url: rejected.into(),
            };
            assert!(connector.validate().is_err(), "{rejected} must be rejected");
        }
    }

    #[test]
    fn stdio_transport_bounds_command_and_arguments() {
        let mut connector = github();
        connector.transport = ConnectorTransport::Stdio {
            command: "  ".into(),
            args: Vec::new(),
        };
        assert!(connector.validate().is_err());

        connector.transport = ConnectorTransport::Stdio {
            command: "npx".into(),
            args: vec!["a".into(); MAX_ARGUMENTS + 1],
        };
        assert!(connector.validate().is_err());

        connector.transport = ConnectorTransport::Stdio {
            command: "npx".into(),
            args: vec!["-y".into(), "server".into()],
        };
        connector.validate().unwrap();
    }

    #[test]
    fn a_zero_call_budget_is_rejected() {
        let mut connector = github();
        connector.max_calls_per_run = Some(0);
        assert!(connector.validate().is_err());
        connector.max_calls_per_run = Some(7);
        assert_eq!(connector.call_limit(), 7);
    }

    #[test]
    fn duplicate_ids_are_rejected() {
        let mut registry = ConnectorRegistry::default();
        registry.add(github()).unwrap();
        assert!(matches!(
            registry.add(github()),
            Err(ConnectorError::Duplicate(id)) if id == "github"
        ));
    }

    #[test]
    fn enabling_and_tool_selection_are_bounded() {
        let mut registry = ConnectorRegistry::default();
        registry.add(github()).unwrap();
        registry.set_enabled("github", true).unwrap();
        assert_eq!(registry.enabled().len(), 1);

        registry
            .set_allowed_tools("github", ["list_issues".into(), "list_issues".into()])
            .unwrap();
        assert_eq!(registry.connectors[0].allowed_tools, vec!["list_issues"]);

        assert!(matches!(
            registry.set_enabled("missing", true),
            Err(ConnectorError::NotFound(_))
        ));
    }

    #[test]
    fn saving_creates_the_registry_under_the_workspace() {
        let dir = tempfile::tempdir().unwrap();
        let mut registry = ConnectorRegistry::default();
        registry.add(github()).unwrap();
        let path = registry.save(dir.path()).unwrap();
        assert_eq!(path, dir.path().join(CONNECTORS_PATH));
        assert_eq!(ConnectorRegistry::load(dir.path()).unwrap(), registry);
    }

    #[test]
    fn a_missing_file_is_an_empty_registry_but_a_broken_file_is_an_error() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(
            ConnectorRegistry::load(dir.path()).unwrap(),
            ConnectorRegistry::default()
        );
        std::fs::create_dir_all(dir.path().join(".ryoko")).unwrap();
        std::fs::write(ConnectorRegistry::path(dir.path()), "{not json").unwrap();
        assert!(ConnectorRegistry::load(dir.path()).is_err());
    }

    #[test]
    fn a_future_schema_version_is_refused() {
        let text = r#"{"version": 99, "connectors": []}"#;
        assert!(ConnectorRegistry::from_json(text).is_err());
    }
}
