//! Reusable skills: folders holding a `SKILL.md` with `name` and
//! `description` front matter.
//!
//! Only names and descriptions go into the system prompt; the model loads a
//! skill's full instructions with the `load_skill` tool when it needs them.
//! Skills are read from `.ryoko/skills`, then `.agents/skills` and
//! `.claude/skills`, so skills installed for other agent tools work too.
//! The first skill with a given name wins.

use std::collections::BTreeSet;
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};

pub const SKILL_DIRS: &[&str] = &[".ryoko/skills", ".agents/skills", ".claude/skills"];
pub const MAX_SKILLS: usize = 64;
pub const MAX_SKILL_BYTES: u64 = 64 * 1024;
const MAX_DESCRIPTION_CHARS: usize = 400;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Skill {
    pub name: String,
    pub description: String,
    pub path: PathBuf,
}

impl Skill {
    /// Reads the skill's full `SKILL.md`, bounded to [`MAX_SKILL_BYTES`].
    pub fn load(&self) -> std::io::Result<String> {
        read_bounded(&self.path)
    }
}

/// Finds skills under the workspace. Entries whose real path leaves the
/// workspace (for example through a symlink) are ignored.
pub fn discover(root: &Path) -> Vec<Skill> {
    let Ok(root) = root.canonicalize() else {
        return Vec::new();
    };
    let mut seen = BTreeSet::new();
    let mut skills = Vec::new();
    for dir in SKILL_DIRS {
        let Ok(entries) = fs::read_dir(root.join(dir)) else {
            continue;
        };
        let mut folders: Vec<PathBuf> = entries.filter_map(Result::ok).map(|e| e.path()).collect();
        folders.sort();
        for folder in folders {
            if skills.len() >= MAX_SKILLS {
                return skills;
            }
            let Ok(file) = folder.join("SKILL.md").canonicalize() else {
                continue;
            };
            if !file.starts_with(&root) || !file.is_file() {
                continue;
            }
            let Ok(text) = read_bounded(&file) else {
                continue;
            };
            let fallback = folder
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or_default();
            let (name, description) = front_matter(&text);
            let name = name.unwrap_or_else(|| fallback.to_owned());
            if !valid_name(&name) || !seen.insert(name.clone()) {
                continue;
            }
            let description: String = description
                .unwrap_or_default()
                .chars()
                .filter(|ch| !ch.is_control())
                .take(MAX_DESCRIPTION_CHARS)
                .collect();
            skills.push(Skill {
                name,
                description,
                path: file,
            });
        }
    }
    skills
}

fn valid_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 64
        && name
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | ':'))
}

fn front_matter(text: &str) -> (Option<String>, Option<String>) {
    let mut lines = text.lines();
    if lines.next().map(str::trim) != Some("---") {
        return (None, None);
    }
    let (mut name, mut description) = (None, None);
    for line in lines {
        if line.trim() == "---" {
            break;
        }
        if let Some((key, value)) = line.split_once(':') {
            let value = value.trim().trim_matches('"').trim_matches('\'').to_owned();
            match key.trim() {
                "name" if !value.is_empty() => name = Some(value),
                "description" if !value.is_empty() => description = Some(value),
                _ => {}
            }
        }
    }
    (name, description)
}

fn read_bounded(path: &Path) -> std::io::Result<String> {
    let mut bytes = Vec::new();
    fs::File::open(path)?
        .take(MAX_SKILL_BYTES)
        .read_to_end(&mut bytes)?;
    Ok(String::from_utf8_lossy(&bytes).into_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write_skill(root: &Path, dir: &str, folder: &str, body: &str) {
        let path = root.join(dir).join(folder);
        fs::create_dir_all(&path).unwrap();
        fs::write(path.join("SKILL.md"), body).unwrap();
    }

    #[test]
    fn discovers_skills_with_front_matter_and_dedupes_by_name() {
        let dir = tempfile::tempdir().unwrap();
        write_skill(
            dir.path(),
            ".ryoko/skills",
            "review",
            "---\nname: code-review\ndescription: \"Review a diff\"\n---\nSteps…",
        );
        write_skill(
            dir.path(),
            ".claude/skills",
            "other",
            "---\nname: code-review\ndescription: duplicate\n---\n",
        );
        write_skill(
            dir.path(),
            ".agents/skills",
            "plain-folder",
            "No front matter",
        );
        write_skill(
            dir.path(),
            ".agents/skills",
            "bad",
            "---\nname: ../evil\n---\n",
        );

        let skills = discover(dir.path());
        let names: Vec<_> = skills.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(names, ["code-review", "plain-folder"]);
        assert_eq!(skills[0].description, "Review a diff");
        assert!(skills[0].load().unwrap().contains("Steps"));
    }

    #[cfg(unix)]
    #[test]
    fn ignores_skills_that_escape_the_workspace() {
        let outside = tempfile::tempdir().unwrap();
        fs::write(outside.path().join("SKILL.md"), "---\nname: escaped\n---\n").unwrap();
        let dir = tempfile::tempdir().unwrap();
        fs::create_dir_all(dir.path().join(".ryoko/skills")).unwrap();
        std::os::unix::fs::symlink(outside.path(), dir.path().join(".ryoko/skills/escaped"))
            .unwrap();
        assert!(discover(dir.path()).is_empty());
    }
}
