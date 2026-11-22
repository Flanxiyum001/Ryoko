//! Marketplace commands for the Ryoko Desktop shell.
//!
//! These commands expose the ryoko-marketplace crate to the frontend
//! for browsing, searching, and installing extensions and skills.

use ryoko_marketplace::{
    DownloadConfig, MarketplaceClient, MarketplaceEntry, MarketplaceIndex, MarketplaceLock,
    MarketplaceType, SkillKind,
};
use serde::Serialize;
use tauri::State;

use crate::workspace::{WorkspaceState, current_root};

const MAX_ENTRIES: usize = 200;
const MAX_DESCRIPTION_LENGTH: usize = 500;

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MarketplaceItemView {
    pub id: String,
    pub name: String,
    pub description: String,
    pub version: String,
    pub author: String,
    pub homepage: Option<String>,
    pub repository: Option<String>,
    pub license: Option<String>,
    pub kind: String, // "extension" | "skill"
    pub tags: Vec<String>,
    pub downloads: u64,
    pub compatible_with: Vec<String>,
    pub asset_url: String,
    pub checksum: String,
    pub updated_at: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MarketplaceSnapshot {
    pub path: String,
    pub extensions: Vec<MarketplaceItemView>,
    pub skills: Vec<MarketplaceItemView>,
    pub error: Option<String>,
}

fn entry_to_view(entry: &MarketplaceEntry) -> MarketplaceItemView {
    MarketplaceItemView {
        id: entry.id.clone(),
        name: entry.name.clone(),
        description: entry.description.clone(),
        version: entry.version.clone(),
        author: entry.author.clone(),
        homepage: entry.homepage.clone(),
        repository: entry.repository.clone(),
        license: entry.license.clone(),
        kind: match entry.kind {
            MarketplaceType::Extension => "extension".to_string(),
            MarketplaceType::Skill => "skill".to_string(),
        },
        tags: entry.tags.clone(),
        downloads: entry.downloads,
        compatible_with: entry.compatible_with.clone(),
        asset_url: entry.asset_url.clone(),
        checksum: entry.checksum.clone(),
        updated_at: entry.updated_at.to_rfc3339(),
    }
}

fn skill_kind_label(kind: SkillKind) -> &'static str {
    match kind {
        SkillKind::Analyzer => "analyzer",
        SkillKind::Generator => "generator",
        SkillKind::Linter => "linter",
        SkillKind::Context => "context",
        SkillKind::Security => "security-analyzer",
        SkillKind::Custom => "custom",
    }
}

#[tauri::command]
pub async fn marketplace_list(
    item_type: String,
    state: State<'_, WorkspaceState>,
) -> Result<MarketplaceSnapshot, String> {
    let root = current_root(&state)?;
    let client = MarketplaceClient::new().map_err(|e| format!("Failed to create marketplace client: {}", e))?;

    let index = client.fetch_index().await.map_err(|e| e.to_string())?;

    let entries = match item_type.as_str() {
        "extension" => index.extensions,
        "skill" => index.skills,
        _ => return Err(format!("Invalid type: {}. Use 'extension' or 'skill'", item_type)),
    };

    let items: Vec<MarketplaceItemView> = entries
        .into_iter()
        .take(MAX_ENTRIES)
        .map(|entry| entry_to_view(&entry))
        .collect();

    Ok(MarketplaceSnapshot {
        path: root.display().to_string(),
        extensions: if item_type == "extension" { items } else { vec![] },
        skills: if item_type == "skill" { items } else { vec![] },
        error: None,
    })
}

#[tauri::command]
pub async fn marketplace_search(
    query: String,
    item_type: String,
    state: State<'_, WorkspaceState>,
) -> Result<MarketplaceSnapshot, String> {
    let root = current_root(&state)?;
    let client = MarketplaceClient::new().map_err(|e| format!("Failed to create marketplace client: {}", e))?;

    let entries = match item_type.as_str() {
        "extension" => client.search_extensions(&query).await?,
        "skill" => client.search_skills(&query).await?,
        _ => return Err(format!("Invalid type: {}. Use 'extension' or 'skill'", item_type)),
    };

    let items: Vec<MarketplaceItemView> = entries
        .into_iter()
        .take(MAX_ENTRIES)
        .map(|entry| entry_to_view(&entry))
        .collect();

    Ok(MarketplaceSnapshot {
        path: root.display().to_string(),
        extensions: if item_type == "extension" { items } else { vec![] },
        skills: if item_type == "skill" { items } else { vec![] },
        error: None,
    })
}

#[tauri::command]
pub async fn marketplace_install(
    id: String,
    state: State<'_, WorkspaceState>,
) -> Result<MarketplaceInstallResult, String> {
    let root = current_root(&state)?;
    let client = MarketplaceClient::new().map_err(|e| format!("Failed to create marketplace client: {}", e))?;

    let entry = client
        .find_item(&id)
        .await
        .map_err(|e| e.to_string())?
        .ok_or_else(|| format!("Item {} not found in marketplace", id))?;

    // Validate before downloading
    entry.validate_for_install().map_err(|e| format!("Invalid marketplace entry: {}", e))?;

    // Determine if it's an extension or skill
    let result = match entry.kind {
        MarketplaceType::Extension => {
            let result = client.install_extension(&id).await.map_err(|e| e.to_string())?;
            MarketplaceInstallResult {
                id: id.clone(),
                kind: "extension".to_string(),
                version: entry.version,
                installed_at: chrono::Utc::now().to_rfc3339(),
                source_url: entry.asset_url,
                install_path: result.path.to_string_lossy().to_string(),
                checksum: result.checksum,
            }
        }
        MarketplaceType::Skill => {
            let path = client.install_skill(&id).await.map_err(|e| e.to_string())?;
            MarketplaceInstallResult {
                id: id.clone(),
                kind: "skill".to_string(),
                version: entry.version,
                installed_at: chrono::Utc::now().to_rfc3339(),
                source_url: entry.asset_url,
                install_path: path.to_string_lossy().to_string(),
                checksum: entry.checksum,
            }
        }
    };

    // Update lock file
    let mut lock = MarketplaceLock::load().map_err(|e| format!("Failed to load lock file: {}", e))?;
    lock.installed.push(ryoko_marketplace::InstallationRecord {
        id: id.clone(),
        kind: entry.kind,
        version: entry.version,
        installed_at: chrono::Utc::now(),
        source_url: entry.asset_url,
        install_path: result.install_path.clone(),
    });
    lock.save().map_err(|e| format!("Failed to save lock file: {}", e))?;

    Ok(result)
}

#[tauri::command]
pub async fn marketplace_check_updates(
    state: State<'_, WorkspaceState>,
) -> Result<Vec<MarketplaceUpdateInfo>, String> {
    let _root = current_root(&state)?;
    let client = MarketplaceClient::new().map_err(|e| format!("Failed to create marketplace client: {}", e))?;
    let lock = MarketplaceLock::load().map_err(|e| format!("Failed to load lock file: {}", e))?;

    let updates = client.check_for_updates(&lock).await.map_err(|e| e.to_string())?;

    let mut results = Vec::new();
    for update in updates {
        results.push(MarketplaceUpdateInfo {
            id: update.entry.id,
            name: update.entry.name,
            kind: match update.entry.kind {
                MarketplaceType::Extension => "extension".to_string(),
                MarketplaceType::Skill => "skill".to_string(),
            },
            current_version: update.current_version,
            latest_version: update.latest_version,
            is_upgrade: update.is_upgrade(),
        });
    }

    Ok(results)
}

#[tauri::command]
pub async fn marketplace_update(
    id: String,
    state: State<'_, WorkspaceState>,
) -> Result<MarketplaceInstallResult, String> {
    let root = current_root(&state)?;
    let client = MarketplaceClient::new().map_err(|e| format!("Failed to create marketplace client: {}", e))?;

    let entry = client
        .find_item(&id)
        .await
        .map_err(|e| e.to_string())?
        .ok_or_else(|| format!("Item {} not found in marketplace", id))?;

    entry.validate_for_install().map_err(|e| format!("Invalid marketplace entry: {}", e))?;

    let result = match entry.kind {
        MarketplaceType::Extension => {
            let result = client.install_extension(&id).await.map_err(|e| e.to_string())?;
            MarketplaceInstallResult {
                id: id.clone(),
                kind: "extension".to_string(),
                version: entry.version,
                installed_at: chrono::Utc::now().to_rfc3339(),
                source_url: entry.asset_url,
                install_path: result.path.to_string_lossy().to_string(),
                checksum: result.checksum,
            }
        }
        MarketplaceType::Skill => {
            let path = client.install_skill(&id).await.map_err(|e| e.to_string())?;
            MarketplaceInstallResult {
                id: id.clone(),
                kind: "skill".to_string(),
                version: entry.version,
                installed_at: chrono::Utc::now().to_rfc3339(),
                source_url: entry.asset_url,
                install_path: path.to_string_lossy().to_string(),
                checksum: entry.checksum,
            }
        }
    };

    // Update lock file
    let mut lock = MarketplaceLock::load().map_err(|e| format!("Failed to load lock file: {}", e))?;
    lock.installed.push(ryoko_marketplace::InstallationRecord {
        id: id.clone(),
        kind: entry.kind,
        version: entry.version,
        installed_at: chrono::Utc::now(),
        source_url: entry.asset_url,
        install_path: result.install_path.clone(),
    });
    lock.save().map_err(|e| format!("Failed to save lock file: {}", e))?;

    Ok(result)
}

#[tauri::command]
pub async fn marketplace_info(
    id: String,
    state: State<'_, WorkspaceState>,
) -> Result<MarketplaceItemView, String> {
    let _root = current_root(&state)?;
    let client = MarketplaceClient::new().map_err(|e| format!("Failed to create marketplace client: {}", e))?;

    let entry = client
        .find_item(&id)
        .await
        .map_err(|e| e.to_string())?
        .ok_or_else(|| format!("Item {} not found in marketplace", id))?;

    Ok(entry_to_view(&entry))
}

#[tauri::command]
pub async fn marketplace_installed(
    state: State<'_, WorkspaceState>,
) -> Result<MarketplaceInstalledView, String> {
    let _root = current_root(&state)?;
    let lock = MarketplaceLock::load().map_err(|e| format!("Failed to load lock file: {}", e))?;

    let mut extensions = Vec::new();
    let mut skills = Vec::new();

    for record in lock.installed {
        let item = MarketplaceInstalledItem {
            id: record.id,
            kind: match record.kind {
                MarketplaceType::Extension => "extension".to_string(),
                MarketplaceType::Skill => "skill".to_string(),
            },
            version: record.version,
            installed_at: record.installed_at.to_rfc3339(),
            source_url: record.source_url,
            install_path: record.install_path,
        };
        match record.kind {
            MarketplaceType::Extension => extensions.push(item),
            MarketplaceType::Skill => skills.push(item),
        }
    }

    Ok(MarketplaceInstalledView {
        extensions,
        skills,
        last_checked: lock.last_checked.to_rfc3339(),
    })
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MarketplaceInstallResult {
    pub id: String,
    pub kind: String,
    pub version: String,
    pub installed_at: String,
    pub source_url: String,
    pub install_path: String,
    pub checksum: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MarketplaceUpdateInfo {
    pub id: String,
    pub name: String,
    pub kind: String,
    pub current_version: String,
    pub latest_version: String,
    pub is_upgrade: bool,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MarketplaceInstalledItem {
    pub id: String,
    pub kind: String,
    pub version: String,
    pub installed_at: String,
    pub source_url: String,
    pub install_path: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MarketplaceInstalledView {
    pub extensions: Vec<MarketplaceInstalledItem>,
    pub skills: Vec<MarketplaceInstalledItem>,
    pub last_checked: String,
}