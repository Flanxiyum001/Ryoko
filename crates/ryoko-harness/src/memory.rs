//! Persistent, bounded project memory shared across harness sessions.
//!
//! Notes live in `<workspace>/.ryoko/agent-memory.md` (ignored by Git) as a
//! plain bullet list the user can read and edit. Notes are short, single-line
//! and screened for credentials before they are stored.

use std::fs;
use std::io::{self, Read};
use std::path::{Path, PathBuf};

/// Largest note the model may store.
pub const MAX_NOTE_CHARS: usize = 500;
/// Largest memory file loaded into a prompt or appended to.
pub const MAX_MEMORY_BYTES: u64 = 16 * 1024;

#[derive(Debug, thiserror::Error)]
pub enum MemoryError {
    #[error("note is empty")]
    Empty,
    #[error("note is longer than {MAX_NOTE_CHARS} characters")]
    TooLong,
    #[error("note looks like it contains a credential; credentials are never stored")]
    LooksLikeSecret,
    #[error("project memory is full ({MAX_MEMORY_BYTES} bytes); ask the user to tidy it")]
    Full,
    #[error("I/O error: {0}")]
    Io(#[from] io::Error),
}

#[derive(Debug, Clone)]
pub struct ProjectMemory {
    path: PathBuf,
}

impl ProjectMemory {
    pub fn for_workspace(root: &Path) -> Self {
        Self {
            path: root.join(".ryoko").join("agent-memory.md"),
        }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Reads the memory file, returning an empty string when it is missing.
    /// Oversized files are truncated to [`MAX_MEMORY_BYTES`].
    pub fn load(&self) -> String {
        let Ok(file) = fs::File::open(&self.path) else {
            return String::new();
        };
        let mut bytes = Vec::new();
        if file.take(MAX_MEMORY_BYTES).read_to_end(&mut bytes).is_err() {
            return String::new();
        }
        String::from_utf8_lossy(&bytes).into_owned()
    }

    /// Validates and appends one note as a bullet.
    pub fn append(&self, note: &str) -> Result<(), MemoryError> {
        let note = normalise_note(note)?;
        let existing = self.load();
        let entry = format!("- {note}\n");
        if (existing.len() + entry.len()) as u64 > MAX_MEMORY_BYTES {
            return Err(MemoryError::Full);
        }
        if let Some(parent) = self.path.parent() {
            fs::create_dir_all(parent)?;
        }
        let mut contents = existing;
        if contents.is_empty() {
            contents.push_str("# Ryoko agent memory\n\n");
        } else if !contents.ends_with('\n') {
            contents.push('\n');
        }
        contents.push_str(&entry);
        fs::write(&self.path, contents)?;
        Ok(())
    }
}

fn normalise_note(note: &str) -> Result<String, MemoryError> {
    let collapsed: String = note
        .chars()
        .map(|ch| if ch.is_control() { ' ' } else { ch })
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    if collapsed.is_empty() {
        return Err(MemoryError::Empty);
    }
    if collapsed.chars().count() > MAX_NOTE_CHARS {
        return Err(MemoryError::TooLong);
    }
    if looks_like_secret(&collapsed) {
        return Err(MemoryError::LooksLikeSecret);
    }
    Ok(collapsed)
}

/// Conservative credential screen. False positives are fine: the note is
/// simply rejected and the model is told why.
pub fn looks_like_secret(text: &str) -> bool {
    const MARKERS: &[&str] = &[
        "-----BEGIN",
        "sk-",
        "sk_live_",
        "rk_live_",
        "ghp_",
        "gho_",
        "github_pat_",
        "xoxb-",
        "xoxp-",
        "AKIA",
        "AIza",
        "glpat-",
    ];
    if MARKERS.iter().any(|marker| text.contains(marker)) {
        return true;
    }
    let lower = text.to_ascii_lowercase();
    if [
        "password=",
        "password:",
        "api_key=",
        "apikey=",
        "secret=",
        "token=",
    ]
    .iter()
    .any(|marker| lower.contains(marker))
    {
        return true;
    }
    // Long unbroken high-entropy-looking runs (keys, JWTs).
    text.split(|ch: char| ch.is_whitespace() || ch == '"' || ch == '\'')
        .any(|word| {
            word.len() >= 40
                && word.chars().any(|ch| ch.is_ascii_digit())
                && word.chars().any(|ch| ch.is_ascii_uppercase())
                && word.chars().any(|ch| ch.is_ascii_lowercase())
                && !word.contains('/')
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn appends_and_loads_notes() {
        let dir = tempfile::tempdir().unwrap();
        let memory = ProjectMemory::for_workspace(dir.path());
        assert_eq!(memory.load(), "");
        memory
            .append("Tests run with  cargo test\n--workspace")
            .unwrap();
        memory.append("UI lives in apps/desktop/ui").unwrap();
        let text = memory.load();
        assert!(text.contains("- Tests run with cargo test --workspace\n"));
        assert!(text.contains("- UI lives in apps/desktop/ui\n"));
    }

    #[test]
    fn rejects_credentials_and_oversized_notes() {
        let dir = tempfile::tempdir().unwrap();
        let memory = ProjectMemory::for_workspace(dir.path());
        for secret in [
            "the key is sk-abc123",
            "use ghp_aaaaaaaaaaaaaaaaaaaa",
            "password=hunter2",
            "jwt eyJhbGciOiJIUzI1NiIsInR5cCI6IkpXVCJ9xYz0123456789abcd",
        ] {
            assert!(
                matches!(memory.append(secret), Err(MemoryError::LooksLikeSecret)),
                "{secret}"
            );
        }
        assert!(matches!(
            memory.append(&"a".repeat(MAX_NOTE_CHARS + 1)),
            Err(MemoryError::TooLong)
        ));
        assert!(matches!(memory.append("  \n "), Err(MemoryError::Empty)));
        assert!(!memory.path().exists());
    }

    #[test]
    fn refuses_to_grow_past_the_cap() {
        let dir = tempfile::tempdir().unwrap();
        let memory = ProjectMemory::for_workspace(dir.path());
        let note = "x ".repeat(240);
        let mut result = Ok(());
        for _ in 0..200 {
            result = memory.append(&note);
            if result.is_err() {
                break;
            }
        }
        assert!(matches!(result, Err(MemoryError::Full)));
        assert!(fs::metadata(memory.path()).unwrap().len() <= MAX_MEMORY_BYTES);
    }
}
