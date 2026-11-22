//! IPC commands for the editor workbench: git change list, workspace search
//! and replace, targeted inline edits, and settings persistence.
//!
//! Payloads are versioned and typed per `docs/protocols/DESKTOP_WORKBENCH.md`.
//! Parsing and validation rules live in the pure sibling modules (`search`,
//! `servers`, `git_status`, `inline_edit`, `settings`) so they stay
//! unit-testable without a Tauri runtime.

use std::collections::BTreeMap;
use std::fs;
use std::hash::{Hash, Hasher};
use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use tauri::State;

use crate::git_status::{self, ChangeEntry};
use crate::inline_edit::{self, InlineEditRequest};
use crate::search::{self, SearchHit};
use crate::servers::LspRegistry;
use crate::settings;
use crate::workspace::{WorkspaceState, current_root};

pub const WORKBENCH_PROTOCOL_VERSION: u32 = 1;
const MAX_FILE_BYTES: u64 = 2 * 1024 * 1024;
const MAX_REPLACE_FILES: usize = 200;
const MAX_INLINE_CHARS: usize = 12_000;

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ChangesSnapshot {
    pub protocol: u32,
    pub entries: Vec<ChangeEntry>,
    pub is_repository: bool,
}

#[tauri::command]
pub fn git_changes(state: State<'_, WorkspaceState>) -> Result<ChangesSnapshot, String> {
    let root = current_root(&state)?;
    match git_status::working_tree_changes(&root) {
        Some(entries) => Ok(ChangesSnapshot {
            protocol: WORKBENCH_PROTOCOL_VERSION,
            entries,
            is_repository: true,
        }),
        None => Ok(ChangesSnapshot {
            protocol: WORKBENCH_PROTOCOL_VERSION,
            entries: Vec::new(),
            is_repository: false,
        }),
    }
}

// ---------------------------------------------------------------------------
// Search and replace
// ---------------------------------------------------------------------------

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SearchReply {
    pub protocol: u32,
    pub hits: Vec<SearchHit>,
    pub truncated: bool,
}

fn walker(root: &std::path::Path) -> ignore::WalkBuilder {
    let mut builder = ignore::WalkBuilder::new(root);
    builder
        .hidden(false)
        .git_ignore(true)
        .git_global(false)
        .git_exclude(false)
        .parents(false)
        .follow_links(false)
        .filter_entry(|entry| {
            entry.depth() == 0
                || !matches!(
                    entry.file_name().to_str(),
                    Some(".git" | ".ryoko" | "target" | "node_modules")
                )
        });
    builder
}

fn relative_of(root: &std::path::Path, path: &std::path::Path) -> Option<String> {
    path.strip_prefix(root)
        .ok()
        .map(|relative| relative.to_string_lossy().replace('\\', "/"))
}

#[tauri::command]
pub fn workspace_search(
    query: String,
    case_sensitive: bool,
    state: State<'_, WorkspaceState>,
) -> Result<SearchReply, String> {
    let root = current_root(&state)?;
    let regex = search::compile_query(&query, case_sensitive)?;
    let mut hits = Vec::new();
    let mut truncated = false;
    'files: for entry in walker(&root).build().filter_map(Result::ok) {
        if !entry.file_type().is_some_and(|kind| kind.is_file()) {
            continue;
        }
        let path = entry.path();
        let Ok(metadata) = entry.metadata() else {
            continue;
        };
        if metadata.len() > MAX_FILE_BYTES {
            continue;
        }
        let Ok(bytes) = fs::read(path) else {
            continue;
        };
        let Ok(text) = String::from_utf8(bytes) else {
            continue;
        };
        for (line, start, end) in search::hit_spans(&regex, &text, search::MAX_HITS_PER_FILE) {
            if hits.len() >= search::MAX_RESULTS {
                truncated = true;
                break 'files;
            }
            let Some(source) = search::line_preview(&text, line) else {
                continue;
            };
            hits.push(SearchHit {
                path: relative_of(&root, path).unwrap_or_default(),
                line,
                start,
                end,
                preview: source,
            });
        }
    }
    Ok(SearchReply {
        protocol: WORKBENCH_PROTOCOL_VERSION,
        hits,
        truncated,
    })
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ReplacePreview {
    pub path: String,
    pub match_count: usize,
    /// The first match's preview line, so apply can refuse a file whose
    /// content drifted even when the count happens to be identical.
    pub first_preview: String,
    /// Full original content fingerprint, not just the first matching line.
    pub content_hash: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ReplaceReply {
    pub protocol: u32,
    pub previews: Vec<ReplacePreview>,
    pub total_matches: usize,
}

#[derive(Debug, Clone)]
struct ReplacePlan {
    path: PathBuf,
    new_text: String,
    match_count: usize,
    first_preview: String,
    content_hash: String,
}

fn fingerprint_text(text: &str) -> String {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    text.hash(&mut hasher);
    format!("{:016x}", hasher.finish())
}

fn replace_plans(root: &std::path::Path, regex: &regex::Regex, template: &str) -> Vec<ReplacePlan> {
    let mut plans = Vec::new();
    for entry in walker(root).build().filter_map(Result::ok) {
        if plans.len() >= MAX_REPLACE_FILES {
            break;
        }
        if !entry.file_type().is_some_and(|kind| kind.is_file()) {
            continue;
        }
        let path = entry.path();
        let Ok(metadata) = entry.metadata() else {
            continue;
        };
        if metadata.len() > MAX_FILE_BYTES {
            continue;
        }
        let Ok(bytes) = fs::read(path) else {
            continue;
        };
        let Ok(text) = String::from_utf8(bytes) else {
            continue;
        };
        if let Some(plan) = search::replacement(&text, regex, template) {
            let first_preview = search::hit_spans(regex, &text, 1)
                .first()
                .and_then(|(line, _, _)| search::line_preview(&text, *line))
                .unwrap_or_default();
            plans.push(ReplacePlan {
                path: path.to_path_buf(),
                new_text: plan.new_text,
                match_count: plan.match_count,
                first_preview,
                content_hash: fingerprint_text(&text),
            });
        }
    }
    plans
}

/// Dry run: which files would change and how often, without writing anything.
#[tauri::command]
pub fn workspace_replace_preview(
    query: String,
    replacement: String,
    case_sensitive: bool,
    state: State<'_, WorkspaceState>,
) -> Result<ReplaceReply, String> {
    let root = current_root(&state)?;
    let regex = search::compile_query(&query, case_sensitive)?;
    let previews: Vec<ReplacePreview> = replace_plans(&root, &regex, &replacement)
        .into_iter()
        .map(|plan| ReplacePreview {
            path: relative_of(&root, &plan.path).unwrap_or_default(),
            match_count: plan.match_count,
            first_preview: plan.first_preview,
            content_hash: plan.content_hash,
        })
        .collect();
    Ok(ReplaceReply {
        protocol: WORKBENCH_PROTOCOL_VERSION,
        total_matches: previews.iter().map(|preview| preview.match_count).sum(),
        previews,
    })
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ReplaceResult {
    pub protocol: u32,
    pub applied_files: usize,
    pub applied_matches: usize,
    pub skipped: Vec<String>,
}

/// Applies exactly what `workspace_replace_preview` showed. Every file is
/// re-read and its plan recomputed at apply time; any drift against the
/// preview (count, first line, or a per-file drift marker) skips the file.
#[tauri::command]
pub fn workspace_replace_apply(
    query: String,
    replacement: String,
    case_sensitive: bool,
    expected: Vec<ReplacePreview>,
    state: State<'_, WorkspaceState>,
) -> Result<ReplaceResult, String> {
    let root = current_root(&state)?;
    let regex = search::compile_query(&query, case_sensitive)?;
    let expected_counts = expected
        .clone()
        .into_iter()
        .map(|preview| (preview.path.clone(), preview))
        .collect::<BTreeMap<_, _>>();
    let mut result = ReplaceResult {
        protocol: WORKBENCH_PROTOCOL_VERSION,
        applied_files: 0,
        applied_matches: 0,
        skipped: Vec::new(),
    };
    let mut applied_paths = std::collections::BTreeSet::new();
    for plan in replace_plans(&root, &regex, &replacement) {
        let Some(relative) = relative_of(&root, &plan.path) else {
            continue;
        };
        let Some(expectation) = expected_counts.get(&relative) else {
            result
                .skipped
                .push(format!("{relative}: appeared after the preview"));
            continue;
        };
        if expectation.match_count != plan.match_count
            || expectation.first_preview != plan.first_preview
            || expectation.content_hash != plan.content_hash
        {
            result
                .skipped
                .push(format!("{relative}: changed since the preview"));
            continue;
        }
        match write_file_content(&plan.path, &plan.new_text, &plan.content_hash) {
            Ok(()) => {
                result.applied_files += 1;
                result.applied_matches += plan.match_count;
                applied_paths.insert(relative);
            }
            Err(error) => result.skipped.push(format!("{relative}: {error}")),
        }
    }
    for preview in expected {
        if !applied_paths.contains(&preview.path) {
            let reported = result
                .skipped
                .iter()
                .any(|skip| skip.starts_with(preview.path.as_str()));
            if !reported {
                result
                    .skipped
                    .push(format!("{}: no longer matches", preview.path));
            }
        }
    }
    Ok(result)
}

fn write_file_content(
    target: &std::path::Path,
    content: &str,
    expected_hash: &str,
) -> Result<(), String> {
    let metadata = fs::symlink_metadata(target).map_err(|error| error.to_string())?;
    if !metadata.file_type().is_file() {
        return Err("replace target is not a regular file".to_owned());
    }
    let original = fs::read_to_string(target).map_err(|error| error.to_string())?;
    if fingerprint_text(&original) != expected_hash {
        return Err("file changed since the replacement preview".to_owned());
    }
    let parent = target
        .parent()
        .ok_or_else(|| "file has no parent directory".to_owned())?;
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or_default();
    let temp = parent.join(format!(
        ".ryoko-replace-{}-{}.tmp",
        std::process::id(),
        stamp
    ));
    let write = (|| -> std::io::Result<()> {
        use std::io::Write;
        let mut options = fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options.open(&temp)?;
        file.write_all(content.as_bytes())?;
        file.set_permissions(metadata.permissions())?;
        file.sync_all()
    })();
    if let Err(error) = write {
        let _ = fs::remove_file(&temp);
        return Err(error.to_string());
    }
    // Catch edits that landed during the temporary-file write as well.
    let unchanged = fs::symlink_metadata(target).is_ok_and(|metadata| metadata.is_file())
        && fs::read_to_string(target)
            .is_ok_and(|latest| fingerprint_text(&latest) == expected_hash);
    if !unchanged {
        let _ = fs::remove_file(&temp);
        return Err("file changed during replacement".to_owned());
    }
    fs::rename(&temp, target).map_err(|error| {
        let _ = fs::remove_file(&temp);
        error.to_string()
    })
}

// ---------------------------------------------------------------------------
// Inline edits (Cmd+K)
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InlineEditArgs {
    pub path: String,
    pub language: String,
    pub instructions: String,
    pub before: String,
    pub selected: String,
    pub after: String,
    #[serde(default)]
    pub model: Option<String>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct InlineEditReply {
    pub protocol: u32,
    pub replacement: String,
    pub model: String,
}

/// One targeted model edit. The provider catalog comes from the per-workspace
/// settings file (`.ryoko/desktop/providers.json`) or the environment, so no
/// key is ever passed through IPC.
#[tauri::command]
pub async fn inline_edit(
    args: InlineEditArgs,
    state: State<'_, WorkspaceState>,
) -> Result<InlineEditReply, String> {
    if args.instructions.trim().is_empty() {
        return Err("inline edit instructions cannot be empty".to_owned());
    }
    if args.instructions.chars().count() > MAX_INLINE_CHARS
        || args.selected.chars().count() > MAX_INLINE_CHARS
        || args.before.chars().count() > MAX_INLINE_CHARS
        || args.after.chars().count() > MAX_INLINE_CHARS
    {
        return Err("inline edit context exceeds the size limit".to_owned());
    }
    let root = current_root(&state)?;
    let (model, router) = crate::model_runtime::load_router(&root, args.model.as_deref())?;
    let request = InlineEditRequest {
        instructions: args.instructions,
        language: args.language,
        path: args.path,
        before: args.before,
        selected: args.selected,
        after: args.after,
    };
    let prompt = inline_edit::build_prompt(&request);
    let chat = ryoko_models::ChatRequest::new(&model)
        .with_system(
            "You are an inline code editor embedded in a desktop IDE. Return the \
             rewritten selection only, as plain UTF-8 text without commentary.",
        )
        .with_user(prompt);
    let completion = router
        .complete(chat)
        .await
        .map_err(|error| format!("inline edit failed: {error}"))?;
    let replacement = inline_edit::validate_answer(&completion.text)?;
    Ok(InlineEditReply {
        protocol: WORKBENCH_PROTOCOL_VERSION,
        replacement,
        model,
    })
}

// ---------------------------------------------------------------------------
// Settings
// ---------------------------------------------------------------------------

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SettingsSnapshot {
    pub protocol: u32,
    pub path: String,
    pub providers: Option<String>,
    pub lsp_servers: Option<String>,
}

#[tauri::command]
pub fn settings_read(state: State<'_, WorkspaceState>) -> Result<SettingsSnapshot, String> {
    let root = current_root(&state)?;
    Ok(SettingsSnapshot {
        protocol: WORKBENCH_PROTOCOL_VERSION,
        path: settings::settings_dir(&root).display().to_string(),
        providers: settings::load_providers(&root)?,
        lsp_servers: settings::load_lsp_registry(&root)?,
    })
}

#[tauri::command]
pub fn settings_save_providers(
    json: String,
    state: State<'_, WorkspaceState>,
) -> Result<String, String> {
    let root = current_root(&state)?;
    settings::save_providers(&root, &json)
}

#[tauri::command]
pub fn settings_save_lsp_servers(
    json: String,
    state: State<'_, WorkspaceState>,
) -> Result<String, String> {
    let root = current_root(&state)?;
    let registry = LspRegistry::parse(&json)?;
    let canonical = serde_json::to_string_pretty(&registry)
        .map_err(|error| format!("cannot serialize registry: {error}"))?;
    settings::save_lsp_registry(&root, &canonical)?;
    Ok(canonical)
}

#[tauri::command]
pub fn workbench_protocol_version() -> u32 {
    WORKBENCH_PROTOCOL_VERSION
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn full_content_hash_detects_edits_with_an_unchanged_preview() {
        let original = "needle on first line\\nkeep this";
        let changed = "needle on first line\\nchanged!!";
        assert_ne!(fingerprint_text(original), fingerprint_text(changed));
    }
}
