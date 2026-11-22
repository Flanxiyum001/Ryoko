//! Pure, platform-neutral validation for the Agent Sessions panel.
//!
//! Kept separate from the Tauri command so these checks run on a Linux host
//! without WebKit/GTK, matching the existing `paths.rs` test pattern.

use std::path::{Path, PathBuf};

const STATE_PATH: &str = ".ryoko/state/worktrees.sqlite3";

/// Return the canonical state database path only when it is a regular file
/// inside the open workspace. A missing database is not an error: it means no
/// agent session has been recorded yet.
pub(crate) fn session_state_path(root: &Path) -> Result<Option<PathBuf>, String> {
    let candidate = root.join(STATE_PATH);
    let metadata = match std::fs::symlink_metadata(&candidate) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(format!("cannot inspect agent session state: {error}")),
    };
    if !metadata.file_type().is_file() {
        return Err("agent session state is not a regular file".to_owned());
    }
    let canonical = candidate
        .canonicalize()
        .map_err(|error| format!("cannot resolve agent session state: {error}"))?;
    if !canonical.starts_with(root) {
        return Err("agent session state escapes the open workspace".to_owned());
    }
    Ok(Some(canonical))
}

pub(crate) fn bounded_text(value: &str, max_chars: usize) -> String {
    let mut output = String::new();
    for character in value.chars().take(max_chars) {
        if character.is_control() && character != '\n' && character != '\t' {
            continue;
        }
        output.push(character);
    }
    if value.chars().count() > max_chars {
        output.push('…');
    }
    output
}

pub(crate) fn session_state(value: &str) -> Option<&'static str> {
    match value {
        "active" => Some("active"),
        "published" => Some("published"),
        "merged" => Some("merged"),
        "abandoned" => Some("abandoned"),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::{bounded_text, session_state, session_state_path};

    #[test]
    fn summary_text_is_bounded_and_control_characters_are_removed() {
        let text = format!("line\u{0}one\n{}", "x".repeat(400));
        let bounded = bounded_text(&text, 40);
        assert!(!bounded.contains('\u{0}'));
        assert!(bounded.contains('\n'));
        assert!(bounded.ends_with('…'));
        assert!(bounded.chars().count() <= 41);
    }

    #[test]
    fn only_known_session_states_are_projected() {
        assert_eq!(session_state("active"), Some("active"));
        assert_eq!(session_state("published"), Some("published"));
        assert_eq!(session_state("injected"), None);
    }

    #[test]
    fn missing_state_is_not_an_error() {
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = std::env::temp_dir().join(format!("ryoko-empty-{stamp}"));
        std::fs::create_dir_all(&root).unwrap();
        assert!(session_state_path(&root).unwrap().is_none());
        std::fs::remove_dir_all(root).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn refuses_session_state_outside_the_workspace() {
        use std::os::unix::fs::symlink;
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let base =
            std::env::temp_dir().join(format!("ryoko-sessions-{}-{stamp}", std::process::id()));
        let outside = base.with_extension("outside");
        let state_dir = base.join(".ryoko/state");
        std::fs::create_dir_all(&outside).unwrap();
        std::fs::create_dir_all(&state_dir).unwrap();
        symlink(
            outside.join("worktrees.sqlite3"),
            state_dir.join("worktrees.sqlite3"),
        )
        .unwrap();
        let root = base.canonicalize().unwrap();
        assert!(session_state_path(&root).is_err());
        std::fs::remove_dir_all(base).unwrap();
        std::fs::remove_dir_all(outside).unwrap();
    }
}
