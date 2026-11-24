//! Explicit prompt context pins (`@file`, `@symbol`, `@docs`, `@git-diff`).
//!
//! The webview parses mentions for display; this module is the trust boundary.
//! Every value is validated, bounded and wrapped as untrusted project text.
//! Symbols come from the existing SQLite index, docs come from project memory
//! and `docs/**/*.md`, and the Git diff is captured with an explicit argv.

use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use rusqlite::{Connection, OpenFlags, params};
use serde::{Deserialize, Serialize};
use tauri::State;

use crate::paths::safe_join;
use crate::workspace::{WorkspaceState, current_root};

pub(crate) const MAX_MENTIONS: usize = 8;
const MAX_FILE_BYTES: u64 = 32 * 1024;
const MAX_TOTAL_BYTES: usize = 128 * 1024;
const MAX_SYMBOL_HITS: usize = 5;
const MAX_CALLERS: usize = 10;
const MAX_SIGNATURE_CHARS: usize = 500;
const MAX_DOC_FILES: usize = 400;
const MAX_DOC_FILE_BYTES: u64 = 64 * 1024;
const MAX_DOC_SECTIONS: usize = 4;
const MAX_DOC_SECTION_BYTES: usize = 4 * 1024;
const MAX_MEMORY_BYTES: usize = 24 * 1024;
const MAX_DIFF_BYTES: usize = 64 * 1024;
const MAX_DIFF_FILES: usize = 500;
const MAX_PREVIEW_CHARS: usize = 4_000;

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ContextMention {
    pub kind: String,
    #[serde(default)]
    pub value: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum MentionKind {
    File,
    Symbol,
    Docs,
    GitDiff,
    Selection,
}

impl MentionKind {
    fn parse(value: &str) -> Option<Self> {
        match value {
            "file" => Some(Self::File),
            "symbol" => Some(Self::Symbol),
            "docs" => Some(Self::Docs),
            "git-diff" => Some(Self::GitDiff),
            "selection" => Some(Self::Selection),
            _ => None,
        }
    }
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MentionPreview {
    pub label: String,
    pub preview: String,
    pub bytes: usize,
}

fn escape_attr(value: &str) -> String {
    value
        .chars()
        .map(|character| match character {
            '<' | '>' | '"' | '&' | '\'' => '_',
            character if character.is_control() => ' ',
            character => character,
        })
        .collect()
}

/// Keeps project text from closing the surrounding context tag.
pub(crate) fn escape_context_body(value: &str) -> String {
    value.replace("</", "<\\/")
}

fn read_bounded(path: &Path, limit: u64) -> Result<String, String> {
    let file = fs::File::open(path).map_err(|error| error.to_string())?;
    let mut content = String::new();
    file.take(limit + 1)
        .read_to_string(&mut content)
        .map_err(|error| error.to_string())?;
    if content.len() as u64 > limit {
        return Err("file exceeds the context limit".to_owned());
    }
    Ok(content)
}

fn truncate_chars(value: &str, max_chars: usize) -> String {
    if value.chars().count() <= max_chars {
        return value.to_owned();
    }
    let mut output: String = value.chars().take(max_chars).collect();
    output.push('…');
    output
}

fn is_utf8_boundary(bytes: &[u8], index: usize) -> bool {
    index == 0 || index >= bytes.len() || (bytes[index] & 0b1100_0000) != 0b1000_0000
}

pub(crate) fn allowed_context_path(path: &str) -> bool {
    !path.split(['/', '\\']).any(|part| {
        let name = part.to_ascii_lowercase();
        name == ".env"
            || name.starts_with(".env.")
            || name == "credentials"
            || name == ".ssh"
            || name == ".git"
            || name == ".ryoko"
            || name == ".npmrc"
            || name == ".pypirc"
            || name == ".netrc"
            || name == ".git-credentials"
            || name == ".envrc"
            || name == ".aws"
            || name == ".gnupg"
            || name == ".docker"
            || name == ".kube"
            || name.ends_with(".pem")
            || name.ends_with(".key")
            || name.ends_with(".p12")
            || name.ends_with(".pfx")
            || name == "id_rsa"
            || name == "id_dsa"
            || name == "id_ecdsa"
            || name == "id_ed25519"
    })
}

pub(crate) fn valid_symbol_name(name: &str) -> bool {
    let mut characters = name.chars();
    let Some(first) = characters.next() else {
        return false;
    };
    if !(first.is_ascii_alphabetic() || first == '_') {
        return false;
    }
    name.len() <= 128
        && characters.all(|character| {
            character.is_ascii_alphanumeric()
                || matches!(
                    character,
                    '_' | ':' | '<' | '>' | '.' | ',' | '$' | '#' | '?' | '!' | '~' | '-'
                )
        })
}

pub(crate) fn valid_docs_query(query: &str) -> bool {
    !query.is_empty()
        && query.len() <= 120
        && !query.contains("..")
        && !query.starts_with('/')
        && query.chars().all(|character| {
            character.is_ascii_alphanumeric() || matches!(character, '_' | '.' | ':' | '/' | '-')
        })
}

fn resolve_one(root: &Path, kind: MentionKind, value: &str) -> Result<String, String> {
    match kind {
        MentionKind::File => file_context(root, value),
        MentionKind::Symbol => symbol_context(root, value),
        MentionKind::Docs => docs_context(root, value),
        MentionKind::GitDiff => git_diff_context(root),
        MentionKind::Selection => Ok(String::new()),
    }
}

/// Resolves the ordered mention list into one bounded context block.
pub(crate) fn resolve(root: &Path, mentions: &[ContextMention]) -> Result<String, String> {
    if mentions.len() > MAX_MENTIONS {
        return Err(format!("at most {MAX_MENTIONS} context pins are allowed"));
    }
    let mut seen = std::collections::HashSet::new();
    let mut output = String::new();
    for mention in mentions {
        let kind = MentionKind::parse(&mention.kind)
            .ok_or_else(|| format!("unknown context pin `{}`", mention.kind))?;
        if !seen.insert((mention.kind.clone(), mention.value.clone())) {
            continue;
        }
        let block = resolve_one(root, kind, &mention.value)?;
        if output.len().saturating_add(block.len()) > MAX_TOTAL_BYTES {
            return Err(format!(
                "context pins exceed the {} KiB prompt limit; remove one pin",
                MAX_TOTAL_BYTES / 1024
            ));
        }
        output.push_str(&block);
    }
    Ok(output)
}

pub(crate) fn file_context(root: &Path, path: &str) -> Result<String, String> {
    if path.is_empty() || path.chars().count() > 240 {
        return Err("file pin must name one workspace file".to_owned());
    }
    if !allowed_context_path(path) {
        return Err(format!("sensitive file cannot be pinned: {path}"));
    }
    let root = root
        .canonicalize()
        .map_err(|error| format!("cannot resolve the workspace: {error}"))?;
    let candidate = safe_join(&root, path)?;
    let canonical = candidate
        .canonicalize()
        .map_err(|error| format!("cannot pin {path}: {error}"))?;
    if !canonical.starts_with(&root) || !canonical.is_file() {
        return Err(format!(
            "pin escapes the workspace or is not a file: {path}"
        ));
    }
    let canonical_relative = canonical
        .strip_prefix(&root)
        .map_err(|_| format!("pin escapes the workspace: {path}"))?
        .to_string_lossy()
        .replace('\\', "/");
    if !allowed_context_path(&canonical_relative) {
        return Err(format!("sensitive file cannot be pinned: {path}"));
    }
    let metadata = fs::metadata(&canonical).map_err(|error| error.to_string())?;
    if metadata.len() > MAX_FILE_BYTES {
        return Err(format!("pinned file exceeds 32 KiB: {path}"));
    }
    let content = read_bounded(&canonical, MAX_FILE_BYTES)
        .map_err(|error| format!("cannot pin {path}: {error}"))?;
    let mut block = String::new();
    block.push_str("\n<workspace-file path=\"");
    block.push_str(&escape_attr(path));
    block.push_str("\">\n");
    block.push_str(&escape_context_body(&content));
    block.push_str("\n</workspace-file>\n");
    Ok(block)
}

fn index_path(root: &Path) -> Result<Option<PathBuf>, String> {
    let root = root
        .canonicalize()
        .map_err(|error| format!("cannot resolve the workspace: {error}"))?;
    let candidate = root.join(".ryoko/state/symbols.sqlite3");
    let metadata = match fs::symlink_metadata(&candidate) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(format!("cannot inspect the symbol index: {error}")),
    };
    if !metadata.file_type().is_file() {
        return Err("the symbol index is not a regular file".to_owned());
    }
    let canonical = candidate
        .canonicalize()
        .map_err(|error| format!("cannot resolve the symbol index: {error}"))?;
    if !canonical.starts_with(&root) {
        return Err("the symbol index escapes the open workspace".to_owned());
    }
    Ok(Some(canonical))
}

pub(crate) fn symbol_context(root: &Path, name: &str) -> Result<String, String> {
    if !valid_symbol_name(name) {
        return Err("symbol pin must be a plain identifier (up to 128 characters)".to_owned());
    }
    let Some(path) = index_path(root)? else {
        return Err("no symbol index yet; run `ryoko index` first".to_owned());
    };
    let connection = Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .map_err(|error| format!("cannot open the symbol index: {error}"))?;
    let mut statement = connection
        .prepare(
            "SELECT path, kind, signature, start_line FROM symbols \
             WHERE name = ?1 ORDER BY path, start_line LIMIT ?2",
        )
        .map_err(|error| format!("cannot query the symbol index: {error}"))?;
    let rows = statement
        .query_map(params![name, MAX_SYMBOL_HITS as i64], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, i64>(3)?,
            ))
        })
        .map_err(|error| format!("cannot query the symbol index: {error}"))?;
    let mut hits = Vec::new();
    for row in rows {
        hits.push(row.map_err(|error| format!("cannot read a symbol row: {error}"))?);
    }
    if hits.is_empty() {
        return Err(format!(
            "symbol `{name}` is not indexed; run `ryoko index` or check the spelling"
        ));
    }
    let mut callers = connection
        .prepare(
            "SELECT caller_path, caller, line FROM calls \
             WHERE callee = ?1 ORDER BY caller_path, line LIMIT ?2",
        )
        .map_err(|error| format!("cannot query symbol callers: {error}"))?;
    let mut block = String::new();
    block.push_str("\n<workspace-symbol name=\"");
    block.push_str(&escape_attr(name));
    block.push_str("\">\n");
    for (path, kind, signature, start_line) in hits {
        block.push_str("  <definition path=\"");
        block.push_str(&escape_attr(&path));
        block.push_str("\" kind=\"");
        block.push_str(&escape_attr(&kind));
        block.push_str("\" line=\"");
        block.push_str(&start_line.to_string());
        block.push_str("\">");
        block.push_str(&escape_attr(&truncate_chars(
            &signature,
            MAX_SIGNATURE_CHARS,
        )));
        block.push_str("</definition>\n");
    }
    let rows = callers
        .query_map(params![name, MAX_CALLERS as i64], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, i64>(2)?,
            ))
        })
        .map_err(|error| format!("cannot query symbol callers: {error}"))?;
    for row in rows {
        let (path, caller, line) =
            row.map_err(|error| format!("cannot read a caller row: {error}"))?;
        block.push_str("  <caller path=\"");
        block.push_str(&escape_attr(&path));
        block.push_str("\" line=\"");
        block.push_str(&line.to_string());
        block.push_str("\">");
        block.push_str(&escape_attr(&truncate_chars(&caller, 200)));
        block.push_str("</caller>\n");
    }
    block.push_str("</workspace-symbol>\n");
    Ok(block)
}

fn markdown_files(root: &Path) -> Vec<PathBuf> {
    let mut files = Vec::new();
    let docs = root.join("docs");
    if fs::symlink_metadata(&docs).is_ok_and(|metadata| metadata.file_type().is_symlink()) {
        return files;
    }
    collect_markdown(&docs, 0, &mut files);
    files.sort();
    files
}

fn collect_markdown(directory: &Path, depth: usize, files: &mut Vec<PathBuf>) {
    if depth > 4 || files.len() >= MAX_DOC_FILES {
        return;
    }
    let Ok(entries) = fs::read_dir(directory) else {
        return;
    };
    let mut entries = entries.filter_map(Result::ok).collect::<Vec<_>>();
    entries.sort_by_key(|entry| entry.file_name());
    for entry in entries {
        if files.len() >= MAX_DOC_FILES {
            return;
        }
        let Ok(file_type) = entry.file_type() else {
            continue;
        };
        if file_type.is_symlink() {
            continue;
        }
        let path = entry.path();
        if file_type.is_dir() {
            collect_markdown(&path, depth + 1, files);
        } else if file_type.is_file()
            && path
                .extension()
                .is_some_and(|extension| extension.eq_ignore_ascii_case("md"))
        {
            files.push(path);
        }
    }
}

fn memory_files(root: &Path) -> Vec<PathBuf> {
    let directory = root.join(".ryoko/memory");
    if fs::symlink_metadata(&directory).is_ok_and(|metadata| metadata.file_type().is_symlink()) {
        return Vec::new();
    }
    ["decisions.md", "conventions.md", "rejections.md"]
        .into_iter()
        .map(|name| directory.join(name))
        .filter(|path| {
            fs::symlink_metadata(path).is_ok_and(|metadata| metadata.file_type().is_file())
        })
        .collect()
}

/// Splits Markdown into heading-delimited sections. A section keeps its heading
/// so a retrieved excerpt still has a title the operator can recognise.
pub(crate) fn markdown_sections(content: &str) -> Vec<(String, String)> {
    let mut sections = Vec::new();
    let mut heading = String::new();
    let mut body = String::new();
    for line in content.lines() {
        if line.starts_with('#') {
            if !heading.is_empty() || !body.trim().is_empty() {
                sections.push((heading.clone(), body.trim().to_owned()));
            }
            heading = line.trim_start_matches('#').trim().to_owned();
            body.clear();
        } else {
            body.push_str(line);
            body.push('\n');
        }
    }
    if !heading.is_empty() || !body.trim().is_empty() {
        sections.push((heading, body.trim().to_owned()));
    }
    sections
}

fn bounded_section(body: &str) -> String {
    if body.len() <= MAX_DOC_SECTION_BYTES {
        return body.to_owned();
    }
    let mut end = MAX_DOC_SECTION_BYTES;
    while end > 0 && !body.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}\n… [section truncated]", &body[..end])
}

fn docs_context(root: &Path, query: &str) -> Result<String, String> {
    if query.is_empty() {
        let mut block = String::new();
        for path in memory_files(root) {
            let Ok(metadata) = fs::metadata(&path) else {
                continue;
            };
            if !metadata.is_file() || metadata.len() > MAX_DOC_FILE_BYTES {
                continue;
            }
            let Ok(content) = read_bounded(&path, MAX_DOC_FILE_BYTES) else {
                continue;
            };
            if block.len().saturating_add(content.len()) > MAX_MEMORY_BYTES {
                break;
            }
            let name = path
                .strip_prefix(root)
                .unwrap_or(&path)
                .to_string_lossy()
                .replace('\\', "/");
            block.push_str("\n<project-memory file=\"");
            block.push_str(&escape_attr(&name));
            block.push_str("\">\n");
            block.push_str(&escape_context_body(&content));
            block.push_str("\n</project-memory>\n");
        }
        if block.is_empty() {
            return Err("no project memory found; add `.ryoko/memory/*.md` first".to_owned());
        }
        return Ok(block);
    }
    if !valid_docs_query(query) {
        return Err(
            "docs pin must be a short word or path (letters, digits, `_ . : / -`)".to_owned(),
        );
    }
    let needle = query.to_ascii_lowercase();
    let mut files = memory_files(root);
    files.extend(markdown_files(root));
    let mut block = String::new();
    let mut sections = 0;
    for path in files {
        if sections >= MAX_DOC_SECTIONS {
            break;
        }
        let Ok(metadata) = fs::metadata(&path) else {
            continue;
        };
        if !metadata.is_file() || metadata.len() > MAX_DOC_FILE_BYTES {
            continue;
        }
        let Ok(content) = read_bounded(&path, MAX_DOC_FILE_BYTES) else {
            continue;
        };
        let name = path
            .strip_prefix(root)
            .unwrap_or(&path)
            .to_string_lossy()
            .replace('\\', "/");
        for (heading, body) in markdown_sections(&content) {
            if sections >= MAX_DOC_SECTIONS {
                break;
            }
            let haystack = format!("{heading}\n{body}").to_ascii_lowercase();
            if !haystack.contains(&needle) {
                continue;
            }
            block.push_str("\n<doc-section path=\"");
            block.push_str(&escape_attr(&name));
            block.push_str("\" heading=\"");
            block.push_str(&escape_attr(&truncate_chars(&heading, 200)));
            block.push_str("\">\n");
            block.push_str(&escape_context_body(&bounded_section(&body)));
            block.push_str("\n</doc-section>\n");
            sections += 1;
        }
    }
    if sections == 0 {
        return Err(format!("no documentation matched `{query}`"));
    }
    Ok(block)
}

fn git_root(root: &Path) -> Result<(), String> {
    let output = Command::new("git")
        .current_dir(root)
        .args(["rev-parse", "--show-toplevel"])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .output()
        .map_err(|error| format!("cannot run Git: {error}"))?;
    if !output.status.success() {
        return Err("open a Git repository to pin @git-diff".to_owned());
    }
    let top = String::from_utf8(output.stdout)
        .map_err(|_| "Git returned a non-UTF-8 repository path".to_owned())?;
    let canonical = Path::new(top.trim())
        .canonicalize()
        .map_err(|error| format!("cannot resolve the Git root: {error}"))?;
    let expected = root
        .canonicalize()
        .map_err(|error| format!("cannot resolve the open workspace: {error}"))?;
    if canonical != expected {
        return Err("open the repository root to pin @git-diff".to_owned());
    }
    Ok(())
}

fn safe_diff_path(path: &str) -> bool {
    !Path::new(path).is_absolute()
        && !path
            .split(['/', '\\'])
            .any(|part| part.is_empty() || part == "..")
        && allowed_context_path(path)
}

fn safe_diff_paths(root: &Path) -> Result<Vec<String>, String> {
    let output = Command::new("git")
        .current_dir(root)
        .args([
            "diff",
            "--name-only",
            "-z",
            "--no-ext-diff",
            "--no-textconv",
            "--ignore-submodules=all",
            "HEAD",
            "--",
            ".",
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .env("GIT_TERMINAL_PROMPT", "0")
        .output()
        .map_err(|error| format!("cannot list the Git diff: {error}"))?;
    if !output.status.success() {
        return Err("Git could not list the working-tree changes".to_owned());
    }
    let mut paths = Vec::new();
    for raw in output.stdout.split(|byte| *byte == 0) {
        if raw.is_empty() {
            continue;
        }
        let path =
            std::str::from_utf8(raw).map_err(|_| "Git returned a non-UTF-8 path".to_owned())?;
        if safe_diff_path(path) {
            paths.push(path.to_owned());
        }
        if paths.len() >= MAX_DIFF_FILES {
            break;
        }
    }
    Ok(paths)
}

pub(crate) fn git_diff_context(root: &Path) -> Result<String, String> {
    git_root(root)?;
    let paths = safe_diff_paths(root)?;
    if paths.is_empty() {
        return Err("there are no non-sensitive tracked working-tree changes to pin".to_owned());
    }
    let mut command = Command::new("git");
    command.current_dir(root).args([
        "diff",
        "--no-ext-diff",
        "--no-textconv",
        "--no-color",
        "--ignore-submodules=all",
        "HEAD",
        "--",
    ]);
    for path in &paths {
        command.arg(format!(":(literal){path}"));
    }
    let mut child = command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .env("GIT_TERMINAL_PROMPT", "0")
        .spawn()
        .map_err(|error| format!("cannot inspect the Git diff: {error}"))?;
    let mut output = Vec::new();
    child
        .stdout
        .take()
        .ok_or_else(|| "cannot read the Git diff".to_owned())?
        .take((MAX_DIFF_BYTES + 1) as u64)
        .read_to_end(&mut output)
        .map_err(|error| format!("cannot read the Git diff: {error}"))?;
    let status = child
        .wait()
        .map_err(|error| format!("cannot finish the Git diff: {error}"))?;
    if !status.success() {
        return Err("Git could not produce a working-tree diff".to_owned());
    }
    if output.is_empty() {
        return Err("there are no tracked working-tree changes to pin".to_owned());
    }
    let truncated = output.len() > MAX_DIFF_BYTES;
    let mut end = output.len().min(MAX_DIFF_BYTES);
    while end > 0 && !is_utf8_boundary(&output, end) {
        end -= 1;
    }
    let mut text = String::from_utf8_lossy(&output[..end]).into_owned();
    if truncated {
        text.push_str("\n… [diff truncated]");
    }
    Ok(format!(
        "\n<git-diff>\n{}\n</git-diff>\n",
        escape_context_body(&text)
    ))
}

/// Validates the pin and returns only a bounded excerpt for the chat chip.
/// The full block is rebuilt when the prompt is sent, so large context never
/// travels to the webview twice.
pub(crate) fn preview(root: &Path, mention: &ContextMention) -> Result<MentionPreview, String> {
    let kind = MentionKind::parse(&mention.kind)
        .ok_or_else(|| format!("unknown context pin `{}`", mention.kind))?;
    let block = resolve_one(root, kind, &mention.value)?;
    let label = match kind {
        MentionKind::File => mention.value.clone(),
        MentionKind::Symbol => mention.value.clone(),
        MentionKind::Docs if mention.value.is_empty() => "project docs".to_owned(),
        MentionKind::Docs => format!("docs: {}", mention.value),
        MentionKind::GitDiff => "git diff".to_owned(),
        MentionKind::Selection => "selection".to_owned(),
    };
    Ok(MentionPreview {
        label,
        preview: truncate_chars(block.trim(), MAX_PREVIEW_CHARS),
        bytes: block.len(),
    })
}

#[tauri::command]
pub fn resolve_context_mention(
    kind: String,
    value: String,
    state: State<'_, WorkspaceState>,
) -> Result<MentionPreview, String> {
    let root = current_root(&state)?;
    preview(&root, &ContextMention { kind, value })
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SymbolSuggestion {
    pub name: String,
    pub path: String,
    pub kind: String,
}

#[tauri::command]
pub fn search_context_symbols(
    query: String,
    state: State<'_, WorkspaceState>,
) -> Result<Vec<SymbolSuggestion>, String> {
    if query.len() > 64 || query.chars().any(char::is_control) {
        return Err("symbol query must be at most 64 characters".to_owned());
    }
    let root = current_root(&state)?;
    let Some(path) = index_path(&root)? else {
        return Ok(Vec::new());
    };
    let connection = Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .map_err(|error| format!("cannot open the symbol index: {error}"))?;
    let pattern = format!("%{}%", query.replace(['%', '_'], ""));
    let mut statement = connection
        .prepare(
            "SELECT name, path, kind FROM symbols WHERE name LIKE ?1 \
             ORDER BY name, path LIMIT 20",
        )
        .map_err(|error| format!("cannot query the symbol index: {error}"))?;
    let rows = statement
        .query_map(params![pattern], |row| {
            Ok(SymbolSuggestion {
                name: row.get(0)?,
                path: row.get(1)?,
                kind: row.get(2)?,
            })
        })
        .map_err(|error| format!("cannot query the symbol index: {error}"))?;
    let mut suggestions = Vec::new();
    for row in rows {
        suggestions.push(row.map_err(|error| format!("cannot read a symbol row: {error}"))?);
    }
    Ok(suggestions)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_root(name: &str) -> PathBuf {
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path = std::env::temp_dir().join(format!("ryoko-mentions-{name}-{stamp}"));
        let _ = fs::remove_dir_all(&path);
        fs::create_dir_all(&path).unwrap();
        path
    }

    #[test]
    fn symbol_names_are_strict_and_bounded() {
        assert!(valid_symbol_name("WorkspaceIndex"));
        assert!(valid_symbol_name("std::io::Read"));
        assert!(!valid_symbol_name(""));
        assert!(!valid_symbol_name("9lives"));
        assert!(!valid_symbol_name("bad name"));
        assert!(!valid_symbol_name(&"a".repeat(200)));
    }

    #[test]
    fn docs_queries_reject_traversal_and_spaces() {
        assert!(valid_docs_query("architecture/EDITOR"));
        assert!(!valid_docs_query(""));
        assert!(!valid_docs_query("../secret"));
        assert!(!valid_docs_query("two words"));
    }

    #[test]
    fn attributes_cannot_close_the_context_tag() {
        assert_eq!(escape_attr("a\"b<c>d&e"), "a_b_c_d_e");
        assert_eq!(
            escape_context_body("before</workspace-file>after"),
            "before<\\/workspace-file>after"
        );
    }

    #[test]
    fn markdown_sections_keep_their_headings() {
        let sections = markdown_sections("# One\nbody\n## Two\nmore\n");
        assert_eq!(sections[0].0, "One");
        assert_eq!(sections[0].1, "body");
        assert_eq!(sections[1].0, "Two");
        assert_eq!(sections[1].1, "more");
    }

    #[test]
    fn file_context_rejects_escape_and_sensitive_paths() {
        let root = temp_root("file");
        fs::write(root.join("ok.rs"), "fn main() {}\n").unwrap();
        assert!(file_context(&root, "ok.rs").unwrap().contains("fn main"));
        assert!(file_context(&root, "../escape.rs").is_err());
        assert!(file_context(&root, ".env").is_err());
        assert!(file_context(&root, "missing.rs").is_err());
        assert!(file_context(&root, &"a".repeat(241)).is_err());
        fs::write(
            root.join("large.txt"),
            "x".repeat(MAX_FILE_BYTES as usize + 1),
        )
        .unwrap();
        assert!(
            file_context(&root, "large.txt")
                .unwrap_err()
                .contains("exceeds 32 KiB")
        );
        fs::write(root.join(".env"), "SECRET=1\n").unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink(root.join(".env"), root.join("notes.txt")).unwrap();
        #[cfg(unix)]
        assert!(file_context(&root, "notes.txt").is_err());
        fs::remove_dir_all(root).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn symlinked_workspace_root_is_canonicalized() {
        let root = temp_root("canonical-root");
        fs::write(root.join("ok.rs"), "fn main() {}\n").unwrap();
        fs::create_dir_all(root.join(".ryoko/state")).unwrap();
        fs::write(root.join(".ryoko/state/symbols.sqlite3"), b"").unwrap();
        let alias = root.with_file_name(format!(
            "{}-alias",
            root.file_name().unwrap().to_string_lossy()
        ));
        let _ = fs::remove_dir_all(&alias);
        std::os::unix::fs::symlink(&root, &alias).unwrap();
        assert!(file_context(&alias, "ok.rs").unwrap().contains("fn main"));
        assert!(index_path(&alias).unwrap().is_some());
        fs::remove_dir_all(&alias).unwrap();
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn symbol_context_reads_definitions_and_callers() {
        let root = temp_root("symbol");
        fs::create_dir_all(root.join(".ryoko/state")).unwrap();
        let db = root.join(".ryoko/state/symbols.sqlite3");
        let connection = Connection::open(&db).unwrap();
        connection
            .execute_batch(
                "CREATE TABLE symbols (path TEXT, name TEXT, kind TEXT, signature TEXT, start_line INTEGER, end_line INTEGER);
                 CREATE TABLE calls (caller_path TEXT, caller TEXT, callee TEXT, line INTEGER);
                 INSERT INTO symbols VALUES ('src/lib.rs','run','function','pub fn run()',10,12);
                 INSERT INTO calls VALUES ('src/main.rs','main','run',4);",
            )
            .unwrap();
        drop(connection);
        let block = symbol_context(&root, "run").unwrap();
        assert!(block.contains("src/lib.rs"));
        assert!(block.contains("src/main.rs"));
        assert!(symbol_context(&root, "missing").is_err());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn docs_context_finds_a_matching_section() {
        let root = temp_root("docs");
        fs::create_dir_all(root.join("docs")).unwrap();
        fs::write(
            root.join("docs/security.md"),
            "# Threat model\nNo secrets in prompts.\n# Other\nNothing.\n",
        )
        .unwrap();
        let block = docs_context(&root, "secrets").unwrap();
        assert!(block.contains("Threat model"));
        assert!(block.contains("No secrets"));
        assert!(docs_context(&root, "absent").is_err());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn docs_directory_does_not_follow_symlinks() {
        let root = temp_root("docs-link");
        let outside = root.join("outside");
        fs::create_dir_all(&outside).unwrap();
        fs::write(outside.join("secret.md"), "# Secret\nSECRET=1\n").unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink(&outside, root.join("docs")).unwrap();
        #[cfg(unix)]
        assert!(docs_context(&root, "secret").is_err());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn project_memory_does_not_follow_symlinks() {
        let root = temp_root("memory-link");
        fs::create_dir_all(root.join(".ryoko/memory")).unwrap();
        fs::write(root.join(".env"), "SECRET=1\n").unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink(root.join(".env"), root.join(".ryoko/memory/decisions.md"))
            .unwrap();
        #[cfg(unix)]
        assert!(docs_context(&root, "").is_err());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn git_diff_omits_sensitive_tracked_files() {
        let root = temp_root("git-diff");
        let git = |args: &[&str]| {
            Command::new("git")
                .current_dir(&root)
                .args(args)
                .env("GIT_TERMINAL_PROMPT", "0")
                .output()
                .unwrap()
        };
        assert!(git(&["init", "-q"]).status.success());
        assert!(
            git(&["config", "user.email", "tests@example.com"])
                .status
                .success()
        );
        assert!(
            git(&["config", "user.name", "Ryoko Tests"])
                .status
                .success()
        );
        fs::write(root.join("keep.rs"), "pub fn keep() {}\n").unwrap();
        fs::write(root.join(".env"), "SECRET=one\n").unwrap();
        assert!(git(&["add", "-f", "."]).status.success());
        assert!(git(&["commit", "-qm", "initial"]).status.success());
        fs::write(root.join("keep.rs"), "pub fn keep() { changed(); }\n").unwrap();
        fs::write(root.join(".env"), "SECRET=two\n").unwrap();
        let block = git_diff_context(&root).unwrap();
        assert!(block.contains("keep.rs"));
        assert!(block.contains("changed"));
        assert!(!block.contains("SECRET=two"));
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn resolve_caps_the_number_of_pins() {
        let root = temp_root("cap");
        let mentions = (0..=MAX_MENTIONS)
            .map(|index| ContextMention {
                kind: "symbol".into(),
                value: format!("s{index}"),
            })
            .collect::<Vec<_>>();
        assert!(resolve(&root, &mentions).is_err());
        fs::remove_dir_all(root).unwrap();
    }
}
