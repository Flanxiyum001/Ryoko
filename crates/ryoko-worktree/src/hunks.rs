//! Parse Git's own unified diffs into selectably staged text hunks.
//!
//! Unsupported binary, rename, deletion and mode-only changes are never
//! guessed: they stay listed as explicit per-file review states instead of
//! silently disappearing. Text hunks may be staged individually, as part of
//! an explicit cross-file atomic group, or - for a newly created file -
//! partially, in which case the selected added lines are renumbered into one
//! contiguous creation patch.
//!
//! Before applying, a selection is verified against a freshly captured patch:
//! every hunk must still anchor to the same file identity (path and index
//! base), exact line ranges and complete hunk content, so a changed base
//! surfaces as an understandable conflict instead of an overwrite or a
//! misleading success.
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
    /// Old index blob abbreviation of the file this hunk was reviewed against,
    /// `0000000` for creations, or empty when Git omitted the index line.
    pub base: String,
}

/// Review state of one file difference in the patch.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileReviewState {
    /// Modified text file with selectable hunks.
    Text,
    /// Newly created text file with selectable hunks.
    Created,
    /// Deleted file; its content is gone, nothing is staged per hunk.
    Deleted,
    /// Renamed file; renamed paths are reviewed as one whole-file change.
    Renamed,
    /// Copied file; copied paths are reviewed as one whole-file change.
    Copied,
    /// Binary content; never editable through text hunks.
    Binary,
    /// Mode change only; no content hunks exist.
    ModeOnly,
    /// Submodule pointer; managed by Git, not by hunk review.
    Submodule,
}

impl FileReviewState {
    /// Stable key for typed IPC surfaces.
    pub fn key(self) -> &'static str {
        match self {
            FileReviewState::Text => "text",
            FileReviewState::Created => "created",
            FileReviewState::Deleted => "deleted",
            FileReviewState::Renamed => "renamed",
            FileReviewState::Copied => "copied",
            FileReviewState::Binary => "binary",
            FileReviewState::ModeOnly => "mode-only",
            FileReviewState::Submodule => "submodule",
        }
    }

    /// Only plain text and creation differences support hunk staging.
    pub fn supports_hunk_staging(self) -> bool {
        matches!(self, FileReviewState::Text | FileReviewState::Created)
    }
}

/// One file's review state with the hunk IDs it owns (empty when its state
/// does not support hunk staging).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileReview {
    pub path: String,
    pub old_path: Option<String>,
    pub state: FileReviewState,
    pub hunk_ids: Vec<usize>,
}

/// A set of hunks that a caller wants applied together or not at all.
///
/// Groups exist because a coherent edit can legitimately span files (a trait
/// and its implementation, a test and the fixture it reads). Git applies the
/// generated patch atomically, so staging the union of a group's hunks is the
/// atomic unit; [`HunkPatch::select_groups`] refuses partial groups and hunks
/// shared between groups.
#[derive(Debug, Clone, PartialEq, Eq)]
#[allow(dead_code)] // desktop embeds this file privately and stages per hunk
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
    old_path: Option<String>,
    state: Option<FileReviewState>,
    index_old: Option<String>,
    newly_created: bool,
}

#[derive(Debug, Clone)]
pub struct HunkPatch {
    files: Vec<FilePatch>,
    hunks: Vec<ReviewHunk>,
}

/// A selected hunk that no longer anchors to the file identity and line
/// ranges it was reviewed against.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StaleHunk {
    pub id: usize,
    pub path: String,
    pub reason: &'static str,
}

/// Outcome of verifying a selection against the freshly captured patch.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct SelectionAudit {
    /// IDs in the current patch, not the reviewed snapshot.
    pub fresh: Vec<usize>,
    pub stale: Vec<StaleHunk>,
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
                    old_path: None,
                    state: None,
                    index_old: None,
                    newly_created: false,
                });
            } else if let Some(file) = &mut current {
                if line.starts_with("@@ ") {
                    file.hunks.push(vec![line.to_owned()]);
                } else if let Some(hunk) = file.hunks.last_mut() {
                    hunk.push(line.to_owned());
                } else {
                    if file.old_path.is_none()
                        && let Some(old) = line
                            .strip_prefix("--- a/")
                            .or_else(|| line.strip_prefix("rename from "))
                            .or_else(|| line.strip_prefix("copy from "))
                    {
                        file.old_path = Some(old.to_owned());
                    }
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
            let binary = file.header.iter().any(|line| {
                line.starts_with("GIT binary patch") || line.starts_with("Binary files ")
            });
            let submodule = file
                .header
                .iter()
                .any(|line| line.starts_with("Submodule "));
            let deleted = file
                .header
                .iter()
                .any(|line| line.starts_with("deleted file mode "));
            let renamed = file
                .header
                .iter()
                .any(|line| line.starts_with("rename from "));
            let copied = file
                .header
                .iter()
                .any(|line| line.starts_with("copy from "));
            let mode_change = file
                .header
                .iter()
                .any(|line| line.starts_with("old mode ") || line.starts_with("new mode "));
            let created = file
                .header
                .iter()
                .any(|line| line.starts_with("new file mode "));

            if let Some(line) = file.header.iter().find(|line| line.starts_with("index ")) {
                file.index_old = line
                    .strip_prefix("index ")
                    .and_then(|rest| rest.split("..").next())
                    .map(str::to_owned);
            }

            let state = if binary {
                FileReviewState::Binary
            } else if submodule {
                FileReviewState::Submodule
            } else if deleted {
                FileReviewState::Deleted
            } else if renamed {
                FileReviewState::Renamed
            } else if copied {
                FileReviewState::Copied
            } else if created {
                FileReviewState::Created
            } else if mode_change {
                FileReviewState::ModeOnly
            } else {
                FileReviewState::Text
            };
            file.state = Some(state);
            file.newly_created = created && state.supports_hunk_staging();

            if !state.supports_hunk_staging() || file.hunks.is_empty() {
                file.hunks.clear();
                if file.path.is_none() {
                    file.path = file
                        .header
                        .iter()
                        .find_map(|line| {
                            line.strip_prefix("rename to ")
                                .or_else(|| line.strip_prefix("copy to "))
                        })
                        .map(str::to_owned);
                }
                if file.path.is_none() {
                    file.path = file.old_path.clone().or_else(|| {
                        file.header
                            .first()
                            .and_then(|line| fallback_diff_path(line))
                    });
                }
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
            let index_old = file.index_old.clone().unwrap_or_default();
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
                    base: index_old.clone(),
                });
            }
        }
        Ok(Self { files, hunks })
    }

    pub fn hunks(&self) -> &[ReviewHunk] {
        &self.hunks
    }

    /// Per-file review states, in patch order, including the differences that
    /// do not support hunk staging (binary, renames, copies, deletions,
    /// mode-only and submodule changes). Callers render these as explicit,
    /// non-editable review states instead of leaving them invisible.
    pub fn file_reviews(&self) -> Vec<FileReview> {
        let mut next_hunk = 1usize;
        let mut reviews = Vec::with_capacity(self.files.len());
        for file in &self.files {
            let hunk_ids = (next_hunk..next_hunk + file.hunks.len()).collect::<Vec<_>>();
            next_hunk += file.hunks.len();
            reviews.push(FileReview {
                path: file.path.clone().unwrap_or_else(|| "unknown".to_owned()),
                old_path: file.old_path.clone(),
                state: file.state.unwrap_or(FileReviewState::Text),
                hunk_ids,
            });
        }
        reviews
    }

    /// One atomic group per file, in patch order. Callers that need a group
    /// spanning several files build their own [`HunkGroup`] list instead.
    #[allow(dead_code)]
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

    /// Verifies the chosen hunks against the freshly captured `current` patch.
    ///
    /// `self` is the reviewed snapshot the caller selected from; `current` is
    /// the diff captured immediately before apply. A hunk stays fresh only
    /// when the same file identity (path and index base blob) still carries
    /// the exact same hunk header and complete body in `current`. IDs are
    /// snapshot-local: fresh IDs refer to the current patch, not `self`.
    /// Ambiguous matches and changed content are refused.
    pub fn verify_selection(
        &self,
        chosen: &[usize],
        current: &HunkPatch,
    ) -> Result<SelectionAudit, String> {
        if chosen.is_empty() || chosen.len() > MAX_HUNKS {
            return Err("select at least one and at most 128 hunk IDs".into());
        }
        let ids: HashSet<usize> = chosen.iter().copied().collect();
        if ids.len() != chosen.len() || ids.iter().any(|id| *id == 0 || *id > self.hunks.len()) {
            return Err("invalid or repeated hunk ID".into());
        }

        let reviewed_bodies: Vec<_> = self.files.iter().flat_map(|file| &file.hunks).collect();
        let current_bodies: Vec<_> = current
            .files
            .iter()
            .flat_map(|file| &file.hunks)
            .collect();
        let mut audit = SelectionAudit::default();
        let mut ordered = ids.into_iter().collect::<Vec<_>>();
        ordered.sort_unstable();
        for id in ordered {
            let hunk = &self.hunks[id - 1];
            let matching_base = |candidate: &ReviewHunk| {
                candidate.path == hunk.path && candidate.base == hunk.base
            };
            let anchors: Vec<_> = current
                .hunks
                .iter()
                .filter(|candidate| matching_base(candidate))
                .filter(|candidate| candidate.header == hunk.header)
                .collect();
            if let [candidate] = anchors.as_slice()
                && current_bodies[candidate.id - 1] == reviewed_bodies[id - 1]
                && !audit.fresh.contains(&candidate.id)
            {
                audit.fresh.push(candidate.id);
                continue;
            }
            let reason = if !current
                .hunks
                .iter()
                .any(|candidate| candidate.path == hunk.path)
            {
                "reviewed file no longer has unstaged differences"
            } else if !current.hunks.iter().any(matching_base) {
                "index base changed for this file"
            } else if anchors.is_empty() {
                "reviewed line range no longer matches the file"
            } else if anchors.len() > 1 {
                "reviewed hunk has ambiguous matches"
            } else {
                "reviewed hunk content changed or was selected more than once"
            };
            audit.stale.push(StaleHunk {
                id,
                path: hunk.path.clone(),
                reason,
            });
        }
        Ok(audit)
    }

    /// Selects the chosen hunks from the freshly captured `current` patch,
    /// but only when every reviewed hunk still anchors to the same file and
    /// base it was reviewed against. Any stale hunk fails the whole selection
    /// with a per-file conflict message; nothing is applied partially, and the
    /// caller must refresh the review instead of overwriting.
    #[allow(dead_code)]
    pub fn select_verified(&self, chosen: &[usize], current: &HunkPatch) -> Result<String, String> {
        let audit = self.verify_selection(chosen, current)?;
        if audit.stale.is_empty() {
            return current.select(&audit.fresh);
        }
        let details = audit
            .stale
            .iter()
            .map(|stale| format!("#{} {}: {}", stale.id, stale.path, stale.reason))
            .collect::<Vec<_>>()
            .join("; ");
        Err(format!(
            "review base is stale; refresh and reselect: {details}"
        ))
    }

    /// Stage every hunk of every chosen atomic group.
    ///
    /// `groups` is the caller's group plan; `chosen_group_ids` names the groups
    /// to stage. A plan is rejected when a group is empty, when two groups
    /// share a hunk, or when a group references an unknown hunk, so a partial
    /// or ambiguous cross-file selection can never reach Git.
    #[allow(dead_code)]
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
    #[allow(dead_code)]
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

/// Derives the new-side path from a `diff --git a/x b/y` header line. Used
/// only as a fallback for differences without `+++` and without explicit
/// rename/copy destinations.
fn fallback_diff_path(line: &str) -> Option<String> {
    let body = line.strip_prefix("diff --git ")?;
    let position = body.find(" b/")?;
    let candidate = body[position + 3..].trim_end_matches('\t');
    if candidate.is_empty() || candidate.chars().any(char::is_control) {
        return None;
    }
    Some(candidate.to_owned())
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

    #[test]
    fn binary_and_rename_files_get_explicit_non_editable_review_states() {
        let patch = HunkPatch::parse(
            "diff --git a/old.rs b/new.rs\nsimilarity index 70%\nrename from old.rs\nrename to new.rs\n--- a/old.rs\n+++ b/new.rs\n@@ -1 +1 @@\n-a\n+b\n",
        )
        .unwrap();
        let reviews = patch.file_reviews();
        assert_eq!(reviews.len(), 1);
        assert_eq!(reviews[0].path, "new.rs");
        assert_eq!(reviews[0].old_path.as_deref(), Some("old.rs"));
        assert_eq!(reviews[0].state, FileReviewState::Renamed);
        assert!(reviews[0].hunk_ids.is_empty());
        assert!(!reviews[0].state.supports_hunk_staging());

        let binary = HunkPatch::parse(
            "diff --git a/blob.bin b/blob.bin\nindex 1234567..89abcde 100644\nGIT binary patch\nliteral 4\nc2VjcmV0\n",
        )
        .unwrap();
        let reviews = binary.file_reviews();
        assert_eq!(reviews.len(), 1);
        assert_eq!(reviews[0].path, "blob.bin");
        assert_eq!(reviews[0].state, FileReviewState::Binary);
        assert!(!reviews[0].state.supports_hunk_staging());
    }

    #[test]
    fn deleted_mode_only_and_submodule_files_get_review_states() {
        let combined = "diff --git a/gone.rs b/gone.rs\ndeleted file mode 100644\nindex 1111111..0000000\n--- a/gone.rs\n+++ /dev/null\n@@ -1 +0,0 @@\n-content\ndiff --git a/lib.rs b/lib.rs\nold mode 100644\nnew mode 100755\ndiff --git a/sub b/sub\nindex abcdef1..1234567 160000\nSubmodule sub abcdef111111111111111111111111111111111..1234567890123456789012345678901234567890:\n";
        let patch = HunkPatch::parse(combined).unwrap();
        assert!(patch.hunks().is_empty());
        let reviews = patch.file_reviews();
        let states = reviews
            .iter()
            .map(|review| (review.path.as_str(), review.state))
            .collect::<Vec<_>>();
        assert!(states.contains(&("gone.rs", FileReviewState::Deleted)));
        assert!(states.contains(&("lib.rs", FileReviewState::ModeOnly)));
        assert!(states.contains(&("sub", FileReviewState::Submodule)));
        assert!(reviews.iter().all(|review| review.hunk_ids.is_empty()));
    }

    #[test]
    fn a_changed_base_marks_the_selected_hunk_stale() {
        let reviewed = HunkPatch::parse(PATCH).unwrap();
        let current = HunkPatch::parse(
            "diff --git a/src/main.rs b/src/main.rs\nindex 9999999..2222222 100644\n--- a/src/main.rs\n+++ b/src/main.rs\n@@ -1,1 +1,1 @@\n-old\n+new\n",
        )
        .unwrap();
        let error = reviewed
            .select_verified(&[1], &current)
            .expect_err("a changed index base must refuse staging");
        assert!(error.contains("index base changed"), "{error}");
        assert!(error.contains("src/main.rs"));
        let audit = reviewed.verify_selection(&[1], &current).unwrap();
        assert!(audit.fresh.is_empty());
        assert_eq!(audit.stale.len(), 1);
        assert_eq!(audit.stale[0].reason, "index base changed for this file");
    }

    #[test]
    fn a_moved_line_range_is_reported_as_stale_with_exact_path_targeting() {
        let reviewed = HunkPatch::parse(PATCH).unwrap();
        let current = HunkPatch::parse(
            "diff --git a/src/main.rs b/src/main.rs\nindex 1111111..2222222 100644\n--- a/src/main.rs\n+++ b/src/main.rs\n@@ -7,1 +7,1 @@\n-older\n+newer\n",
        )
        .unwrap();
        let audit = reviewed.verify_selection(&[2], &current).unwrap();
        assert!(audit.fresh.is_empty());
        assert_eq!(audit.stale.len(), 1);
        assert_eq!(audit.stale[0].path, "src/main.rs");
        assert_eq!(
            audit.stale[0].reason,
            "reviewed line range no longer matches the file"
        );
        assert!(reviewed.select_verified(&[2], &current).is_err());
    }

    #[test]
    fn a_vanished_file_difference_is_reported_as_stale() {
        let combined = format!(
            "{PATCH}diff --git a/other.rs b/other.rs\nindex 4444444..5555555 100644\n--- a/other.rs\n+++ b/other.rs\n@@ -1,1 +1,1 @@\n-a\n+b\n"
        );
        let reviewed = HunkPatch::parse(&combined).unwrap();
        let current = HunkPatch::parse(PATCH).unwrap();
        let audit = reviewed.verify_selection(&[3], &current).unwrap();
        assert!(audit.fresh.is_empty());
        assert_eq!(
            audit.stale[0].reason,
            "reviewed file no longer has unstaged differences"
        );
    }

    #[test]
    fn selecting_hunks_of_an_unchanged_file_survives_unrelated_file_changes() {
        let combined = format!(
            "{PATCH}diff --git a/generated.rs b/generated.rs\nindex 6666666..7777777 100644\n--- a/generated.rs\n+++ b/generated.rs\n@@ -1,1 +1,1 @@\n-a\n+b\n"
        );
        let reviewed = HunkPatch::parse(&combined).unwrap();
        let mut drifted = String::new();
        for line in combined.lines() {
            if line.starts_with("index 6666666") {
                drifted.push_str("index 6666668..7777778 100644\n");
            } else {
                drifted.push_str(line);
                drifted.push('\n');
            }
        }
        let current = HunkPatch::parse(&drifted).unwrap();
        let audit = reviewed.verify_selection(&[1], &current).unwrap();
        assert_eq!(audit.fresh, vec![1]);
        assert!(audit.stale.is_empty());

        let audit = reviewed.verify_selection(&[3], &current).unwrap();
        assert!(audit.fresh.is_empty());
        assert_eq!(audit.stale[0].reason, "index base changed for this file");
    }

    #[test]
    fn verified_selection_refuses_changed_content_at_the_same_anchor() {
        let reviewed = HunkPatch::parse(PATCH).unwrap();
        let current = HunkPatch::parse(
            "diff --git a/src/main.rs b/src/main.rs\nindex 1111111..2222222 100644\n--- a/src/main.rs\n+++ b/src/main.rs\n@@ -1,1 +1,1 @@\n-old\n+fresh\n@@ -20,1 +20,1 @@\n-older\n+newer\n",
        )
        .unwrap();
        let error = reviewed.select_verified(&[1], &current).unwrap_err();
        assert!(error.contains("content changed"), "{error}");
    }

    #[test]
    fn verified_selection_remaps_shifted_ids_to_the_reviewed_file() {
        let reviewed = HunkPatch::parse(PATCH).unwrap();
        let unrelated = "diff --git a/a.rs b/a.rs\nindex 3333333..4444444 100644\n--- a/a.rs\n+++ b/a.rs\n@@ -1 +1 @@\n-a\n+b\n";
        let current = HunkPatch::parse(&format!("{unrelated}{PATCH}")).unwrap();
        let audit = reviewed.verify_selection(&[1], &current).unwrap();
        assert_eq!(audit.fresh, vec![2]);
        assert!(audit.stale.is_empty());
        let selected = reviewed.select_verified(&[1], &current).unwrap();
        assert!(selected.contains("+new\n"));
        assert!(!selected.contains("a/a.rs"));
        assert!(!selected.contains("+newer"));
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
