//! Browse-only marketplace catalog backed by the bundled Ryoko CLI.
//! No untrusted marketplace package can be installed from this surface.

use std::process::{Command, Stdio};

use serde::Serialize;
use tauri::State;

use crate::run::resolve_cli;
use crate::workspace::{WorkspaceState, current_root};

const MAX_CATALOG_BYTES: usize = 4 * 1024 * 1024;
const MAX_ITEMS: usize = 100;

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CatalogItem {
    id: String,
    name: String,
    description: String,
    author: String,
    version: String,
    kind: String,
}

fn required_text<'a>(record: &'a serde_json::Value, field: &str) -> Result<&'a str, String> {
    record
        .get(field)
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| format!("registry entry missing {field}"))
}

#[tauri::command]
pub async fn marketplace_catalog(
    kind: String,
    workspace: State<'_, WorkspaceState>,
) -> Result<Vec<CatalogItem>, String> {
    if kind != "extension" && kind != "skill" {
        return Err("marketplace catalog kind must be extension or skill".into());
    }
    let root = current_root(&workspace)?;
    tauri::async_runtime::spawn_blocking(move || {
        let cli = resolve_cli(&root)?;
        let result = Command::new(cli)
            .current_dir(crate::paths::host_path(&root))
            .args(["marketplace", "list", &kind, "--json"])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .output()
            .map_err(|error| format!("cannot launch marketplace catalog: {error}"))?;
        if !result.status.success() {
            return Err(format!(
                "Marketplace registry unavailable: {}",
                String::from_utf8_lossy(&result.stderr)
                    .chars()
                    .take(300)
                    .collect::<String>()
            ));
        }
        if result.stdout.len() > MAX_CATALOG_BYTES {
            return Err("marketplace registry response exceeds 4 MiB".to_owned());
        }
        let response: serde_json::Value = serde_json::from_slice(&result.stdout)
            .map_err(|error| format!("invalid marketplace catalog JSON: {error}"))?;
        let records = response
            .as_array()
            .ok_or_else(|| "marketplace catalog is not a JSON array".to_owned())?;
        let mut items = Vec::new();
        for record in records.iter().take(MAX_ITEMS) {
            let item_kind = required_text(record, "kind")?;
            if item_kind != kind {
                continue;
            }
            items.push(CatalogItem {
                id: required_text(record, "id")?.chars().take(128).collect(),
                name: required_text(record, "name")?.chars().take(160).collect(),
                description: required_text(record, "description")?
                    .chars()
                    .take(420)
                    .collect(),
                author: required_text(record, "author")?.chars().take(100).collect(),
                version: required_text(record, "version")?.chars().take(80).collect(),
                kind: item_kind.to_owned(),
            });
        }
        Ok(items)
    })
    .await
    .map_err(|error| format!("marketplace catalog task failed: {error}"))?
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn catalog_fields_must_be_present_and_strings() {
        let entry = serde_json::json!({"id":"safe", "name":"Example"});
        assert_eq!(required_text(&entry, "id").unwrap(), "safe");
        assert!(required_text(&entry, "missing").is_err());
        assert!(required_text(&serde_json::json!({"id": 42}), "id").is_err());
    }
}
