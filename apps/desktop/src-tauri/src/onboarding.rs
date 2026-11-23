//! Machine-scoped onboarding preferences and workspace history.
//! Credentials are stored exclusively by credential_store, not in this JSON.
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use crate::credential_store;

#[derive(Debug, Default, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SetupState {
    #[serde(default)]
    pub provider: String,
    #[serde(default)]
    pub model: String,
    #[serde(default)]
    pub configured: bool,
    #[serde(default)]
    pub recent_folders: Vec<String>,
}

pub fn provider_endpoint(id: &str) -> Option<(&'static str, &'static str)> {
    match id {
        "openai" => Some(("https://api.openai.com/v1", "openai")),
        "anthropic" => Some(("https://api.anthropic.com", "anthropic")),
        "gemini" => Some(("https://generativelanguage.googleapis.com/v1beta", "gemini")),
        "ollama" => Some(("http://127.0.0.1:11434/v1", "openai")),
        _ => None,
    }
}

fn settings_path() -> Result<PathBuf, String> {
    #[cfg(target_os = "windows")]
    let root = std::env::var_os("APPDATA")
        .map(PathBuf::from)
        .ok_or("APPDATA is not set")?
        .join("Ryoko");
    #[cfg(target_os = "macos")]
    let root = std::env::var_os("HOME")
        .map(PathBuf::from)
        .ok_or("HOME is not set")?
        .join("Library/Application Support/Ryoko");
    #[cfg(not(any(target_os = "windows", target_os = "macos")))]
    let root = if let Some(path) = std::env::var_os("XDG_CONFIG_HOME") {
        PathBuf::from(path).join("ryoko")
    } else {
        std::env::var_os("HOME")
            .map(PathBuf::from)
            .ok_or("HOME is not set")?
            .join(".config/ryoko")
    };
    Ok(root.join("desktop-setup.json"))
}

pub fn load() -> Result<SetupState, String> {
    let path = settings_path()?;
    let text = match fs::read_to_string(&path) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(SetupState::default());
        }
        Err(error) => return Err(format!("cannot read desktop preferences: {error}")),
    };
    if text.len() > 32 * 1024 {
        return Err("desktop preferences are too large".to_owned());
    }
    serde_json::from_str(&text).map_err(|error| format!("invalid desktop preferences: {error}"))
}

fn save(state: &SetupState) -> Result<(), String> {
    let path = settings_path()?;
    let parent = path.parent().ok_or("invalid desktop settings path")?;
    fs::create_dir_all(parent)
        .map_err(|error| format!("cannot create desktop settings folder: {error}"))?;
    if fs::symlink_metadata(&path).is_ok_and(|metadata| metadata.file_type().is_symlink()) {
        return Err("refusing to overwrite a symlinked settings file".to_owned());
    }
    let tmp = path.with_extension(format!(
        "tmp-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos()
    ));
    let mut opts = fs::OpenOptions::new();
    opts.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600);
    }
    let write_result = (|| -> Result<(), String> {
        let mut file = opts.open(&tmp).map_err(|error| error.to_string())?;
        let json = serde_json::to_vec_pretty(state).map_err(|error| error.to_string())?;
        file.write_all(&json)
            .and_then(|()| file.sync_all())
            .map_err(|error| error.to_string())
    })();
    if let Err(error) = write_result {
        let _ = fs::remove_file(&tmp);
        return Err(format!("cannot save desktop preferences: {error}"));
    }
    fs::rename(&tmp, &path).map_err(|error| {
        let _ = fs::remove_file(&tmp);
        format!("cannot replace desktop preferences: {error}")
    })
}

pub fn remember(path: &Path) -> Result<SetupState, String> {
    let canonical = path
        .canonicalize()
        .map_err(|error| format!("folder not found: {error}"))?;
    if !canonical.is_dir() {
        return Err("workspace is not a directory".to_owned());
    }
    let value = canonical.to_string_lossy().into_owned();
    let mut state = load()?;
    state.recent_folders.retain(|folder| folder != &value);
    state.recent_folders.insert(0, value);
    state.recent_folders.truncate(8);
    save(&state)?;
    Ok(state)
}

#[tauri::command]
pub fn onboarding_state() -> Result<SetupState, String> {
    load()
}

#[tauri::command]
pub fn onboarding_configure(
    provider: String,
    model: String,
    api_key: Option<String>,
) -> Result<SetupState, String> {
    provider_endpoint(&provider).ok_or("Choose a supported model provider")?;
    let model = model.trim();
    if model.is_empty()
        || model.len() > 128
        || !model
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b":-._/".contains(&byte))
    {
        return Err("Model ID must be 1–128 letters, digits, periods, hyphens, colons, underscores or slashes".to_owned());
    }
    if provider != "ollama" {
        let key = api_key
            .as_deref()
            .map(str::trim)
            .filter(|key| !key.is_empty())
            .ok_or("An API key is required for this provider")?;
        if key.len() > 8192 {
            return Err("API key exceeds 8 KiB".to_owned());
        }
        credential_store::store(&provider, key)?;
    }
    let mut state = load()?;
    state.provider = provider;
    state.model = model.to_owned();
    state.configured = true;
    save(&state)?;
    Ok(state)
}

#[tauri::command]
pub fn onboarding_remember_folder(path: String) -> Result<SetupState, String> {
    remember(Path::new(&path))
}

#[tauri::command]
pub async fn clone_repository(url: String, parent: String) -> Result<String, String> {
    let parsed = reqwest::Url::parse(&url).map_err(|_| "Enter a valid HTTPS clone URL")?;
    if parsed.scheme() != "https"
        || parsed.host_str().is_none()
        || !parsed.username().is_empty()
        || parsed.password().is_some()
        || parsed.query().is_some()
        || parsed.fragment().is_some()
    {
        return Err(
            "Only HTTPS repository URLs without embedded credentials are accepted".to_owned(),
        );
    }
    let segment = parsed
        .path_segments()
        .and_then(|mut parts| parts.next_back())
        .unwrap_or_default()
        .trim_end_matches(".git");
    if segment.is_empty()
        || segment == "."
        || segment == ".."
        || !segment
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"-._".contains(&byte))
    {
        return Err("Repository URL must end in a safe folder name".to_owned());
    }
    let dir = PathBuf::from(parent)
        .canonicalize()
        .map_err(|error| error.to_string())?;
    if !dir.is_dir() {
        return Err("Choose a parent directory".to_owned());
    }
    let destination = dir.join(segment);
    if destination.exists() {
        return Err("A folder with this repository name already exists".to_owned());
    }
    let url = url.to_owned();
    let result = tauri::async_runtime::spawn_blocking(move || {
        let status = Command::new("git")
            .arg("-c")
            .arg("credential.interactive=never")
            .arg("clone")
            .arg("--depth")
            .arg("1")
            .arg("--")
            .arg(&url)
            .arg(&destination)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .map_err(|error| format!("Git is not installed or could not start: {error}"))?;
        if !status.success() {
            return Err(
                "Git clone failed. Check the URL, access, and network, then retry.".to_owned(),
            );
        }
        Ok(destination.to_string_lossy().into_owned())
    })
    .await
    .map_err(|error| format!("clone task failed: {error}"))??;
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn provider_routes_are_bounded_to_known_endpoints() {
        assert!(provider_endpoint("openai").is_some());
        assert!(provider_endpoint("ollama").is_some());
        assert!(provider_endpoint("https://unknown").is_none());
    }
}
