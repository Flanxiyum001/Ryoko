//! Local WASI extension installation.
//!
//! The marketplace catalog stays browse-only. An operator may still install a
//! local extension directory they already trust. Ryoko copies and verifies the
//! module through the existing `ryoko plugin install` workflow; it never runs
//! the plugin, never accepts a URL here, and never edits the plugin policy.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use serde::Serialize;
use tauri::{AppHandle, State};
use tauri_plugin_dialog::{DialogExt, MessageDialogButtons};

use crate::run::resolve_cli;
use crate::workspace::{WorkspaceState, current_root};

const MAX_MANIFEST_BYTES: u64 = 64 * 1024;
const MAX_OUTPUT: usize = 8 * 1024;
const MAX_EXTENSIONS: usize = 200;

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct InstalledExtension {
    pub id: String,
    pub name: String,
    pub version: String,
    pub kind: String,
    pub description: String,
    pub path: String,
    pub wasm_sha256: Option<String>,
}

pub(crate) fn valid_sha256(value: &str) -> bool {
    value.len() == 64 && value.chars().all(|character| character.is_ascii_hexdigit())
}

pub(crate) fn valid_local_source(value: &str) -> bool {
    let value = value.trim();
    let scp_style_remote = !value.starts_with('/')
        && !value.starts_with("./")
        && !value.starts_with("../")
        && value.contains('@')
        && value.contains(':');
    !value.is_empty()
        && value.len() <= 4_096
        && !value.contains("://")
        && !scp_style_remote
        && !value.chars().any(char::is_control)
}

fn extension_root(root: &Path) -> PathBuf {
    root.join(".ryoko/plugins")
}

fn read_manifest(path: &Path, root: &Path) -> Option<InstalledExtension> {
    let metadata = fs::symlink_metadata(path).ok()?;
    if !metadata.file_type().is_file() || metadata.len() > MAX_MANIFEST_BYTES {
        return None;
    }
    let text = fs::read_to_string(path).ok()?;
    let value: serde_json::Value = serde_json::from_str(&text).ok()?;
    let field = |name: &str| {
        value
            .get(name)
            .and_then(|entry| entry.as_str())
            .unwrap_or("")
    };
    let id = field("id");
    let kind = field("kind");
    if id.is_empty()
        || id.len() > 64
        || !id.chars().all(|character| {
            character.is_ascii_lowercase()
                || character.is_ascii_digit()
                || "._-".contains(character)
        })
        || !matches!(kind, "linter" | "context" | "agent")
    {
        return None;
    }
    let directory = path.parent()?;
    let wasm = directory.join("plugin.wasm");
    let wasm_metadata = fs::symlink_metadata(&wasm).ok()?;
    if !wasm_metadata.file_type().is_file() || wasm_metadata.len() > 64 * 1024 * 1024 {
        return None;
    }
    Some(InstalledExtension {
        id: id.to_owned(),
        name: field("name").chars().take(120).collect(),
        version: field("version").chars().take(40).collect(),
        kind: kind.to_owned(),
        description: field("description").chars().take(500).collect(),
        path: directory
            .strip_prefix(root)
            .unwrap_or(directory)
            .to_string_lossy()
            .replace('\\', "/"),
        wasm_sha256: value
            .get("wasm_sha256")
            .and_then(|entry| entry.as_str())
            .filter(|entry| valid_sha256(entry))
            .map(str::to_owned),
    })
}

pub(crate) fn installed_extensions(root: &Path) -> Vec<InstalledExtension> {
    let directory = extension_root(root);
    if fs::symlink_metadata(&directory).is_ok_and(|metadata| metadata.file_type().is_symlink()) {
        return Vec::new();
    }
    let Ok(entries) = fs::read_dir(&directory) else {
        return Vec::new();
    };
    let mut extensions = entries
        .filter_map(Result::ok)
        .filter(|entry| entry.file_type().is_ok_and(|kind| kind.is_dir()))
        .take(MAX_EXTENSIONS)
        .filter_map(|entry| {
            let manifest = entry.path().join("plugin.json");
            let path = if manifest.is_file() {
                manifest
            } else {
                entry.path().join("manifest.json")
            };
            read_manifest(&path, root)
        })
        .collect::<Vec<_>>();
    extensions.sort_by(|left, right| left.id.cmp(&right.id));
    extensions
}

#[tauri::command]
pub fn list_installed_extensions(
    state: State<'_, WorkspaceState>,
) -> Result<Vec<InstalledExtension>, String> {
    let root = current_root(&state)?;
    Ok(installed_extensions(&root))
}

fn run_install(root: &Path, source: &Path, sha256: Option<&str>) -> Result<String, String> {
    let cli = resolve_cli(root)?;
    let mut command = Command::new(cli);
    command
        .current_dir(root)
        .args(["plugin", "install"])
        .arg(source)
        .args(["--dir", ".ryoko/plugins"])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if let Some(pin) = sha256 {
        command.args(["--sha256", pin]);
    }
    let output = command
        .output()
        .map_err(|error| format!("cannot start plugin installation: {error}"))?;
    if !output.status.success() {
        let detail = String::from_utf8_lossy(&output.stderr);
        return Err(format!(
            "plugin installation failed: {}",
            detail.chars().take(MAX_OUTPUT).collect::<String>()
        ));
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    Ok(stdout.chars().take(MAX_OUTPUT).collect())
}

#[tauri::command]
pub async fn install_wasi_extension(
    source_path: String,
    sha256: Option<String>,
    app: AppHandle,
    state: State<'_, WorkspaceState>,
) -> Result<String, String> {
    if !valid_local_source(&source_path) {
        return Err("only a local extension directory can be installed here".to_owned());
    }
    if let Some(pin) = sha256.as_deref()
        && !valid_sha256(pin)
    {
        return Err("the expected SHA-256 must be exactly 64 hexadecimal characters".to_owned());
    }
    let root = current_root(&state)?;
    let source = PathBuf::from(source_path.trim())
        .canonicalize()
        .map_err(|error| format!("cannot open the extension directory: {error}"))?;
    if !source.is_dir() {
        return Err("the extension source must be a directory".to_owned());
    }
    let approved = tauri::async_runtime::spawn_blocking({
        let display = source.display().to_string();
        move || {
            app.dialog()
                .message(format!(
                    "Install the WASI extension from:\n\n{display}\n\nRyoko copies plugin.json and plugin.wasm into .ryoko/plugins, verifies the module hash, and records the manifest. It does not run the plugin or change your plugin policy. Only install code you trust.",
                ))
                .title("Ryoko — install local WASI extension")
                .buttons(MessageDialogButtons::OkCancelCustom(
                    "Install copy".to_owned(),
                    "Cancel".to_owned(),
                ))
                .blocking_show()
        }
    })
    .await
    .map_err(|_| "could not display native extension permission".to_owned())?;
    if !approved {
        return Err("extension installation cancelled".to_owned());
    }
    if current_root(&state)? != root {
        return Err("workspace changed while awaiting permission".to_owned());
    }
    tauri::async_runtime::spawn_blocking(move || run_install(&root, &source, sha256.as_deref()))
        .await
        .map_err(|_| "extension installation worker failed".to_owned())?
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hashes_and_local_sources_are_strict() {
        assert!(valid_sha256(&"a".repeat(64)));
        assert!(!valid_sha256("abc"));
        assert!(valid_local_source("/tmp/extension"));
        assert!(valid_local_source("/tmp/user@example/extension"));
        assert!(!valid_local_source("https://example.com/plugin.git"));
        assert!(!valid_local_source("git@example.com:plugin.git"));
        assert!(!valid_local_source(""));
    }

    #[test]
    fn manifests_are_summarised_without_running_the_module() {
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = std::env::temp_dir().join(format!("ryoko-ext-{stamp}"));
        let plugin = root.join(".ryoko/plugins/demo");
        fs::create_dir_all(&plugin).unwrap();
        fs::write(plugin.join("plugin.wasm"), b"\0asm").unwrap();
        fs::write(
            plugin.join("plugin.json"),
            r#"{"schema":1,"id":"demo","name":"Demo","version":"1.0.0","kind":"linter","description":"x"}"#,
        )
        .unwrap();
        let found = installed_extensions(&root);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].id, "demo");
        assert_eq!(found[0].kind, "linter");
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn a_missing_wasm_module_is_not_listed() {
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = std::env::temp_dir().join(format!("ryoko-ext-missing-{stamp}"));
        let plugin = root.join(".ryoko/plugins/demo");
        fs::create_dir_all(&plugin).unwrap();
        fs::write(
            plugin.join("plugin.json"),
            r#"{"schema":1,"id":"demo","name":"Demo","version":"1.0.0","kind":"linter"}"#,
        )
        .unwrap();
        assert!(installed_extensions(&root).is_empty());
        fs::remove_dir_all(root).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn a_symlinked_plugin_root_is_not_followed() {
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = std::env::temp_dir().join(format!("ryoko-ext-link-{stamp}"));
        let outside = root.join("outside/demo");
        fs::create_dir_all(&outside).unwrap();
        fs::write(outside.join("plugin.wasm"), b"\0asm").unwrap();
        fs::write(
            outside.join("plugin.json"),
            r#"{"schema":1,"id":"demo","name":"Demo","version":"1.0.0","kind":"linter"}"#,
        )
        .unwrap();
        fs::create_dir_all(root.join(".ryoko")).unwrap();
        std::os::unix::fs::symlink(root.join("outside"), root.join(".ryoko/plugins")).unwrap();
        assert!(installed_extensions(&root).is_empty());
        fs::remove_dir_all(root).unwrap();
    }
}
