//! Git working-tree status for explorer badges and the multi-file review
//! drawer. Git is invoked with an explicit argv, never a shell, and output is
//! size-bounded before parsing.

use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use serde::Serialize;

const MAX_STATUS_BYTES: usize = 512 * 1024;
/// Bounds the drawer; a pathological tree stops being listed, never truncated
/// into a wrong status letter.
const MAX_ENTRIES: usize = 2_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ChangeKind {
    Added,
    Modified,
    Deleted,
    Renamed,
    Untracked,
    Conflicted,
    Other,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ChangeEntry {
    pub path: String,
    pub kind: ChangeKind,
    /// Unstaged working-tree letter (`x` from porcelain), for badges.
    pub untracked: bool,
}

fn kind_for(index: char, worktree: char) -> ChangeKind {
    let letter = if worktree == ' ' { index } else { worktree };
    match letter {
        'A' => ChangeKind::Added,
        'M' => ChangeKind::Modified,
        'D' => ChangeKind::Deleted,
        'R' => ChangeKind::Renamed,
        '?' => ChangeKind::Untracked,
        'U' => ChangeKind::Conflicted,
        _ => ChangeKind::Other,
    }
}

/// Porcelain quotes paths containing special characters. Resolve the common
/// escapes; an unresolvable escape stays quoted, never guessed.
fn unquote(path: &str) -> String {
    let trimmed = path.trim();
    if !(trimmed.len() >= 2 && trimmed.starts_with('"') && trimmed.ends_with('"')) {
        return trimmed.to_owned();
    }
    let body = &trimmed[1..trimmed.len() - 1];
    let mut out = String::with_capacity(body.len());
    let mut chars = body.chars();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('n') => out.push('\n'),
            Some('t') => out.push('\t'),
            Some('\\') => out.push('\\'),
            Some('"') => out.push('"'),
            Some(other) => {
                out.push('\\');
                out.push(other);
            }
            None => out.push('\\'),
        }
    }
    out
}

fn normalized(path: &str) -> String {
    path.replace('\\', "/")
}

fn repository_root(root: &Path) -> Option<PathBuf> {
    let output = Command::new("git")
        .current_dir(root)
        .args(["rev-parse", "--show-toplevel"])
        .stdin(Stdio::null())
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let top = String::from_utf8(output.stdout).ok()?;
    let canonical = Path::new(top.trim()).canonicalize().ok()?;
    (canonical == root.canonicalize().ok()?).then_some(canonical)
}

/// `None` when the workspace is not a Git repository (badges stay hidden).
pub fn working_tree_changes(root: &Path) -> Option<Vec<ChangeEntry>> {
    repository_root(root)?;
    let mut child = Command::new("git")
        .current_dir(root)
        .args([
            "status",
            "--porcelain=v1",
            "--untracked-files=all",
            "--no-renames",
            "-z",
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .env("GIT_TERMINAL_PROMPT", "0")
        .spawn()
        .ok()?;
    let mut output = Vec::new();
    let read = child.stdout.take().map(|stdout| {
        stdout
            .take((MAX_STATUS_BYTES + 1) as u64)
            .read_to_end(&mut output)
    });
    let read = match read {
        Some(Ok(read)) => read as usize,
        _ => {
            let _ = child.kill();
            let _ = child.wait();
            return None;
        }
    };
    if read > MAX_STATUS_BYTES {
        // The pipe may be full. Kill before wait or a large status result
        // can deadlock the UI while Git is still trying to write stdout.
        let _ = child.kill();
        let _ = child.wait();
        return Some(Vec::new());
    }
    let _ = child.wait();
    let text = String::from_utf8_lossy(&output).into_owned();
    Some(parse_porcelain(&text))
}

/// Parses `-z` porcelain output, which is NUL-terminated so paths may contain
/// newlines. Rename targets point at the new path; quoted paths are resolved
/// conservatively (an unresolvable escape stays quoted, never guessed).
pub fn parse_porcelain(text: &str) -> Vec<ChangeEntry> {
    let mut entries = Vec::new();
    let mut fields = text.split('\0');
    while entries.len() < MAX_ENTRIES {
        let Some(record) = fields.next() else {
            break;
        };
        if record.is_empty() {
            continue;
        }
        let (status_field, path) = record.split_at(3.min(record.len()));
        let mut chars = status_field.chars();
        let (index, worktree) = match (chars.next(), chars.next()) {
            (Some(x), Some(y)) => (x, y),
            _ => continue,
        };
        if path.is_empty() {
            continue;
        }
        // `R  old -> new` and ` R old -> new`: badges point at the target.
        let path = match path.split_once(" -> ") {
            Some((_, target)) => target.to_owned(),
            None => unquote(path),
        };
        entries.push(ChangeEntry {
            path: normalized(&path),
            kind: kind_for(index, worktree),
            untracked: worktree == '?',
        });
    }
    entries
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_z_porcelain_records() {
        let text = " M src/main.rs\0?? scratch.txt\0A  new.rs\0D  gone.rs\0";
        let entries = parse_porcelain(text);
        assert_eq!(entries[0].path, "src/main.rs");
        assert_eq!(entries[0].kind, ChangeKind::Modified);
        assert_eq!(entries[1].path, "scratch.txt");
        assert_eq!(entries[1].kind, ChangeKind::Untracked);
        assert!(entries[1].untracked);
        assert_eq!(entries[2].kind, ChangeKind::Added);
        assert_eq!(entries[3].kind, ChangeKind::Deleted);
    }

    #[test]
    fn renames_point_at_the_target() {
        let entries = parse_porcelain("R  old_name.rs -> new_name.rs\0");
        assert_eq!(entries[0].path, "new_name.rs");
        assert_eq!(entries[0].kind, ChangeKind::Renamed);
    }

    #[test]
    fn quoted_paths_are_resolved() {
        let entries = parse_porcelain(" M \"with\\ttab.rs\"\0");
        assert_eq!(entries[0].path, "with\ttab.rs");
    }

    #[test]
    fn nul_separated_output_keeps_newlines_in_paths() {
        let text = " M a\nb.rs\0?? plain.rs\0";
        let entries = parse_porcelain(text);
        assert_eq!(entries[0].path, "a\nb.rs");
        assert_eq!(entries[1].path, "plain.rs");
        assert_eq!(entries[1].kind, ChangeKind::Untracked);
    }

    #[test]
    fn short_or_malformed_records_are_skipped() {
        assert!(parse_porcelain("x\0\0").is_empty());
    }
}
