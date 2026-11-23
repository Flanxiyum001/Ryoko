//! AST-verified, optimistic symbol edits. This module only plans file contents;
//! the workspace lease applies the resulting multi-file proposal later.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::context::ContextExtractor;
use crate::model::ProposedChange;
use crate::workspace::validate_relative_path;

const MAX_SOURCE_BYTES: usize = 512 * 1024;
const MAX_SYMBOL_BYTES: usize = 128 * 1024;
const MAX_EDITS: usize = 32;
const MAX_EDITED_FILES: usize = 16;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SymbolEdit {
    /// Existing file path relative to the current task worktree.
    pub path: String,
    /// Exact declaration name as found by tree-sitter.
    pub symbol: String,
    /// Exact source of the declaration that the agent originally inspected.
    /// This acts as a compare-and-swap precondition.
    pub before: String,
    /// Complete replacement declaration source.
    pub after: String,
}

/// Plan all symbol edits in memory. On any stale or ambiguous symbol, nothing
/// is written; the model receives an actionable error for its next attempt.
pub fn materialize_symbol_edits(
    root: &Path,
    edits: &[SymbolEdit],
) -> Result<Vec<ProposedChange>, String> {
    if edits.is_empty() {
        return Ok(Vec::new());
    }
    if edits.len() > MAX_EDITS {
        return Err("symbol edit proposal exceeds 32 edits".into());
    }
    let root = root.canonicalize().map_err(|error| error.to_string())?;
    if !root.is_dir() {
        return Err("symbol edit workspace is not a directory".into());
    }

    let mut groups: BTreeMap<String, Vec<&SymbolEdit>> = BTreeMap::new();
    let mut casefold = BTreeSet::new();
    for edit in edits {
        let relative = validate_relative_path(&edit.path).map_err(|error| error.to_string())?;
        let normalized = relative.to_string_lossy().replace('\\', "/");
        if !groups.contains_key(&normalized) && !casefold.insert(normalized.to_ascii_lowercase()) {
            return Err(format!("case-colliding symbol edit path: {}", edit.path));
        }
        if edit.symbol.trim().is_empty()
            || edit.before.is_empty()
            || edit.after.is_empty()
            || edit.before.len() > MAX_SYMBOL_BYTES
            || edit.after.len() > MAX_SYMBOL_BYTES
        {
            return Err(format!("invalid symbol edit payload for {}", edit.path));
        }
        groups.entry(normalized).or_default().push(edit);
    }
    if groups.len() > MAX_EDITED_FILES {
        return Err("symbol edit proposal exceeds 16 files".into());
    }

    let mut output = Vec::new();
    for (path, group) in groups {
        let relative = validate_relative_path(&path).map_err(|error| error.to_string())?;
        let target = root.join(relative);
        let metadata = std::fs::symlink_metadata(&target).map_err(|error| error.to_string())?;
        if !metadata.is_file() || metadata.file_type().is_symlink() {
            return Err(format!(
                "symbol edits require a regular source file: {path}"
            ));
        }
        let canonical = target.canonicalize().map_err(|error| error.to_string())?;
        if !canonical.starts_with(&root) {
            return Err(format!("symbol edit escapes the worktree: {path}"));
        }
        if metadata.len() > MAX_SOURCE_BYTES as u64 {
            return Err(format!("source file too large for symbol editing: {path}"));
        }
        let mut source = std::fs::read_to_string(&canonical)
            .map_err(|error| format!("cannot read {path}: {error}"))?;
        if source.len() > MAX_SOURCE_BYTES {
            return Err(format!("source grew while reading: {path}"));
        }

        let mut symbols_seen = BTreeSet::new();
        for edit in group {
            if !symbols_seen.insert(&edit.symbol) {
                return Err(format!("duplicate symbol edit in {path}: {}", edit.symbol));
            }
            let mut extractor = ContextExtractor::new()
                .map_err(|error| error.to_string())?
                .with_max_snippet_bytes(MAX_SYMBOL_BYTES);
            let context = extractor
                .extract_symbols(&path, &source, &[&edit.symbol])
                .map_err(|error| format!("cannot parse {path}: {error}"))?;
            if context.has_syntax_errors {
                return Err(format!("source contains syntax errors: {path}"));
            }
            let snippets = context
                .snippets
                .iter()
                .filter(|item| item.symbol.as_deref() == Some(edit.symbol.as_str()))
                .collect::<Vec<_>>();
            if snippets.len() != 1 || snippets[0].truncated {
                return Err(format!(
                    "symbol must have one unambiguous declaration: {} in {path}",
                    edit.symbol
                ));
            }
            if snippets[0].text != edit.before {
                return Err(format!(
                    "stale source snapshot for {} in {path}; retrieve it again",
                    edit.symbol
                ));
            }
            let positions = source.match_indices(&edit.before).collect::<Vec<_>>();
            if positions.len() != 1 {
                return Err(format!(
                    "declaration source is not unique: {} in {path}",
                    edit.symbol
                ));
            }
            let offset = positions[0].0;
            source.replace_range(offset..offset + edit.before.len(), &edit.after);
            if source.len() > MAX_SOURCE_BYTES {
                return Err(format!("edited file exceeds size limit: {path}"));
            }
            let updated = extractor
                .extract(&path, &source)
                .map_err(|error| format!("cannot parse modified {path}: {error}"))?;
            if updated.has_syntax_errors {
                return Err(format!(
                    "symbol replacement introduces syntax errors: {path}"
                ));
            }
        }
        output.push(
            ProposedChange::new(path, source).with_description("AST-verified symbol replacement"),
        );
    }
    Ok(output)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn targeted_edit_preserves_unrelated_code_and_does_not_write_on_plan() {
        let dir = tempfile::tempdir().unwrap();
        let source = "fn alpha() -> i32 { 1 }\nfn beta() -> i32 { 2 }\n";
        std::fs::write(dir.path().join("lib.rs"), source).unwrap();
        let edit = SymbolEdit {
            path: "lib.rs".into(),
            symbol: "alpha".into(),
            before: "fn alpha() -> i32 { 1 }".into(),
            after: "fn alpha() -> i32 { 3 }".into(),
        };
        let changes = materialize_symbol_edits(dir.path(), &[edit]).unwrap();
        assert_eq!(changes.len(), 1);
        assert!(changes[0].content.contains("fn alpha() -> i32 { 3 }"));
        assert!(changes[0].content.contains("fn beta() -> i32 { 2 }"));
        assert_eq!(
            std::fs::read_to_string(dir.path().join("lib.rs")).unwrap(),
            source
        );
    }

    #[test]
    fn stale_and_ambiguous_targets_fail_without_mutating_files() {
        let dir = tempfile::tempdir().unwrap();
        let code = "fn run() {}\nfn run() {}\n";
        std::fs::write(dir.path().join("lib.rs"), code).unwrap();
        let edit = SymbolEdit {
            path: "lib.rs".into(),
            symbol: "run".into(),
            before: "fn run() {}".into(),
            after: "fn run() { println!(\"ok\"); }".into(),
        };
        assert!(materialize_symbol_edits(dir.path(), std::slice::from_ref(&edit)).is_err());
        std::fs::write(dir.path().join("lib.rs"), "fn run() { 4 }\n").unwrap();
        assert!(materialize_symbol_edits(dir.path(), &[edit]).is_err());
    }

    #[test]
    fn rejects_invalid_replacements_and_cross_file_partial_plans() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("one.rs"), "fn one() {}\n").unwrap();
        std::fs::write(dir.path().join("two.rs"), "fn two() {}\n").unwrap();
        let valid = SymbolEdit {
            path: "one.rs".into(),
            symbol: "one".into(),
            before: "fn one() {}".into(),
            after: "fn one() { let x = 1; }".into(),
        };
        let invalid = SymbolEdit {
            path: "two.rs".into(),
            symbol: "two".into(),
            before: "fn two() {}".into(),
            after: "fn two( {".into(),
        };
        assert!(materialize_symbol_edits(dir.path(), &[valid, invalid]).is_err());
        assert_eq!(
            std::fs::read_to_string(dir.path().join("one.rs")).unwrap(),
            "fn one() {}\n"
        );
        assert_eq!(
            std::fs::read_to_string(dir.path().join("two.rs")).unwrap(),
            "fn two() {}\n"
        );
    }

    #[cfg(unix)]
    #[test]
    fn rejects_symlink_escape_in_source_path() {
        let root = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        std::fs::write(outside.path().join("secret.rs"), "fn secret() {}\n").unwrap();
        std::os::unix::fs::symlink(outside.path(), root.path().join("linked")).unwrap();
        let edit = SymbolEdit {
            path: "linked/secret.rs".into(),
            symbol: "secret".into(),
            before: "fn secret() {}".into(),
            after: "fn secret() { 1 }".into(),
        };
        assert!(materialize_symbol_edits(root.path(), &[edit]).is_err());
    }
}
