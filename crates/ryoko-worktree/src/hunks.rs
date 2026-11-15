//! Parse Git's own unified diffs into selectably staged text hunks.
//! Unsupported binary, rename, deletion and mode-only changes are never guessed.
use std::collections::HashSet;
use std::path::{Component, Path};

const MAX_PATCH_BYTES: usize = 1024 * 1024;
const MAX_HUNKS: usize = 128;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReviewHunk {
    pub id: usize,
    pub path: String,
    pub header: String,
    pub preview: String,
}

#[derive(Debug, Clone)]
struct FilePatch {
    header: Vec<String>,
    hunks: Vec<Vec<String>>,
    path: Option<String>,
    newly_created: bool,
}

#[derive(Debug, Clone)]
pub struct HunkPatch {
    files: Vec<FilePatch>,
    hunks: Vec<ReviewHunk>,
}

impl HunkPatch {
    pub fn parse(patch: &str) -> Result<Self, String> {
        if patch.len() > MAX_PATCH_BYTES {
            return Err("patch exceeds the 1 MiB hunk review limit".into());
        }
        let mut files: Vec<FilePatch> = Vec::new();
        let mut current: Option<FilePatch> = None;
        for line in patch.lines() {
            if line.starts_with("diff --git ") {
                if let Some(file) = current.take() {
                    files.push(file);
                }
                current = Some(FilePatch {
                    header: vec![line.to_owned()],
                    hunks: Vec::new(),
                    path: None,
                    newly_created: false,
                });
            } else if let Some(file) = &mut current {
                if line.starts_with("@@ ") {
                    file.hunks.push(vec![line.to_owned()]);
                } else if let Some(hunk) = file.hunks.last_mut() {
                    hunk.push(line.to_owned());
                } else {
                    file.header.push(line.to_owned());
                }
            } else if !line.trim().is_empty() {
                return Err("patch does not begin with a Git file header".into());
            }
        }
        if let Some(file) = current {
            files.push(file);
        }

        let mut hunks = Vec::new();
        for file in &mut files {
            let unsupported = file.header.iter().any(|line| {
                line.starts_with("GIT binary patch")
                    || line.starts_with("Binary files ")
                    || line.starts_with("rename from ")
                    || line.starts_with("rename to ")
                    || line.starts_with("copy from ")
                    || line.starts_with("copy to ")
                    || line.starts_with("deleted file mode ")
                    || line.starts_with("old mode ")
                    || line.starts_with("new mode ")
                    || line.starts_with("Submodule ")
            });
            if unsupported || file.hunks.is_empty() {
                file.hunks.clear();
                continue;
            }
            let Some(path) = file
                .header
                .iter()
                .find_map(|line| line.strip_prefix("+++ b/"))
            else {
                file.hunks.clear();
                continue;
            };
            if path.is_empty()
                || path.chars().any(char::is_control)
                || !Path::new(path)
                    .components()
                    .all(|part| matches!(part, Component::Normal(_)))
            {
                file.hunks.clear();
                continue;
            }
            if !file
                .header
                .iter()
                .any(|line| line.starts_with("--- a/") || line == "--- /dev/null")
            {
                file.hunks.clear();
                continue;
            }
            file.path = Some(path.to_owned());
            file.newly_created = file
                .header
                .iter()
                .any(|line| line.starts_with("new file mode "));
            for hunk in &file.hunks {
                let id = hunks.len() + 1;
                if id > MAX_HUNKS {
                    return Err("more than 128 text hunks; refine the proposal first".into());
                }
                let preview = hunk
                    .iter()
                    .filter(|line| line.starts_with('+') || line.starts_with('-'))
                    .take(6)
                    .map(String::as_str)
                    .collect::<Vec<_>>()
                    .join("\n");
                hunks.push(ReviewHunk {
                    id,
                    path: path.to_owned(),
                    header: hunk[0].clone(),
                    preview: preview.chars().take(500).collect(),
                });
            }
        }
        Ok(Self { files, hunks })
    }

    pub fn hunks(&self) -> &[ReviewHunk] {
        &self.hunks
    }

    /// Select only the requested text hunks. A newly created file must be
    /// included in its entirety because a partial new-file blob is ambiguous.
    pub fn select(&self, chosen: &[usize]) -> Result<String, String> {
        if chosen.is_empty() || chosen.len() > MAX_HUNKS {
            return Err("select at least one and at most 128 hunk IDs".into());
        }
        let ids: HashSet<usize> = chosen.iter().copied().collect();
        if ids.len() != chosen.len() || ids.iter().any(|id| *id == 0 || *id > self.hunks.len()) {
            return Err("invalid or repeated hunk ID".into());
        }
        let mut result = Vec::new();
        let mut index = 0;
        for file in &self.files {
            let mut selected = Vec::new();
            for hunk in &file.hunks {
                index += 1;
                if ids.contains(&index) {
                    selected.push(hunk);
                }
            }
            if selected.is_empty() {
                continue;
            }
            if file.newly_created && selected.len() != file.hunks.len() {
                return Err(format!(
                    "new file {} must be selected as a whole",
                    file.path.as_deref().unwrap_or("unknown")
                ));
            }
            result.extend(file.header.iter().cloned());
            for hunk in selected {
                result.extend(hunk.iter().cloned());
            }
        }
        if result.is_empty() {
            return Err("no selectable text hunk was found".into());
        }
        Ok(result.join("\n") + "\n")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const PATCH: &str = "diff --git a/src/main.rs b/src/main.rs\nindex 1111111..2222222 100644\n--- a/src/main.rs\n+++ b/src/main.rs\n@@ -1,1 +1,1 @@\n-old\n+new\n@@ -20,1 +20,1 @@\n-older\n+newer\n";

    #[test]
    fn selects_only_requested_hunk_without_leaking_others() {
        let patch = HunkPatch::parse(PATCH).unwrap();
        assert_eq!(patch.hunks().len(), 2);
        assert_eq!(patch.hunks()[1].path, "src/main.rs");
        let second = patch.select(&[2]).unwrap();
        assert!(second.contains("+newer"));
        assert!(!second.contains("+new\n"));
        assert!(second.starts_with("diff --git "));
        assert!(patch.select(&[0]).is_err());
        assert!(patch.select(&[2, 2]).is_err());
        assert!(patch.select(&[]).is_err());
    }

    #[test]
    fn excludes_binary_diffs_and_renames() {
        let patch =
            HunkPatch::parse("diff --git a/a.bin b/a.bin\nGIT binary patch\nliteral 3\nabc\n")
                .unwrap();
        assert!(patch.hunks().is_empty());
        let renamed = HunkPatch::parse(
            "diff --git a/a.rs b/b.rs\nsimilarity index 80%\nrename from a.rs\nrename to b.rs\n--- a/a.rs\n+++ b/b.rs\n@@ -1 +1 @@\n-a\n+b\n",
        )
        .unwrap();
        assert!(renamed.hunks().is_empty());
    }

    #[test]
    fn refuses_partial_new_file() {
        let patch = HunkPatch::parse(
            "diff --git a/new.rs b/new.rs\nnew file mode 100644\n--- /dev/null\n+++ b/new.rs\n@@ -0,0 +1 @@\n+one\n@@ -0,0 +3 @@\n+two\n",
        )
        .unwrap();
        assert!(patch.select(&[1]).is_err());
        assert!(patch.select(&[1, 2]).is_ok());
    }
}
