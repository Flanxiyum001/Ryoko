//! In-app updates.
//!
//! Release builds made with an update signing key embed the updater's public
//! key and the URL of a `latest.json` manifest (see
//! `scripts/updater/release_config.mjs`). This module exposes three commands
//! to the renderer:
//!
//! - `update_status` reports whether this build can update itself.
//! - `update_check` fetches the manifest and reports a newer version, if any.
//! - `update_install` downloads that version, verifies its signature against
//!   the built-in public key, installs it, and restarts the app.
//!
//! Nothing is downloaded or installed unless the user asks: the renderer only
//! calls `update_install` from an explicit "Download and install" action.
//! The manifest is untrusted input. Its version and notes are bounded before
//! being shown, and the downloaded artifact is installed only if its minisign
//! signature, including the signed version, verifies.

use serde::Serialize;
use std::collections::HashMap;
use std::sync::Mutex;
use tauri::{AppHandle, Emitter, Runtime, State};

use tauri_plugin_updater::{Update, UpdaterExt};

/// Event carrying download progress to the renderer.
pub const PROGRESS_EVENT: &str = "ryoko://update-progress";

/// Release notes longer than this are cut before reaching the renderer.
pub const MAX_NOTES_CHARS: usize = 8_000;
const MAX_VERSION_CHARS: usize = 64;

#[derive(Default)]
pub struct UpdaterState {
    /// Whether this build was configured with an updater key and endpoint.
    pub configured: bool,
    pending: Mutex<Option<Update>>,
    installing: Mutex<bool>,
}

impl UpdaterState {
    pub fn new(configured: bool) -> Self {
        Self {
            configured,
            ..Self::default()
        }
    }
}

#[derive(Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct UpdateStatus {
    pub configured: bool,
    pub current_version: String,
}

#[derive(Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct UpdateInfo {
    pub version: String,
    pub current_version: String,
    pub notes: String,
    pub date: Option<String>,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct UpdateProgress {
    pub downloaded: u64,
    pub total: Option<u64>,
}

/// Bounds and cleans manifest text before it reaches the renderer.
pub fn clean_text(text: &str, max_chars: usize) -> String {
    text.chars()
        .filter(|c| !c.is_control() || *c == '\n' || *c == '\t')
        .take(max_chars)
        .collect()
}

/// Whether the bundled configuration enables the updater plugin.
pub fn is_configured(plugins: &HashMap<String, serde_json::Value>) -> bool {
    let Some(updater) = plugins.get("updater") else {
        return false;
    };
    let has_key = updater
        .get("pubkey")
        .and_then(|value| value.as_str())
        .is_some_and(|key| !key.trim().is_empty());
    let has_https_endpoint = updater
        .get("endpoints")
        .and_then(|value| value.as_array())
        .is_some_and(|endpoints| {
            !endpoints.is_empty()
                && endpoints
                    .iter()
                    .all(|url| url.as_str().is_some_and(|url| url.starts_with("https://")))
        });
    has_key && has_https_endpoint
}

#[tauri::command]
pub fn update_status<R: Runtime>(
    app: AppHandle<R>,
    state: State<'_, UpdaterState>,
) -> UpdateStatus {
    UpdateStatus {
        configured: state.configured,
        current_version: app.package_info().version.to_string(),
    }
}

#[tauri::command]
pub async fn update_check<R: Runtime>(
    app: AppHandle<R>,
    state: State<'_, UpdaterState>,
) -> Result<Option<UpdateInfo>, String> {
    if !state.configured {
        return Err("This build was not published with in-app updates.".into());
    }
    let update = app
        .updater()
        .map_err(|error| error.to_string())?
        .check()
        .await
        .map_err(|error| format!("Could not check for updates: {error}"))?;
    let info = update.as_ref().map(|update| UpdateInfo {
        version: clean_text(&update.version, MAX_VERSION_CHARS),
        current_version: clean_text(&update.current_version, MAX_VERSION_CHARS),
        notes: clean_text(update.body.as_deref().unwrap_or_default(), MAX_NOTES_CHARS),
        date: update.date.map(|date| date.to_string()),
    });
    *state.pending.lock().map_err(|_| "updater state poisoned")? = update;
    Ok(info)
}

#[tauri::command]
pub async fn update_install<R: Runtime>(
    app: AppHandle<R>,
    state: State<'_, UpdaterState>,
) -> Result<(), String> {
    {
        let mut installing = state
            .installing
            .lock()
            .map_err(|_| "updater state poisoned")?;
        if *installing {
            return Err("An update is already being installed.".into());
        }
        *installing = true;
    }
    let result = install_pending(&app, &state).await;
    if let Ok(mut installing) = state.installing.lock() {
        *installing = false;
    }
    result?;
    // Windows installers exit the app themselves; elsewhere restart into the new version.
    app.restart();
}

async fn install_pending<R: Runtime>(
    app: &AppHandle<R>,
    state: &State<'_, UpdaterState>,
) -> Result<(), String> {
    let update = state
        .pending
        .lock()
        .map_err(|_| "updater state poisoned")?
        .take()
        .ok_or("Check for updates first.")?;
    let mut downloaded: u64 = 0;
    let progress_app = app.clone();
    update
        .download_and_install(
            move |chunk, total| {
                downloaded = downloaded.saturating_add(chunk as u64);
                let _ = progress_app.emit(PROGRESS_EVENT, UpdateProgress { downloaded, total });
            },
            || {},
        )
        .await
        .map_err(|error| format!("The update could not be installed: {error}"))
}

/// Registers the updater plugin only when the build carries updater config,
/// so development and unsigned builds start normally.
pub fn register<R: Runtime>(builder: tauri::Builder<R>, configured: bool) -> tauri::Builder<R> {
    if configured {
        builder.plugin(tauri_plugin_updater::Builder::new().build())
    } else {
        builder
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn plugins(value: serde_json::Value) -> HashMap<String, serde_json::Value> {
        value.as_object().cloned().unwrap().into_iter().collect()
    }

    #[test]
    fn unconfigured_builds_do_not_enable_the_updater() {
        assert!(!is_configured(&plugins(json!({}))));
        assert!(!is_configured(&plugins(json!({ "updater": {} }))));
        assert!(!is_configured(&plugins(json!({
            "updater": { "pubkey": " ", "endpoints": ["https://example.com/latest.json"] }
        }))));
    }

    #[test]
    fn insecure_or_missing_endpoints_are_refused() {
        assert!(!is_configured(&plugins(json!({
            "updater": { "pubkey": "KEY", "endpoints": ["http://example.com/latest.json"] }
        }))));
        assert!(!is_configured(&plugins(json!({
            "updater": { "pubkey": "KEY", "endpoints": [] }
        }))));
    }

    #[test]
    fn signed_https_config_enables_the_updater() {
        assert!(is_configured(&plugins(json!({
            "updater": { "pubkey": "KEY", "endpoints": ["https://example.com/latest.json"] }
        }))));
    }

    #[test]
    fn manifest_text_is_bounded_and_stripped_of_control_characters() {
        assert_eq!(clean_text("1.2.3\u{1b}[31m\u{0}", 64), "1.2.3[31m");
        assert_eq!(clean_text("line one\nline two", 64), "line one\nline two");
        assert_eq!(
            clean_text(&"x".repeat(10_000), MAX_NOTES_CHARS).len(),
            MAX_NOTES_CHARS
        );
    }
}
