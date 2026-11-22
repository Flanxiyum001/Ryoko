//! `ryoko connectors`: inspect, scaffold and toggle the workspace registry.
//!
//! The registry holds environment variable *names*, never values. Commands here
//! only read that file and the presence of environment variables; they never
//! spawn a connector server.
use std::error::Error;
use std::path::{Path, PathBuf};

use ryoko_core::ConnectorConfig;
use ryoko_core::connectors::{
    ConnectorRegistry, ConnectorStatus, ConnectorTransport, builtin_preset, builtin_presets,
    readiness,
};

use crate::cli_error;

/// Connector registry location for one command.
pub struct RegistryLocation {
    pub path: PathBuf,
}

impl RegistryLocation {
    /// Resolves `--repo` plus an optional `--connectors-config` override.
    pub fn resolve(repo: &Path, config: Option<&Path>) -> Result<Self, Box<dyn Error>> {
        let root = repo
            .canonicalize()
            .map_err(|error| cli_error(format!("cannot open {}: {error}", repo.display())))?;
        if !root.is_dir() {
            return Err(cli_error(format!("{} is not a directory", root.display())));
        }
        let path = match config {
            Some(path) if path.is_absolute() => path.to_path_buf(),
            Some(path) => root.join(path),
            None => ConnectorRegistry::path(&root),
        };
        Ok(Self { path })
    }

    pub fn load(&self) -> Result<ConnectorRegistry, Box<dyn Error>> {
        if !self.path.exists() {
            return Ok(ConnectorRegistry::default());
        }
        let text = std::fs::read_to_string(&self.path)?;
        Ok(ConnectorRegistry::from_json(&text)?)
    }

    pub fn save(&self, registry: &ConnectorRegistry) -> Result<(), Box<dyn Error>> {
        registry.save_to(&self.path)?;
        Ok(())
    }
}

fn env_lookup(name: &str) -> Option<String> {
    std::env::var(name).ok()
}

fn transport_label(transport: &ConnectorTransport) -> String {
    match transport {
        ConnectorTransport::Stdio { command, args } => {
            let mut label = command.clone();
            for argument in args {
                label.push(' ');
                label.push_str(argument);
            }
            format!("stdio {label}")
        }
        ConnectorTransport::Sse { url } => format!("sse {url}"),
        ConnectorTransport::StreamableHttp { url } => format!("streamable_http {url}"),
    }
}

/// Prints presets and configured connectors with offline readiness.
pub fn list(location: &RegistryLocation, json: bool) -> Result<(), Box<dyn Error>> {
    let registry = location.load()?;
    if json {
        let connectors = registry
            .connectors
            .iter()
            .map(|connector| {
                let readiness = readiness(connector, env_lookup);
                serde_json::json!({
                    "connector": connector,
                    "readiness": readiness,
                })
            })
            .collect::<Vec<_>>();
        let payload = serde_json::json!({
            "path": location.path,
            "presets": builtin_presets().connectors,
            "connectors": connectors,
        });
        println!("{}", serde_json::to_string_pretty(&payload)?);
        return Ok(());
    }

    println!("Built-in connector presets:");
    for preset in builtin_presets().connectors {
        println!(
            "  {:<12} {:<16} {} ({} read / {} write tools)",
            preset.id,
            preset.category,
            preset.name,
            preset.allowed_tools.len(),
            preset.write_tools.len()
        );
    }
    println!();
    if registry.connectors.is_empty() {
        println!(
            "No connectors configured at {}. Start with: ryoko connectors add github",
            location.path.display()
        );
        return Ok(());
    }
    println!("Configured connectors ({}):", location.path.display());
    for connector in &registry.connectors {
        let readiness = readiness(connector, env_lookup);
        let enabled = if connector.enabled {
            "enabled "
        } else {
            "disabled"
        };
        println!(
            "  {:<12} {:<9} {:<9} {}",
            connector.id,
            enabled,
            match readiness.status {
                ConnectorStatus::Ready => "ready",
                ConnectorStatus::Disabled => "off",
                ConnectorStatus::MissingEnv => "needs env",
                ConnectorStatus::NoTools => "no tools",
            },
            connector.name
        );
        println!("    {}", transport_label(&connector.transport));
        println!(
            "    {} allowed tool(s), {} write tool(s), max {} call(s) per run",
            connector.allowed_tools.len(),
            connector.write_tools.len(),
            connector.call_limit()
        );
        if !connector.required_env.is_empty() {
            let described = connector
                .required_env
                .iter()
                .map(|name| {
                    let state = if readiness.missing_env.iter().any(|missing| missing == name) {
                        "MISSING"
                    } else {
                        "set"
                    };
                    format!("{name}: {state}")
                })
                .collect::<Vec<_>>()
                .join(", ");
            println!("    {described}");
        }
        if connector.enabled && readiness.status == ConnectorStatus::NoTools {
            println!(
                "    allow at least one tool before running: ryoko connectors enable {}",
                connector.id
            );
        }
    }
    let ready = registry
        .connectors
        .iter()
        .filter(|connector| readiness(connector, env_lookup).ready())
        .count();
    println!();
    println!(
        "Offline check only: {ready} of {} connector(s) ready; no server was started.",
        registry.connectors.len()
    );
    Ok(())
}

/// Options accepted by `ryoko connectors add`.
pub struct AddOptions {
    pub preset: String,
    pub id: Option<String>,
    pub display_name: Option<String>,
    pub category: Option<String>,
    pub command: Option<String>,
    pub args: Vec<String>,
    pub url: Option<String>,
    pub transport: Option<String>,
    pub env: Vec<String>,
    pub allow_tools: Vec<String>,
    pub max_calls: Option<u32>,
    pub replace: bool,
}

impl Default for AddOptions {
    fn default() -> Self {
        Self {
            preset: "custom".into(),
            id: None,
            display_name: None,
            category: None,
            command: None,
            args: Vec::new(),
            url: None,
            transport: None,
            env: Vec::new(),
            allow_tools: Vec::new(),
            max_calls: None,
            replace: false,
        }
    }
}

fn scaffold(options: &AddOptions) -> Result<ConnectorConfig, Box<dyn Error>> {
    let mut connector = builtin_preset(&options.preset).ok_or_else(|| {
        let ids = builtin_presets()
            .connectors
            .iter()
            .map(|preset| preset.id.as_str())
            .collect::<Vec<_>>()
            .join(", ");
        cli_error(format!(
            "unknown preset `{}` (choose one of: {ids})",
            options.preset
        ))
    })?;
    if let Some(id) = &options.id {
        connector.id = id.clone();
    }
    if let Some(name) = &options.display_name {
        connector.name = name.clone();
    }
    if let Some(category) = &options.category {
        connector.category = category.clone();
    }
    match (&options.command, &options.url) {
        (Some(_), Some(_)) => {
            return Err(cli_error("choose either --command or --url, not both"));
        }
        (Some(command), None) => {
            connector.transport = ConnectorTransport::Stdio {
                command: command.clone(),
                args: options.args.clone(),
            };
        }
        (None, Some(url)) => {
            connector.transport = match options.transport.as_deref().unwrap_or("sse") {
                "sse" => ConnectorTransport::Sse { url: url.clone() },
                "streamable-http" | "streamable_http" => {
                    ConnectorTransport::StreamableHttp { url: url.clone() }
                }
                other => {
                    return Err(cli_error(format!(
                        "unknown --transport `{other}` (use sse or streamable-http)"
                    )));
                }
            };
        }
        (None, None) => {
            if !options.args.is_empty() {
                return Err(cli_error("--arg requires --command"));
            }
        }
    }
    if !options.env.is_empty() {
        connector.required_env = options.env.clone();
    }
    if !options.allow_tools.is_empty() {
        connector.allowed_tools = options.allow_tools.clone();
    }
    if let Some(max_calls) = options.max_calls {
        connector.max_calls_per_run = Some(max_calls);
    }
    connector.enabled = false;
    connector.validate()?;
    Ok(connector)
}

/// Copies a preset (with optional overrides) into the workspace registry.
pub fn add(location: &RegistryLocation, options: &AddOptions) -> Result<(), Box<dyn Error>> {
    let connector = scaffold(options)?;
    let mut registry = location.load()?;
    let existing = registry.get(&connector.id).is_some();
    if existing && !options.replace {
        return Err(cli_error(format!(
            "connector `{}` already exists; pass --replace to overwrite it",
            connector.id
        )));
    }
    if existing {
        registry.upsert(connector.clone())?;
    } else {
        registry.add(connector.clone())?;
    }
    location.save(&registry)?;
    println!(
        "{} connector `{}` in {}",
        if existing { "Updated" } else { "Added" },
        connector.id,
        location.path.display()
    );
    println!("Next: set the required environment variables, then enable it:");
    println!("  ryoko connectors enable {}", connector.id);
    Ok(())
}

/// Toggles one connector in the workspace registry.
pub fn set_enabled(
    location: &RegistryLocation,
    id: &str,
    enabled: bool,
) -> Result<(), Box<dyn Error>> {
    let mut registry = location.load()?;
    let connector = registry
        .get_mut(id)
        .ok_or_else(|| cli_error(format!("connector `{id}` is not configured")))?;
    if enabled && connector.allowed_tools.is_empty() {
        return Err(cli_error(format!(
            "connector `{id}` has no allowed tools; add tools to {} before enabling it",
            location.path.display()
        )));
    }
    connector.enabled = enabled;
    location.save(&registry)?;
    println!(
        "Connector `{id}` is now {}.",
        if enabled { "enabled" } else { "disabled" }
    );
    Ok(())
}

/// Loads the enabled connectors a run should mount.
///
/// Every entry is validated again here, and an entry that cannot be mounted is
/// an error: a run must never silently lose a connector the operator enabled.
pub fn load_enabled(
    repo: &Path,
    config: Option<&Path>,
) -> Result<Vec<ConnectorConfig>, Box<dyn Error>> {
    let location = RegistryLocation::resolve(repo, config)?;
    let registry = location.load()?;
    let mut enabled = Vec::new();
    for connector in registry.enabled() {
        connector.validate()?;
        if !matches!(connector.transport, ConnectorTransport::Stdio { .. }) {
            return Err(cli_error(format!(
                "connector `{}` uses the {} transport, which this build does not mount; disable it in {} or use a stdio server",
                connector.id,
                connector.transport.kind(),
                location.path.display()
            )));
        }
        if connector.allowed_tools.is_empty() {
            return Err(cli_error(format!(
                "connector `{}` is enabled but allows no tools; add at least one tool in {}",
                connector.id,
                location.path.display()
            )));
        }
        enabled.push(connector.clone());
    }
    Ok(enabled)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn location() -> (tempfile::TempDir, RegistryLocation) {
        let dir = tempfile::tempdir().unwrap();
        let location = RegistryLocation::resolve(dir.path(), None).unwrap();
        (dir, location)
    }

    #[test]
    fn a_preset_is_scaffolded_disabled_and_secret_free() {
        let options = AddOptions {
            preset: "github".into(),
            ..AddOptions::default()
        };
        let connector = scaffold(&options).unwrap();
        assert_eq!(connector.id, "github");
        assert!(!connector.enabled);
        assert_eq!(
            connector.required_env,
            vec!["GITHUB_PERSONAL_ACCESS_TOKEN".to_string()]
        );
        let encoded = serde_json::to_string(&connector).unwrap();
        assert!(!encoded.contains("ghp_"));
    }

    #[test]
    fn overrides_replace_the_transport_and_tools() {
        let options = AddOptions {
            preset: "custom".into(),
            id: Some("internal".into()),
            command: Some("node".into()),
            args: vec!["server.mjs".into()],
            env: vec!["INTERNAL_TOKEN".into()],
            allow_tools: vec!["lookup".into()],
            max_calls: Some(4),
            ..AddOptions::default()
        };
        let connector = scaffold(&options).unwrap();
        assert_eq!(connector.id, "internal");
        assert_eq!(
            connector.transport,
            ConnectorTransport::Stdio {
                command: "node".into(),
                args: vec!["server.mjs".into()]
            }
        );
        assert_eq!(connector.call_limit(), 4);
        assert!(!connector.enabled);
    }

    #[test]
    fn an_unknown_preset_and_conflicting_transports_are_refused() {
        let unknown = AddOptions {
            preset: "does-not-exist".into(),
            ..AddOptions::default()
        };
        assert!(scaffold(&unknown).is_err());

        let conflicting = AddOptions {
            preset: "custom".into(),
            command: Some("npx".into()),
            url: Some("https://example.com/sse".into()),
            ..AddOptions::default()
        };
        assert!(scaffold(&conflicting).is_err());
    }

    #[test]
    fn adding_writes_the_registry_and_refuses_silent_overwrites() {
        let (_dir, location) = location();
        let options = AddOptions {
            preset: "github".into(),
            ..AddOptions::default()
        };
        add(&location, &options).unwrap();
        assert!(location.path.exists());
        assert!(add(&location, &options).is_err());

        let replace = AddOptions {
            replace: true,
            ..options
        };
        add(&location, &replace).unwrap();
        assert_eq!(location.load().unwrap().connectors.len(), 1);
    }

    #[test]
    fn enabling_requires_tools_and_a_known_id() {
        let (_dir, location) = location();
        add(
            &location,
            &AddOptions {
                preset: "custom".into(),
                command: Some("node".into()),
                args: vec!["server.mjs".into()],
                ..AddOptions::default()
            },
        )
        .unwrap();
        assert!(set_enabled(&location, "custom", true).is_err());
        assert!(set_enabled(&location, "missing", false).is_err());

        let mut registry = location.load().unwrap();
        registry
            .set_allowed_tools("custom", ["lookup".to_string()])
            .unwrap();
        location.save(&registry).unwrap();
        set_enabled(&location, "custom", true).unwrap();
        assert!(location.load().unwrap().connectors[0].enabled);
        set_enabled(&location, "custom", false).unwrap();
        assert!(!location.load().unwrap().connectors[0].enabled);
    }

    #[test]
    fn only_enabled_connectors_are_loaded_and_a_missing_file_is_empty() {
        let (dir, location) = location();
        assert!(load_enabled(dir.path(), None).unwrap().is_empty());

        add(
            &location,
            &AddOptions {
                preset: "github".into(),
                ..AddOptions::default()
            },
        )
        .unwrap();
        assert!(load_enabled(dir.path(), None).unwrap().is_empty());
    }

    #[test]
    fn an_enabled_network_connector_is_refused_instead_of_silently_skipped() {
        let (dir, location) = location();
        add(
            &location,
            &AddOptions {
                preset: "custom".into(),
                id: Some("remote".into()),
                url: Some("https://mcp.example.com/sse".into()),
                env: Vec::new(),
                allow_tools: vec!["lookup".into()],
                ..AddOptions::default()
            },
        )
        .unwrap();
        set_enabled(&location, "remote", true).unwrap();
        let error = load_enabled(dir.path(), None).expect_err("sse is not mounted here");
        assert!(error.to_string().contains("does not mount"), "{error}");
    }
}
