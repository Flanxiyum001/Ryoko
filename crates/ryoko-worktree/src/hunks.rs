//! Parse Git's own unified diffs into selectably staged text hunks.
//!
//! Unsupported binary, rename, deletion and mode-only changes are never guessed.
//! Text hunks may be staged individually, as part of an explicit cross-file
//! atomic group, or - for a newly created file - partially, in which case the
//! selected added lines are renumbered into one contiguous creation patch.
use std::collections::{HashMap, HashSet};
use std::path::{Component, Path};

const MAX_PATCH_BYTES: usize = 1024 * 1024;
const MAX_HUNKS: usize = 128;
const NO_NEWLINE_MARKER: &str = "\\ No newline at end of file";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReviewHunk {
    pub id: usize,
    pub path: String,
    pub header: String,
    pub preview: String,
}

/// A set of hunks that a caller wants applied together or not at all.
///
/// Groups exist because a coherent edit can legitimately span files (a trait
/// and its implementation, a test and the fixture it reads). Git applies the
/// generated patch atomically, so staging the union of a group's hunks is the
/// atomic unit; [`HunkPatch::select_groups`] refuses partial groups and hunks
/// shared between groups.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HunkGroup {
    pub id: usize,
    pub label: String,
    pub hunk_ids: Vec<usize>,
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

    /// One atomic group per file, in patch order. Callers that need a group
    /// spanning several files build their own [`HunkGroup`] list instead.
    pub fn file_groups(&self) -> Vec<HunkGroup> {
        let mut groups: Vec<HunkGroup> = Vec::new();
        let mut index = 0;
        for file in &self.files {
            let mut hunk_ids = Vec::new();
            for _ in &file.hunks {
                index += 1;
                hunk_ids.push(index);
            }
            if hunk_ids.is_empty() {
                continue;
            }
            let id = groups.len() + 1;
            groups.push(HunkGroup {
                id,
                label: file.path.clone().unwrap_or_else(|| "unknown".to_owned()),
                hunk_ids,
            });
        }
        groups
    }

    /// Select only the requested text hunks.
    ///
    /// A newly created file may be staged partially: the selected added lines
    /// are renumbered into a single creation patch, so the index receives a
    /// file containing exactly those lines. A source hunk that carries context
    /// or removed lines cannot be split that way and is rejected.
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
                result.extend(partial_new_file_patch(file, &selected)?);
                continue;
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

    /// Stage every hunk of every chosen atomic group.
    ///
    /// `groups` is the caller's group plan; `chosen_group_ids` names the groups
    /// to stage. A plan is rejected when a group is empty, when two groups
    /// share a hunk, or when a group references an unknown hunk, so a partial
    /// or ambiguous cross-file selection can never reach Git.
    pub fn select_groups(
        &self,
        groups: &[HunkGroup],
        chosen_group_ids: &[usize],
    ) -> Result<String, String> {
        let selected = self.flatten_groups(groups, chosen_group_ids)?;
        self.select(&selected)
    }

    /// Resolve chosen group IDs into a deduplicated, ordered hunk ID list after
    /// validating that the group plan is a partition of the hunks it covers.
    fn flatten_groups(
        &self,
        groups: &[HunkGroup],
        chosen_group_ids: &[usize],
    ) -> Result<Vec<usize>, String> {
        if groups.is_empty() {
            return Err("no atomic hunk groups were supplied".into());
        }
        if chosen_group_ids.is_empty() {
            return Err("select at least one hunk group".into());
        }
        let mut owner: HashMap<usize, usize> = HashMap::new();
        let mut by_id: HashMap<usize, &HunkGroup> = HashMap::new();
        for group in groups {
            if group.hunk_ids.is_empty() {
                return Err(format!(
                    "hunk group {} ({}) is empty",
                    group.id, group.label
                ));
            }
            if by_id.insert(group.id, group).is_some() {
                return Err(format!("duplicate hunk group id {}", group.id));
            }
            for hunk_id in &group.hunk_ids {
                if *hunk_id == 0 || *hunk_id > self.hunks.len() {
                    return Err(format!(
                        "hunk group {} references unknown hunk {}",
                        group.id, hunk_id
                    ));
                }
                if let Some(existing) = owner.insert(*hunk_id, group.id) {
                    return Err(format!(
                        "hunk {} belongs to both group {} and group {}",
                        hunk_id, existing, group.id
                    ));
                }
            }
        }
        let chosen: HashSet<usize> = chosen_group_ids.iter().copied().collect();
        if chosen.len() != chosen_group_ids.len() {
            return Err("repeated hunk group id".into());
        }
        let mut flattened = Vec::new();
        for group_id in chosen_group_ids {
            let Some(group) = by_id.get(group_id) else {
                return Err(format!("unknown hunk group {group_id}"));
            };
            flattened.extend(group.hunk_ids.iter().copied());
        }
        flattened.sort_unstable();
        flattened.dedup();
        Ok(flattened)
    }
}

/// Rebuild a creation patch that contains only the selected added lines.
///
/// Git cannot apply non-contiguous hunks to a file that does not exist yet, so
/// the selected lines are renumbered from line 1 into one hunk. The patch omits
/// the original `index` line because the synthesized blob is a different object
/// from the one the reviewed diff described.
fn partial_new_file_patch(
    file: &FilePatch,
    selected: &[&Vec<String>],
) -> Result<Vec<String>, String> {
    let path = file
        .path
        .as_deref()
        .ok_or_else(|| "new file patch has no target path".to_owned())?;
    let mut prefix = Vec::new();
    for line in &file.header {
        if line.starts_with("index ") || line.starts_with("similarity index ") {
            continue;
        }
        prefix.push(line.clone());
        if line.starts_with("+++ b/") {
            break;
        }
    }
    if !prefix.iter().any(|line| line.starts_with("+++ b/")) {
        return Err(format!(
            "new file {path} has no usable file header for partial staging"
        ));
    }

    let mut added: Vec<String> = Vec::new();
    let mut trailing_no_newline = false;
    for hunk in selected {
        let mut saw_added = false;
        for line in hunk.iter().skip(1) {
            if line.starts_with('+') {
                added.push(line.clone());
                saw_added = true;
                trailing_no_newline = false;
            } else if line == NO_NEWLINE_MARKER {
                trailing_no_newline = saw_added;
            } else if line.starts_with('-') || line.starts_with(' ') {
                return Err(format!(
                    "new file {path} mixes context or removed lines; stage it as a whole"
                ));
            }
        }
        if !saw_added {
            return Err(format!(
                "selected hunk of new file {path} adds no lines; stage it as a whole"
            ));
        }
    }
    if added.is_empty() {
        return Err(format!("new file {path} has no added lines to stage"));
    }
    let mut patch = prefix;
    patch.push(format!("@@ -0,0 +1,{} @@", added.len()));
    patch.extend(added);
    if trailing_no_newline {
        patch.push(NO_NEWLINE_MARKER.to_owned());
    }
    Ok(patch)
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

    const NEW_FILE: &str = "diff --git a/new.rs b/new.rs\nnew file mode 100644\nindex 0000000..3333333\n--- /dev/null\n+++ b/new.rs\n@@ -0,0 +1,1 @@\n+one\n@@ -0,0 +3,1 @@\n+two\n";

    #[test]
    fn stages_a_partial_new_file_by_renumbering_the_selected_lines() {
        let patch = HunkPatch::parse(NEW_FILE).unwrap();
        let first_only = patch.select(&[1]).unwrap();
        assert!(first_only.contains("new file mode 100644"));
        assert!(first_only.contains("@@ -0,0 +1,1 @@"));
        assert!(first_only.contains("+one"));
        assert!(!first_only.contains("+two"));
        assert!(
            !first_only.contains("index 0000000"),
            "the synthesized blob is not the reviewed object"
        );

        // Selecting every hunk keeps the reviewed header and hunk ranges intact
        // instead of renumbering, so Git applies exactly what was reviewed.
        let whole = patch.select(&[1, 2]).unwrap();
        assert!(whole.contains("index 0000000"));
        assert!(whole.contains("@@ -0,0 +1,1 @@"));
        assert!(whole.contains("@@ -0,0 +3,1 @@"));
        assert!(whole.contains("+one") && whole.contains("+two"));
    }

    #[test]
    fn refuses_to_partially_stage_a_new_file_with_context_lines() {
        let patch = HunkPatch::parse(
            "diff --git a/new.rs b/new.rs\nnew file mode 100644\n--- /dev/null\n+++ b/new.rs\n@@ -0,0 +1,2 @@\n+one\n+two\n@@ -0,0 +4,1 @@\n+three\n",
        )
        .unwrap();
        // Still fine: every hunk only adds lines.
        assert!(patch.select(&[2]).is_ok());

        let mixed = HunkPatch::parse(
            "diff --git a/new.rs b/new.rs\nnew file mode 100644\n--- /dev/null\n+++ b/new.rs\n@@ -0,0 +1,1 @@\n+one\n@@ -0,0 +3,2 @@\n+two\n three\n",
        )
        .unwrap();
        let error = mixed.select(&[2]).unwrap_err();
        assert!(error.contains("stage it as a whole"), "{error}");
    }

    #[test]
    fn preserves_a_trailing_no_newline_marker() {
        let patch = HunkPatch::parse(
            "diff --git a/new.txt b/new.txt\nnew file mode 100644\n--- /dev/null\n+++ b/new.txt\n@@ -0,0 +1,1 @@\n+hello\n\\ No newline at end of file\n",
        )
        .unwrap();
        let selected = patch.select(&[1]).unwrap();
        assert!(selected.contains(NO_NEWLINE_MARKER));
    }

    #[test]
    fn builds_one_atomic_group_per_file() {
        let patch = HunkPatch::parse(PATCH).unwrap();
        let groups = patch.file_groups();
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].label, "src/main.rs");
        assert_eq!(groups[0].hunk_ids, vec![1, 2]);
        let staged = patch.select_groups(&groups, &[groups[0].id]).unwrap();
        assert!(staged.contains("+new") && staged.contains("+newer"));
    }

    #[test]
    fn stages_a_cross_file_atomic_group() {
        let combined = format!(
            "{PATCH}diff --git a/tests/main.rs b/tests/main.rs\nindex 4444444..5555555 100644\n--- a/tests/main.rs\n+++ b/tests/main.rs\n@@ -1,1 +1,1 @@\n-old\n+assert\n"
        );
        let patch = HunkPatch::parse(&combined).unwrap();
        assert_eq!(patch.hunks().len(), 3);
        let group = HunkGroup {
            id: 1,
            label: "source + test".to_owned(),
            hunk_ids: vec![2, 3],
        };
        let staged = patch.select_groups(&[group], &[1]).unwrap();
        assert!(staged.contains("+newer"));
        assert!(staged.contains("+assert"));
        assert!(!staged.contains("+new\n"));
    }

    #[test]
    fn rejects_ambiguous_or_partial_group_plans() {
        let combined = format!(
            "{PATCH}diff --git a/tests/main.rs b/tests/main.rs\nindex 4444444..5555555 100644\n--- a/tests/main.rs\n+++ b/tests/main.rs\n@@ -1,1 +1,1 @@\n-old\n+assert\n"
        );
        let patch = HunkPatch::parse(&combined).unwrap();
        let overlapping = vec![
            HunkGroup {
                id: 1,
                label: "a".to_owned(),
                hunk_ids: vec![1, 2],
            },
            HunkGroup {
                id: 2,
                label: "b".to_owned(),
                hunk_ids: vec![2, 3],
            },
        ];
        assert!(patch.select_groups(&overlapping, &[1]).is_err());
        assert!(patch.select_groups(&overlapping, &[1, 2]).is_err());
        let empty = vec![HunkGroup {
            id: 1,
            label: "empty".to_owned(),
            hunk_ids: Vec::new(),
        }];
        assert!(patch.select_groups(&empty, &[1]).is_err());
        let unknown = vec![HunkGroup {
            id: 1,
            label: "unknown".to_owned(),
            hunk_ids: vec![99],
        }];
        assert!(patch.select_groups(&unknown, &[1]).is_err());
        assert!(patch.select_groups(&overlapping, &[7]).is_err());
    }
}
