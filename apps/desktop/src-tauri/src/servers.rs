//! Third-party language-server registry and method routing.
//!
//! Standard servers (rust-analyzer, pyright, tsserver, gopls …) run beside
//! `ryoko-lsp`, which keeps its custom diagnostics and agent code lenses.
//! The registry lives in `.ryoko/desktop/lsp.json` inside the workspace; it is
//! read and validated here, and never executed without validation. Commands
//! are plain executable names resolved on PATH (no path separators, no shell).

use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

pub const LSP_REGISTRY_VERSION: u32 = 1;
const MAX_SERVERS: usize = 16;
const MAX_ARGS: usize = 16;
const MAX_ARG_CHARS: usize = 512;
const MAX_LANGUAGES: usize = 12;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ServerEntry {
    pub id: String,
    pub command: String,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default)]
    pub languages: Vec<String>,
}

impl ServerEntry {
    fn validate(&self) -> Result<(), String> {
        valid_server_id(&self.id)?;
        if self.id == crate::lsp::RYOKO_LSP_ID {
            return Err("ryoko-lsp is reserved for the built-in language server".to_owned());
        }
        let command = self.command.trim();
        if command.is_empty() {
            return Err(format!("server {} has an empty command", self.id));
        }
        valid_command(command)?;
        if self.languages.is_empty() {
            return Err(format!("server {} declares no languages", self.id));
        }
        if self.languages.len() > MAX_LANGUAGES {
            return Err(format!(
                "server {} declares more than {MAX_LANGUAGES} languages",
                self.id
            ));
        }
        for language in &self.languages {
            valid_language(language)?;
        }
        if self.args.len() > MAX_ARGS {
            return Err(format!(
                "server {} passes more than {MAX_ARGS} args",
                self.id
            ));
        }
        for argument in &self.args {
            if argument.is_empty()
                || argument.len() > MAX_ARG_CHARS
                || argument.chars().any(char::is_control)
            {
                return Err(format!("server {} has an invalid argument", self.id));
            }
            if std::path::Path::new(argument)
                .components()
                .any(|component| matches!(component, std::path::Component::ParentDir))
            {
                return Err(format!("server {} argument escapes upward", self.id));
            }
        }
        Ok(())
    }
}

/// A plain executable name: no separators, no traversal, no shell syntax.
fn valid_command(command: &str) -> Result<(), String> {
    if command.len() > 256
        || command.chars().any(char::is_whitespace)
        || command.starts_with('-')
        || command.chars().any(|c| {
            matches!(
                c,
                '/' | '\\' | ';' | '|' | '&' | '$' | '`' | '>' | '<' | '*' | '?' | '"' | '\''
            )
        })
        || command.contains("..")
    {
        return Err(format!(
            "server command must be a bare executable name on PATH: {command:?}"
        ));
    }
    Ok(())
}

fn valid_server_id(id: &str) -> Result<(), String> {
    let valid = !id.is_empty()
        && id.len() <= 48
        && id
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
        && !id.starts_with('-');
    if valid {
        Ok(())
    } else {
        Err(format!(
            "server id must be lowercase kebab-case up to 48 characters: {id:?}"
        ))
    }
}

fn valid_language(language: &str) -> Result<(), String> {
    let valid = !language.is_empty()
        && language.len() <= 32
        && language
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '+');
    if valid {
        Ok(())
    } else {
        Err(format!(
            "language ids must be lowercase (letters, digits, -, +): {language:?}"
        ))
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct LspRegistry {
    #[serde(default = "default_version")]
    pub version: u32,
    #[serde(default)]
    pub servers: Vec<ServerEntry>,
}

fn default_version() -> u32 {
    LSP_REGISTRY_VERSION
}

impl LspRegistry {
    /// Parses and validates a registry file's contents.
    pub fn parse(text: &str) -> Result<Self, String> {
        let registry: LspRegistry = serde_json::from_str(text)
            .map_err(|error| format!("invalid language-server registry: {error}"))?;
        registry.validate()?;
        Ok(registry)
    }

    /// Loads `.ryoko/desktop/lsp.json` relative to the workspace. A missing
    /// file is an empty registry so the shell starts with `ryoko-lsp` only.
    pub fn load(root: &Path) -> Result<Self, String> {
        let path = registry_path(root);
        let metadata = match fs::symlink_metadata(&path) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(Self::default());
            }
            Err(error) => return Err(format!("cannot inspect LSP registry: {error}")),
        };
        if !metadata.file_type().is_file() || metadata.len() > 256 * 1024 {
            return Err("LSP registry must be a regular file under 256 KiB".to_owned());
        }
        let text = fs::read_to_string(&path)
            .map_err(|error| format!("cannot read LSP registry: {error}"))?;
        Self::parse(&text)
    }

    pub fn validate(&self) -> Result<(), String> {
        if self.version != LSP_REGISTRY_VERSION {
            return Err(format!(
                "language-server registry version must be {LSP_REGISTRY_VERSION}"
            ));
        }
        if self.servers.len() > MAX_SERVERS {
            return Err(format!(
                "at most {MAX_SERVERS} language servers may be registered"
            ));
        }
        let mut seen = std::collections::BTreeSet::new();
        for server in &self.servers {
            server.validate()?;
            if !seen.insert(server.id.as_str()) {
                return Err(format!("duplicate language-server id: {}", server.id));
            }
        }
        // One preferred server per language; later duplicates are rejected
        // instead of being silently shadowed.
        let mut language_owner = std::collections::BTreeMap::new();
        for server in &self.servers {
            for language in &server.languages {
                if let Some(owner) = language_owner.insert(language.as_str(), server.id.as_str()) {
                    return Err(format!(
                        "language {language} is claimed by both {owner} and {}",
                        server.id
                    ));
                }
            }
        }
        Ok(())
    }

    /// The server id that owns a language, if any.
    #[cfg(test)]
    pub fn owner_of(&self, language: &str) -> Option<&str> {
        self.servers
            .iter()
            .find(|server| {
                server
                    .languages
                    .iter()
                    .any(|candidate| candidate == language)
            })
            .map(|server| server.id.as_str())
    }
}

pub fn registry_path(root: &Path) -> PathBuf {
    root.join(".ryoko/desktop/lsp.json")
}

#[cfg(test)]
const WORKSPACE_REQUESTS: [&str; 2] = ["workspace/executeCommand", "workspace/symbol"];

/// Decides which registered servers receive an outbound JSON-RPC message.
#[cfg(test)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Route {
    /// The shell-level lifecycle: the multiplexer sends these per server.
    Lifecycle,
    /// Broadcast to every running server (document notifications fan out so
    /// ryoko-lsp keeps publishing diagnostics, and workspace state is shared).
    AllServers,
    /// Only the server owning the document's language answers; requests are
    /// never fanned out, so the UI never merges conflicting edits.
    DocumentOwner,
    /// ryoko-lsp owns custom lenses and diagnostics and is always running.
    RyokoOnly,
}

#[cfg(test)]
pub fn route_outbound(registry: &LspRegistry, language: &str, method: &str) -> Route {
    if matches!(method, "initialize" | "initialized" | "shutdown" | "exit") {
        return Route::Lifecycle;
    }
    if method.starts_with("window/") {
        return Route::Lifecycle;
    }
    if method.starts_with("workspace/") && !WORKSPACE_REQUESTS.contains(&method) {
        return Route::AllServers;
    }
    if method.starts_with("textDocument/") {
        return match method {
            "textDocument/didOpen" | "textDocument/didChange" | "textDocument/didClose" => {
                Route::AllServers
            }
            _ => {
                if registry.owner_of(language).is_some() {
                    Route::DocumentOwner
                } else {
                    Route::RyokoOnly
                }
            }
        };
    }
    Route::RyokoOnly
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse_registry(text: &str) -> LspRegistry {
        LspRegistry::parse(text).expect("valid registry")
    }

    #[test]
    fn accepts_a_typical_registry() {
        let registry = parse_registry(
            r#"{"version":1,"servers":[
                {"id":"rust-analyzer","command":"rust-analyzer","languages":["rust"]},
                {"id":"pyright","command":"pyright-langserver","args":["--stdio"],"languages":["python"]}
            ]}"#,
        );
        registry.validate().expect("valid");
        assert_eq!(registry.owner_of("rust"), Some("rust-analyzer"));
        assert_eq!(registry.owner_of("python"), Some("pyright"));
        assert_eq!(registry.owner_of("plaintext"), None);
    }

    #[test]
    fn a_missing_file_is_an_empty_registry() {
        let root = std::env::temp_dir();
        let registry = LspRegistry::load(&root).expect("empty registry");
        assert_eq!(registry.servers, Vec::new());
    }

    #[test]
    fn rejects_shell_metacharacters_and_traversal() {
        for command in ["rm -rf /", "sh;id", "/bin/evil", "../python", "a$PATH"] {
            let text = format!(
                r#"{{"servers":[{{"id":"bad","command":"{command}","languages":["x"]}}]}}"#
            );
            assert!(
                LspRegistry::parse(&text).is_err(),
                "command must be rejected: {command}"
            );
        }
    }

    #[test]
    fn the_builtin_server_id_cannot_be_overridden() {
        let registry =
            r#"{"servers":[{"id":"ryoko-lsp","command":"python3","languages":["rust"]}]}"#;
        assert!(LspRegistry::parse(registry).is_err());
    }

    #[test]
    fn rejects_duplicate_servers_and_colliding_languages() {
        let text = r#"{"servers":[
            {"id":"a","command":"a-ls","languages":["rust","python"]},
            {"id":"a","command":"a-ls","languages":["rust"]}
        ]}"#;
        assert!(LspRegistry::parse(text).is_err());

        let text = r#"{"servers":[
            {"id":"a","command":"a-ls","languages":["rust"]},
            {"id":"b","command":"b-ls","languages":["rust"]}
        ]}"#;
        assert!(LspRegistry::parse(text).is_err());
    }

    #[test]
    fn unknown_fields_and_bad_versions_are_rejected() {
        let text = r#"{"version":2,"servers":[]}"#;
        assert!(LspRegistry::parse(text).is_err());

        let text = r#"{"servers":[{"id":"a","command":"a","languages":["rust"],"extra":1}]}"#;
        assert!(LspRegistry::parse(text).is_err());
    }

    #[test]
    fn lifecycle_and_workspace_methods_have_explicit_routes() {
        let registry = parse_registry(
            r#"{"servers":[{"id":"pyright","command":"pyright-langserver","languages":["python"]}]}"#,
        );
        assert_eq!(
            route_outbound(&registry, "python", "initialize"),
            Route::Lifecycle
        );
        assert_eq!(
            route_outbound(&registry, "python", "workspace/didChangeConfiguration"),
            Route::AllServers
        );
        assert_eq!(
            route_outbound(&registry, "python", "textDocument/completion"),
            Route::DocumentOwner
        );
        assert_eq!(
            route_outbound(&registry, "plaintext", "textDocument/completion"),
            Route::RyokoOnly
        );
        assert_eq!(
            route_outbound(&registry, "python", "textDocument/didOpen"),
            Route::AllServers
        );
    }
}
