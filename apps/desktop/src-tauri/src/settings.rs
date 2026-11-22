//! Workspace-scoped settings storage for the visual settings UI.
//!
//! Two files under `.ryoko/desktop/` are written on explicit user action:
//! `providers.json` (a validated `ryoko-models` provider catalog — API keys
//! stay in environment variables, never in this file) and `lsp.json`
//! (validated by [`crate::servers`]). Nothing else in `.ryoko/desktop` is
//! created by the shell, and every write is bounded and symlink-checked.

use std::fs;
use std::path::{Path, PathBuf};

use ryoko_models::ProviderCatalog;

pub const PROVIDERS_FILE: &str = "providers.json";
pub const LSP_REGISTRY_FILE: &str = "lsp.json";
const MAX_SETTINGS_BYTES: u64 = 256 * 1024;

pub fn settings_dir(root: &Path) -> PathBuf {
    root.join(".ryoko/desktop")
}

fn write_private(path: &Path, content: &str) -> Result<(), String> {
    let temp = path.with_extension(format!(
        "tmp-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|duration| duration.as_nanos())
            .unwrap_or_default()
    ));
    let write = (|| {
        let mut options = fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        use std::io::Write;
        let mut file = options.open(&temp).map_err(|error| error.to_string())?;
        file.write_all(content.as_bytes())
            .and_then(|()| file.sync_all())
            .map_err(|error| error.to_string())
    })();
    if let Err(error) = write {
        let _ = fs::remove_file(&temp);
        return Err(format!("cannot write settings: {error}"));
    }
    fs::rename(&temp, path).map_err(|error| {
        let _ = fs::remove_file(&temp);
        format!("cannot write settings: {error}")
    })
}

/// Validates and saves the provider catalog. Returns the canonical JSON that
/// was written so the UI can render exactly what is on disk.
pub fn save_providers(root: &Path, json: &str) -> Result<String, String> {
    if json.len() as u64 > MAX_SETTINGS_BYTES {
        return Err("provider catalog exceeds 256 KiB".to_owned());
    }
    let catalog = ProviderCatalog::parse_json(json)
        .map_err(|error| format!("invalid provider catalog: {error}"))?;
    catalog
        .validate()
        .map_err(|error| format!("invalid provider catalog: {error}"))?;
    let canonical = serde_json::to_string_pretty(&catalog)
        .map_err(|error| format!("cannot serialize provider catalog: {error}"))?;
    let directory = settings_dir(root);
    crate::paths::create_directories_without_symlinks(root, &directory)?;
    write_private(&directory.join(PROVIDERS_FILE), &canonical)?;
    Ok(canonical)
}

pub fn load_providers(root: &Path) -> Result<Option<String>, String> {
    read_optional(&settings_dir(root).join(PROVIDERS_FILE))
}

pub fn load_lsp_registry(root: &Path) -> Result<Option<String>, String> {
    read_optional(&crate::servers::registry_path(root))
}

/// Saves an already-validated registry (the caller validates with
/// [`crate::servers::LspRegistry::parse`] before calling this).
pub fn save_lsp_registry(root: &Path, canonical: &str) -> Result<(), String> {
    let directory = settings_dir(root);
    crate::paths::create_directories_without_symlinks(root, &directory)?;
    write_private(&directory.join(LSP_REGISTRY_FILE), canonical)
}

fn read_optional(path: &Path) -> Result<Option<String>, String> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.is_file() => {
            if metadata.len() > MAX_SETTINGS_BYTES {
                return Err("settings file exceeds 256 KiB".to_owned());
            }
            let real = path
                .canonicalize()
                .map_err(|error| format!("cannot resolve {}: {error}", path.display()))?;
            fs::read_to_string(real)
                .map(Some)
                .map_err(|error| format!("cannot read {}: {error}", path.display()))
        }
        Ok(_) => Err("settings path is not a regular file".to_owned()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(format!("cannot read {}: {error}", path.display())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct TempWorkspace(PathBuf);

    impl TempWorkspace {
        fn new() -> Self {
            let root = std::env::temp_dir().join(format!(
                "ryoko-settings-{}-{}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|duration| duration.as_nanos())
                    .unwrap_or_default()
            ));
            fs::create_dir_all(&root).expect("temp workspace");
            Self(root)
        }
    }

    impl Drop for TempWorkspace {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn a_valid_catalog_is_canonicalized_and_round_trips() {
        let workspace = TempWorkspace::new();
        let written = save_providers(
            &workspace.0,
            &serde_json::json!({
                "default_provider": "ollama",
                "providers": [{
                    "id": "ollama",
                    "base_url": "http://127.0.0.1:11434/v1",
                    "api_key_env": "RYOKO_OLLAMA_KEY"
                }]
            })
            .to_string(),
        )
        .expect("valid catalog saves");
        assert!(written.contains("default_provider"));
        let loaded = load_providers(&workspace.0)
            .expect("loads")
            .expect("present");
        assert_eq!(loaded, written);
    }

    #[test]
    fn rejects_broken_catalogs_without_writing() {
        let workspace = TempWorkspace::new();
        assert!(save_providers(&workspace.0, "not json").is_err());
        assert!(
            load_providers(&workspace.0)
                .expect("nothing written")
                .is_none()
        );
    }

    #[test]
    fn lsp_registry_round_trips() {
        let workspace = TempWorkspace::new();
        let registry = crate::servers::LspRegistry::parse(
            r#"{"version":1,"servers":[{"id":"pyright","command":"pyright-langserver","languages":["python"]}]}"#,
        )
        .expect("valid");
        registry.validate().expect("valid");
        save_lsp_registry(&workspace.0, &serde_json::to_string(&registry).unwrap()).expect("saves");
        let loaded = load_lsp_registry(&workspace.0)
            .expect("loads")
            .expect("present");
        assert!(loaded.contains("pyright-langserver"));
    }
}
