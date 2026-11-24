//! Human-editable, workspace-scoped project memory.
//!
//! Memory lives in `.ryoko/memory/{decisions,conventions,rejections}.md`. Each
//! file uses `## ` headings for entries and freeform Markdown below each
//! heading, so an operator can edit it without a special tool. Memory is
//! untrusted project text: it is bounded, never executed, and only included in
//! prompts as reference material.
//!
//! This is deliberately a small, explicit store rather than an automatic
//! memory system. Agents may propose changes through normal review, but only
//! a human edits these files or the CLI records an explicit rejection.

use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use crate::error::{CoreError, CoreResult};
use serde::Serialize;

const MAX_MEMORY_BYTES: usize = 256 * 1024;
const MAX_ENTRY_BYTES: usize = 8 * 1024;
const MAX_ENTRIES: usize = 100;
const MAX_TITLE_CHARS: usize = 120;

/// A category of persistent project memory.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum MemoryKind {
    Decision,
    Convention,
    Rejection,
}

impl MemoryKind {
    pub fn label(self) -> &'static str {
        match self {
            Self::Decision => "decisions",
            Self::Convention => "conventions",
            Self::Rejection => "review rejections",
        }
    }

    fn file(self) -> &'static str {
        match self {
            Self::Decision => "decisions.md",
            Self::Convention => "conventions.md",
            Self::Rejection => "rejections.md",
        }
    }
}

/// One `## ` section from a memory file.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct MemoryEntry {
    pub title: String,
    pub body: String,
}

/// Loaded project memory. Missing files are an empty section, not an error.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct ProjectMemory {
    pub decisions: Vec<MemoryEntry>,
    pub conventions: Vec<MemoryEntry>,
    pub rejections: Vec<MemoryEntry>,
}

impl ProjectMemory {
    /// Loads `.ryoko/memory/` beneath `root`. Symlinked memory files and
    /// directories are rejected, and each file is capped at 256 KiB.
    pub fn load(root: &Path) -> CoreResult<Self> {
        let root = canonical_root(root)?;
        let directory = root.join(".ryoko/memory");
        let mut memory = Self::default();
        for (kind, entries) in [
            (MemoryKind::Decision, &mut memory.decisions),
            (MemoryKind::Convention, &mut memory.conventions),
            (MemoryKind::Rejection, &mut memory.rejections),
        ] {
            let path = directory.join(kind.file());
            if let Some(text) = read_memory_file(&path)? {
                *entries = parse_entries(&text);
            }
        }
        Ok(memory)
    }

    /// Renders a compact, bounded reference block for an agent prompt.
    pub fn render_for_context(&self, max_bytes: usize) -> String {
        let max_bytes = max_bytes.min(MAX_MEMORY_BYTES);
        if max_bytes == 0 {
            return String::new();
        }
        let sections = [
            (MemoryKind::Decision, &self.decisions),
            (MemoryKind::Convention, &self.conventions),
            (MemoryKind::Rejection, &self.rejections),
        ];
        let mut output = String::new();
        for (kind, entries) in sections {
            if entries.is_empty() {
                continue;
            }
            let heading = format!("## Project memory: {}\n", kind.label());
            if output.len().saturating_add(heading.len()) >= max_bytes {
                break;
            }
            output.push_str(&heading);
            let mut ordered: Vec<&MemoryEntry> = entries.iter().collect();
            if kind == MemoryKind::Rejection {
                ordered.reverse();
            }
            for entry in ordered {
                let block = format!("### {}\n{}\n\n", entry.title, entry.body.trim());
                if output.len().saturating_add(block.len()) > max_bytes {
                    let remaining = max_bytes.saturating_sub(output.len());
                    if remaining > 32 {
                        output.push_str(&truncate(&block, remaining));
                    }
                    return output;
                }
                output.push_str(&block);
            }
        }
        output
    }

    /// Records a human review rejection at the top of `rejections.md`.
    /// Writes are private on Unix and keeps the newest entries when the file
    /// reaches its size cap.
    pub fn append_rejection(root: &Path, title: &str, body: &str) -> CoreResult<()> {
        let root = canonical_root(root)?;
        let directory = ensure_memory_dir(&root)?;
        let path = directory.join(MemoryKind::Rejection.file());
        if path
            .symlink_metadata()
            .is_ok_and(|metadata| metadata.file_type().is_symlink())
        {
            return Err(memory_error(format!(
                "{} must not be a symlink",
                path.display()
            )));
        }
        let existing = read_memory_file(&path)?.unwrap_or_default();
        let title = clean_title(title);
        let body = truncate(body.trim(), MAX_ENTRY_BYTES);
        let mut content = format!("## {title}\n{body}\n\n");
        content.push_str(&existing);
        write_private(&path, &truncate(&content, MAX_MEMORY_BYTES))
    }

    /// Records an architectural decision at the end of `decisions.md`.
    /// Decisions are stored chronologically (oldest first) to preserve context.
    /// Writes are private on Unix and keeps the newest entries when the file
    /// reaches its size cap.
    pub fn append_decision(root: &Path, title: &str, body: &str) -> CoreResult<()> {
        let root = canonical_root(root)?;
        let directory = ensure_memory_dir(&root)?;
        let path = directory.join(MemoryKind::Decision.file());
        if path
            .symlink_metadata()
            .is_ok_and(|metadata| metadata.file_type().is_symlink())
        {
            return Err(memory_error(format!(
                "{} must not be a symlink",
                path.display()
            )));
        }
        let existing = read_memory_file(&path)?.unwrap_or_default();
        let title = clean_title(title);
        let body = truncate(body.trim(), MAX_ENTRY_BYTES);
        let mut content = existing;
        if !content.is_empty() && !content.ends_with('\n') {
            content.push('\n');
        }
        content.push_str(&format!("\n## {title}\n{body}\n"));
        write_private(&path, &truncate(&content, MAX_MEMORY_BYTES))
    }

    /// Records a codebase convention at the end of `conventions.md`.
    /// Conventions are stored chronologically (oldest first) to preserve context.
    /// Writes are private on Unix and keeps the newest entries when the file
    /// reaches its size cap.
    pub fn append_convention(root: &Path, title: &str, body: &str) -> CoreResult<()> {
        let root = canonical_root(root)?;
        let directory = ensure_memory_dir(&root)?;
        let path = directory.join(MemoryKind::Convention.file());
        if path
            .symlink_metadata()
            .is_ok_and(|metadata| metadata.file_type().is_symlink())
        {
            return Err(memory_error(format!(
                "{} must not be a symlink",
                path.display()
            )));
        }
        let existing = read_memory_file(&path)?.unwrap_or_default();
        let title = clean_title(title);
        let body = truncate(body.trim(), MAX_ENTRY_BYTES);
        let mut content = existing;
        if !content.is_empty() && !content.ends_with('\n') {
            content.push('\n');
        }
        content.push_str(&format!("\n## {title}\n{body}\n"));
        write_private(&path, &truncate(&content, MAX_MEMORY_BYTES))
    }
}

fn canonical_root(root: &Path) -> CoreResult<PathBuf> {
    root.canonicalize()
        .map_err(|error| memory_error(format!("cannot resolve project root: {error}")))
}

fn memory_error(message: String) -> CoreError {
    CoreError::Internal(format!("project memory: {message}"))
}

fn read_memory_file(path: &Path) -> CoreResult<Option<String>> {
    let metadata = match std::fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(memory_error(format!(
                "cannot inspect {}: {error}",
                path.display()
            )));
        }
    };
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err(memory_error(format!(
            "{} is not a regular file",
            path.display()
        )));
    }
    if metadata.len() > MAX_MEMORY_BYTES as u64 {
        return Err(memory_error(format!(
            "{} exceeds the 256 KiB limit",
            path.display()
        )));
    }
    let mut bytes = Vec::new();
    std::fs::File::open(path)
        .and_then(|file| {
            file.take(MAX_MEMORY_BYTES as u64 + 1)
                .read_to_end(&mut bytes)
        })
        .map_err(|error| memory_error(format!("cannot read {}: {error}", path.display())))?;
    if bytes.len() > MAX_MEMORY_BYTES {
        return Err(memory_error(format!(
            "{} exceeds the 256 KiB limit",
            path.display()
        )));
    }
    String::from_utf8(bytes)
        .map(Some)
        .map_err(|_| memory_error(format!("{} is not UTF-8", path.display())))
}

fn parse_entries(text: &str) -> Vec<MemoryEntry> {
    let mut entries = Vec::new();
    let mut current: Option<MemoryEntry> = None;
    for line in text.lines() {
        if let Some(title) = line.strip_prefix("## ") {
            if let Some(entry) = current.take() {
                entries.push(entry);
            }
            current = Some(MemoryEntry {
                title: clean_title(title),
                body: String::new(),
            });
        } else if let Some(entry) = current.as_mut() {
            if !entry.body.is_empty() {
                entry.body.push('\n');
            }
            entry.body.push_str(line);
        }
    }
    if let Some(entry) = current {
        entries.push(entry);
    }
    entries.truncate(MAX_ENTRIES);
    for entry in &mut entries {
        entry.body = truncate(entry.body.trim(), MAX_ENTRY_BYTES);
    }
    entries
}

fn clean_title(raw: &str) -> String {
    let title: String = raw
        .chars()
        .filter(|character| !character.is_control())
        .take(MAX_TITLE_CHARS)
        .collect();
    let title = title.trim();
    if title.is_empty() {
        "Untitled".to_owned()
    } else {
        title.to_owned()
    }
}

fn truncate(text: &str, max_bytes: usize) -> String {
    if text.len() <= max_bytes {
        return text.to_owned();
    }
    let mut end = max_bytes;
    while end > 0 && !text.is_char_boundary(end) {
        end -= 1;
    }
    text[..end].to_owned()
}

fn ensure_memory_dir(root: &Path) -> CoreResult<PathBuf> {
    for directory in [root.join(".ryoko"), root.join(".ryoko/memory")] {
        match std::fs::symlink_metadata(&directory) {
            Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_dir() => {
                return Err(memory_error(format!(
                    "{} must be a real directory",
                    directory.display()
                )));
            }
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                std::fs::create_dir(&directory).map_err(|error| {
                    memory_error(format!("cannot create {}: {error}", directory.display()))
                })?;
                #[cfg(unix)]
                {
                    use std::os::unix::fs::PermissionsExt;
                    let _ = std::fs::set_permissions(
                        &directory,
                        std::fs::Permissions::from_mode(0o700),
                    );
                }
            }
            Err(error) => {
                return Err(memory_error(format!(
                    "cannot inspect {}: {error}",
                    directory.display()
                )));
            }
        }
    }
    let directory = root.join(".ryoko/memory");
    let resolved = directory
        .canonicalize()
        .map_err(|error| memory_error(format!("cannot resolve memory directory: {error}")))?;
    if !resolved.starts_with(root) {
        return Err(memory_error(
            "memory directory escapes the project root".to_owned(),
        ));
    }
    Ok(resolved)
}

fn write_private(path: &Path, content: &str) -> CoreResult<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .truncate(true)
            .write(true)
            .mode(0o600)
            .open(path)
            .map_err(|error| memory_error(format!("cannot write {}: {error}", path.display())))?;
        file.write_all(content.as_bytes())
            .map_err(|error| memory_error(format!("cannot write {}: {error}", path.display())))?;
    }
    #[cfg(not(unix))]
    {
        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .truncate(true)
            .write(true)
            .open(path)
            .map_err(|error| memory_error(format!("cannot write {}: {error}", path.display())))?;
        file.write_all(content.as_bytes())
            .map_err(|error| memory_error(format!("cannot write {}: {error}", path.display())))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn write(dir: &TempDir, name: &str, content: &str) {
        let memory = dir.path().join(".ryoko/memory");
        std::fs::create_dir_all(&memory).unwrap();
        std::fs::write(memory.join(name), content).unwrap();
    }

    #[test]
    fn loads_markdown_sections_and_ignores_missing_files() {
        let dir = TempDir::new().unwrap();
        write(
            &dir,
            "decisions.md",
            "## Use SQLite\nKeep the index local.\n\n## No network\nOffline only.\n",
        );
        write(&dir, "conventions.md", "## Formatting\nRun cargo fmt.\n");
        let memory = ProjectMemory::load(dir.path()).unwrap();
        assert_eq!(memory.decisions.len(), 2);
        assert_eq!(memory.decisions[0].title, "Use SQLite");
        assert!(memory.decisions[0].body.contains("Keep the index local."));
        assert_eq!(memory.conventions.len(), 1);
        assert!(memory.rejections.is_empty());
    }

    #[test]
    fn renders_bounded_context_with_newest_rejections_first() {
        let dir = TempDir::new().unwrap();
        write(
            &dir,
            "rejections.md",
            "## Old rejection\nDo not repeat the old approach.\n\n## New rejection\nDo not repeat the new approach.\n",
        );
        let memory = ProjectMemory::load(dir.path()).unwrap();
        let context = memory.render_for_context(400);
        assert!(context.len() <= 400, "{context:?}");
        let new = context.find("New rejection").unwrap();
        let old = context.find("Old rejection").unwrap();
        assert!(new < old, "{context}");
    }

    #[test]
    fn append_rejection_keeps_newest_and_writes_private_file() {
        let dir = TempDir::new().unwrap();
        ProjectMemory::append_rejection(dir.path(), "First proposal", "Rejected: too broad.")
            .unwrap();
        ProjectMemory::append_rejection(dir.path(), "Second proposal", "Rejected: missing tests.")
            .unwrap();
        let memory = ProjectMemory::load(dir.path()).unwrap();
        assert_eq!(memory.rejections.len(), 2);
        assert_eq!(memory.rejections[0].title, "Second proposal");
        assert!(memory.rejections[0].body.contains("missing tests"));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(dir.path().join(".ryoko/memory/rejections.md"))
                .unwrap()
                .permissions()
                .mode()
                & 0o777;
            assert_eq!(mode, 0o600);
        }
    }

    #[cfg(unix)]
    #[test]
    fn rejects_symlinked_memory_files_and_directories() {
        let dir = TempDir::new().unwrap();
        let outside = TempDir::new().unwrap();
        std::fs::write(outside.path().join("secret.md"), "## Secret\nnope\n").unwrap();
        std::fs::create_dir_all(dir.path().join(".ryoko/memory")).unwrap();
        std::os::unix::fs::symlink(
            outside.path().join("secret.md"),
            dir.path().join(".ryoko/memory/decisions.md"),
        )
        .unwrap();
        assert!(ProjectMemory::load(dir.path()).is_err());

        let dir = TempDir::new().unwrap();
        let outside = TempDir::new().unwrap();
        std::os::unix::fs::symlink(outside.path(), dir.path().join(".ryoko")).unwrap();
        assert!(ProjectMemory::append_rejection(dir.path(), "title", "body").is_err());
    }

    #[test]
    fn oversized_titles_and_bodies_are_bounded() {
        let dir = TempDir::new().unwrap();
        let title = "x".repeat(10_000);
        let body = "y".repeat(100_000);
        ProjectMemory::append_rejection(dir.path(), &title, &body).unwrap();
        let memory = ProjectMemory::load(dir.path()).unwrap();
        assert_eq!(memory.rejections.len(), 1);
        assert!(memory.rejections[0].title.chars().count() <= MAX_TITLE_CHARS);
        assert!(memory.rejections[0].body.len() <= MAX_ENTRY_BYTES);
    }

    #[test]
    fn append_decision_appends_chronologically_and_writes_private_file() {
        let dir = TempDir::new().unwrap();
        ProjectMemory::append_decision(
            dir.path(),
            "First decision",
            "Use SQLite for local storage.",
        )
        .unwrap();
        ProjectMemory::append_decision(dir.path(), "Second decision", "Adopt Rust 2024 edition.")
            .unwrap();
        let memory = ProjectMemory::load(dir.path()).unwrap();
        assert_eq!(memory.decisions.len(), 2);
        assert_eq!(memory.decisions[0].title, "First decision");
        assert!(memory.decisions[0].body.contains("Use SQLite"));
        assert_eq!(memory.decisions[1].title, "Second decision");
        assert!(memory.decisions[1].body.contains("Rust 2024"));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(dir.path().join(".ryoko/memory/decisions.md"))
                .unwrap()
                .permissions()
                .mode()
                & 0o777;
            assert_eq!(mode, 0o600);
        }
    }

    #[test]
    fn append_convention_appends_chronologically_and_writes_private_file() {
        let dir = TempDir::new().unwrap();
        ProjectMemory::append_convention(dir.path(), "Formatting", "Run cargo fmt before commit.")
            .unwrap();
        ProjectMemory::append_convention(dir.path(), "Testing", "All new code must have tests.")
            .unwrap();
        let memory = ProjectMemory::load(dir.path()).unwrap();
        assert_eq!(memory.conventions.len(), 2);
        assert_eq!(memory.conventions[0].title, "Formatting");
        assert!(memory.conventions[0].body.contains("cargo fmt"));
        assert_eq!(memory.conventions[1].title, "Testing");
        assert!(memory.conventions[1].body.contains("must have tests"));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(dir.path().join(".ryoko/memory/conventions.md"))
                .unwrap()
                .permissions()
                .mode()
                & 0o777;
            assert_eq!(mode, 0o600);
        }
    }

    #[test]
    fn append_decision_and_convention_reject_symlinks() {
        let dir = TempDir::new().unwrap();
        let outside = TempDir::new().unwrap();
        std::fs::create_dir_all(dir.path().join(".ryoko/memory")).unwrap();
        std::os::unix::fs::symlink(
            outside.path().join("secret.md"),
            dir.path().join(".ryoko/memory/decisions.md"),
        )
        .unwrap();
        assert!(ProjectMemory::append_decision(dir.path(), "title", "body").is_err());

        let dir = TempDir::new().unwrap();
        let outside = TempDir::new().unwrap();
        std::os::unix::fs::symlink(outside.path(), dir.path().join(".ryoko")).unwrap();
        assert!(ProjectMemory::append_convention(dir.path(), "title", "body").is_err());
    }
}
