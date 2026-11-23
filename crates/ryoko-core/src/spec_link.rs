//! Living specifications: a two-way link between markdown specs (RFCs, PRDs,
//! design notes) and the implementation files they govern.
//!
//! A spec opts in by naming the files it is responsible for:
//!
//! ```markdown
//! <!-- ryoko:implements crates/ryoko-core/src/spec_link.rs -->
//! ```
//!
//! `SpecIndex::record` stores a digest of the spec and of every linked file.
//! `SpecIndex::drift` compares the tree with that record, so a change on either
//! side surfaces the other side as drifted work. `drift_tasks` turns each drift
//! into a targeted task for the team.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::error::{CoreError, CoreResult};
use crate::model::{AgentRole, Task};
use crate::workspace::validate_relative_path;

/// The directive that links a spec to code.
pub const DIRECTIVE: &str = "ryoko:implements";

/// Metadata keys carried by a drift task, so the orchestrator and the UI can
/// attribute a task to the spec that caused it.
pub const SPEC_METADATA: &str = "ryoko.spec";
pub const FILES_METADATA: &str = "ryoko.files";
pub const KIND_METADATA: &str = "ryoko.kind";
pub const DRIFT_KIND: &str = "spec-drift";

const MAX_LINKS: usize = 512;
const MAX_SPEC_BYTES: u64 = 512 * 1024;
const MAX_FILE_BYTES: u64 = 4 * 1024 * 1024;
const MAX_VISITED: usize = 4_000;
const MAX_DEPTH: usize = 12;
const SKIPPED_DIRS: [&str; 9] = [
    ".git",
    ".ryoko",
    ".hg",
    ".svn",
    "node_modules",
    "target",
    "dist",
    "build",
    "vendor",
];

/// One spec and the files it claims.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SpecLink {
    pub spec: PathBuf,
    pub files: Vec<PathBuf>,
}

/// A recorded snapshot of one spec and its files.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SpecRecord {
    /// Digest of the spec text when it was recorded.
    pub digest: u64,
    /// Linked file, with its digest when it was recorded.
    pub files: BTreeMap<String, u64>,
}

/// The persisted record of every known link.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SpecIndex {
    #[serde(default)]
    pub specs: BTreeMap<String, SpecRecord>,
}

/// What moved for one spec.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SpecDrift {
    pub spec: PathBuf,
    /// Linked files whose content changed since the record.
    pub changed: Vec<PathBuf>,
    /// Linked files that no longer exist.
    pub missing: Vec<PathBuf>,
    /// Links added to the spec since the record.
    pub added: Vec<PathBuf>,
    /// Links removed from the spec since the record.
    pub removed: Vec<PathBuf>,
    /// The spec text itself changed.
    pub spec_changed: bool,
    /// The spec was never recorded, so nothing can be compared yet.
    pub unrecorded: bool,
}

impl SpecDrift {
    pub fn is_empty(&self) -> bool {
        !self.unrecorded
            && !self.spec_changed
            && self.changed.is_empty()
            && self.missing.is_empty()
            && self.added.is_empty()
            && self.removed.is_empty()
    }

    /// Every file this drift asks an agent to look at, deduplicated.
    pub fn files(&self) -> Vec<PathBuf> {
        let mut seen = BTreeSet::new();
        let mut files = Vec::new();
        for path in self
            .changed
            .iter()
            .chain(&self.missing)
            .chain(&self.added)
            .chain(&self.removed)
        {
            if seen.insert(path.clone()) {
                files.push(path.clone());
            }
        }
        files
    }

    pub fn describe(&self) -> String {
        let spec = portable_path(&self.spec);
        if self.unrecorded {
            return format!("{spec}: no recorded baseline");
        }
        let mut parts = Vec::new();
        if self.spec_changed {
            parts.push("spec changed".to_owned());
        }
        if !self.changed.is_empty() {
            parts.push(format!("changed: {}", join(&self.changed)));
        }
        if !self.missing.is_empty() {
            parts.push(format!("missing: {}", join(&self.missing)));
        }
        if !self.added.is_empty() {
            parts.push(format!("newly linked: {}", join(&self.added)));
        }
        if !self.removed.is_empty() {
            parts.push(format!("unlinked: {}", join(&self.removed)));
        }
        format!("{spec}: {}", parts.join("; "))
    }
}

// Persist and display relative repository paths with portable separators.
// Windows PathBuf::display() renders backslashes even for an unchanged file.
fn portable_path(path: &Path) -> String {
    path.to_string_lossy().replace('\\', "/")
}

fn join(paths: &[PathBuf]) -> String {
    paths
        .iter()
        .map(|path| portable_path(path))
        .collect::<Vec<_>>()
        .join(", ")
}

/// FNV-1a. Drift detection needs cheap, stable comparison, not a cryptographic
/// guarantee: the digests never leave the local state file.
fn digest(bytes: &[u8]) -> u64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in bytes {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
}

/// Blanks out fenced code blocks so a spec can document the directive without
/// linking the paths it uses as examples. Newlines are preserved so byte offsets
/// and line structure do not shift.
fn without_code_fences(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut fence: Option<&str> = None;
    for line in text.split_inclusive('\n') {
        let trimmed = line.trim_start();
        let marker = if trimmed.starts_with("```") {
            Some("```")
        } else if trimmed.starts_with("~~~") {
            Some("~~~")
        } else {
            None
        };
        match (fence, marker) {
            (None, Some(open)) => {
                fence = Some(open);
                out.extend(line.chars().map(|c| if c == '\n' { '\n' } else { ' ' }));
            }
            (Some(_), Some(_)) => {
                fence = None;
                out.extend(line.chars().map(|c| if c == '\n' { '\n' } else { ' ' }));
            }
            (Some(_), None) => {
                out.extend(line.chars().map(|c| if c == '\n' { '\n' } else { ' ' }));
            }
            (None, None) => out.push_str(line),
        }
    }
    out
}

/// Extracts the valid paths from every `ryoko:implements` directive in a spec.
/// Invalid links are dropped rather than trusted: a spec cannot point outside
/// the project, at Git or Ryoko metadata, or at an absurd number of files.
pub fn parse_links(text: &str) -> Vec<PathBuf> {
    let mut links = Vec::new();
    let mut seen = BTreeSet::new();
    let text = without_code_fences(text);
    let mut rest = text.as_str();
    while let Some(start) = rest.find("<!--") {
        let after = &rest[start + 4..];
        let Some(end) = after.find("-->") else {
            break;
        };
        let body = &after[..end];
        rest = &after[end + 3..];
        let Some((marker, paths)) = body.trim().split_once(char::is_whitespace) else {
            continue;
        };
        if marker.trim() != DIRECTIVE {
            continue;
        }
        for token in paths.split_whitespace() {
            if links.len() >= MAX_LINKS {
                return links;
            }
            if !seen.insert(token.to_owned()) {
                continue;
            }
            if let Ok(path) = validate_relative_path(token) {
                links.push(path);
            }
        }
    }
    links
}

fn is_spec_file(path: &Path) -> bool {
    matches!(
        path.extension().and_then(|extension| extension.to_str()),
        Some("md" | "markdown" | "mdx")
    )
}

/// Walks the project for specs and reads their links. The walk is bounded and
/// skips build output, dependencies and metadata directories.
pub fn discover(root: &Path) -> CoreResult<Vec<SpecLink>> {
    let mut links = Vec::new();
    let mut budget = MAX_VISITED;
    let mut stack = vec![(root.to_path_buf(), 0usize)];
    while let Some((dir, depth)) = stack.pop() {
        if budget == 0 {
            break;
        }
        let entries = match std::fs::read_dir(&dir) {
            Ok(entries) => entries,
            Err(_) => continue,
        };
        let mut children = Vec::new();
        for entry in entries.flatten() {
            budget = budget.saturating_sub(1);
            if budget == 0 {
                break;
            }
            let path = entry.path();
            let name = entry.file_name().to_string_lossy().to_string();
            let Ok(file_type) = entry.file_type() else {
                continue;
            };
            if file_type.is_dir() {
                if depth < MAX_DEPTH
                    && !name.starts_with('.')
                    && !SKIPPED_DIRS
                        .iter()
                        .any(|skip| skip.eq_ignore_ascii_case(&name))
                {
                    children.push(path);
                }
                continue;
            }
            if !file_type.is_file() || !is_spec_file(&path) {
                continue;
            }
            if entry.metadata().map(|meta| meta.len()).unwrap_or(0) > MAX_SPEC_BYTES {
                continue;
            }
            let Ok(text) = std::fs::read_to_string(&path) else {
                continue;
            };
            let files = parse_links(&text);
            if files.is_empty() {
                continue;
            }
            let relative = path
                .strip_prefix(root)
                .map_err(|_| CoreError::Workspace(format!("spec outside root: {name}")))?;
            links.push(SpecLink {
                spec: relative.to_path_buf(),
                files,
            });
        }
        children.sort();
        for child in children.into_iter().rev() {
            stack.push((child, depth + 1));
        }
    }
    links.sort_by(|a, b| a.spec.cmp(&b.spec));
    Ok(links)
}

fn read_digest(root: &Path, relative: &Path) -> Option<u64> {
    let path = root.join(relative);
    let metadata = std::fs::metadata(&path).ok()?;
    if !metadata.is_file() || metadata.len() > MAX_FILE_BYTES {
        return None;
    }
    std::fs::read(&path).ok().map(|bytes| digest(&bytes))
}

fn spec_digest(root: &Path, relative: &Path) -> Option<u64> {
    std::fs::read(root.join(relative))
        .ok()
        .map(|bytes| digest(&bytes))
}

impl SpecIndex {
    pub fn load(path: &Path) -> CoreResult<Self> {
        match std::fs::read_to_string(path) {
            Ok(text) => serde_json::from_str(&text).map_err(|error| {
                CoreError::Workspace(format!("spec index is not valid JSON: {error}"))
            }),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(error) => Err(CoreError::Workspace(format!(
                "cannot read spec index: {error}"
            ))),
        }
    }

    pub fn save(&self, path: &Path) -> CoreResult<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|error| {
                CoreError::Workspace(format!("cannot create spec index directory: {error}"))
            })?;
        }
        let text = serde_json::to_string_pretty(self)
            .map_err(|error| CoreError::Workspace(format!("cannot encode spec index: {error}")))?;
        std::fs::write(path, text + "\n")
            .map_err(|error| CoreError::Workspace(format!("cannot write spec index: {error}")))
    }

    /// Records (or refreshes) every spec discovered under `root`. Returns the
    /// number of specs recorded.
    pub fn record(&mut self, root: &Path) -> CoreResult<usize> {
        let links = discover(root)?;
        self.specs
            .retain(|spec, _| links.iter().any(|link| portable_path(&link.spec) == *spec));
        for link in &links {
            let key = portable_path(&link.spec);
            let digest = spec_digest(root, &link.spec).unwrap_or(0);
            let files = link
                .files
                .iter()
                .map(|file| {
                    let key = portable_path(file);
                    let digest = read_digest(root, file).unwrap_or(0);
                    (key, digest)
                })
                .collect();
            self.specs.insert(key, SpecRecord { digest, files });
        }
        Ok(links.len())
    }

    /// Reports every spec whose text, links or linked files moved.
    pub fn drift(&self, root: &Path) -> CoreResult<Vec<SpecDrift>> {
        let mut drift = Vec::new();
        for link in discover(root)? {
            let key = portable_path(&link.spec);
            let current: BTreeSet<String> =
                link.files.iter().map(|file| portable_path(file)).collect();
            let Some(record) = self.specs.get(&key) else {
                drift.push(SpecDrift {
                    spec: link.spec,
                    changed: Vec::new(),
                    missing: Vec::new(),
                    added: link.files,
                    removed: Vec::new(),
                    spec_changed: false,
                    unrecorded: true,
                });
                continue;
            };
            let spec_changed = spec_digest(root, &link.spec)
                .map(|now| now != record.digest)
                .unwrap_or(true);
            let mut changed = Vec::new();
            let mut missing = Vec::new();
            for (file, recorded) in &record.files {
                match read_digest(root, Path::new(file)) {
                    Some(now) if now == *recorded => {}
                    Some(_) => changed.push(PathBuf::from(file)),
                    None => missing.push(PathBuf::from(file)),
                }
            }
            let added = current
                .iter()
                .filter(|file| !record.files.contains_key(*file))
                .map(PathBuf::from)
                .collect::<Vec<_>>();
            let removed = record
                .files
                .keys()
                .filter(|file| !current.contains(*file))
                .map(PathBuf::from)
                .collect::<Vec<_>>();
            changed.sort();
            missing.sort();
            let entry = SpecDrift {
                spec: link.spec,
                changed,
                missing,
                added,
                removed,
                spec_changed,
                unrecorded: false,
            };
            if !entry.is_empty() {
                drift.push(entry);
            }
        }
        drift.sort_by(|a, b| a.spec.cmp(&b.spec));
        Ok(drift)
    }

    /// Reverse lookup: which specs claim this file.
    pub fn specs_for(&self, file: &str) -> Vec<&str> {
        let file = file.replace('\\', "/");
        self.specs
            .iter()
            .filter(|(_, record)| record.files.contains_key(&file))
            .map(|(spec, _)| spec.as_str())
            .collect()
    }
}

/// The only paths listed in a drift are ones an agent may edit, so each becomes
/// its own task instead of one vague "update the spec" instruction.
pub fn drift_tasks(drift: &[SpecDrift], role: AgentRole) -> Vec<Task> {
    let mut tasks = Vec::new();
    for entry in drift {
        let files = entry.files();
        if files.is_empty() {
            if entry.unrecorded || entry.spec_changed {
                tasks.push(
                    Task::new(
                        format!(
                            "Review spec {} and confirm its linked code is up to date",
                            portable_path(&entry.spec)
                        ),
                        role.clone(),
                    )
                    .with_metadata(SPEC_METADATA, portable_path(&entry.spec))
                    .with_metadata(KIND_METADATA, DRIFT_KIND),
                );
            }
            continue;
        }
        let description = format!(
            "Bring {} in line with spec {} ({})",
            join(&files),
            portable_path(&entry.spec),
            entry.describe()
        );
        tasks.push(
            Task::new(description, role.clone())
                .with_metadata(SPEC_METADATA, portable_path(&entry.spec))
                .with_metadata(
                    FILES_METADATA,
                    files
                        .iter()
                        .map(|file| portable_path(file))
                        .collect::<Vec<_>>()
                        .join(","),
                )
                .with_metadata(KIND_METADATA, DRIFT_KIND),
        );
    }
    tasks
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn write(root: &Path, path: &str, text: &str) {
        let path = root.join(path);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    }

    #[test]
    fn parses_single_line_and_block_directives() {
        let text = "\
# RFC 12

<!-- ryoko:implements src/api.rs src/client.rs -->
Some prose.

<!-- ryoko:implements
  src/deep/mod.rs
-->
<!-- unrelated comment -->
";
        let links = parse_links(text);
        assert_eq!(
            links,
            vec![
                PathBuf::from("src/api.rs"),
                PathBuf::from("src/client.rs"),
                PathBuf::from("src/deep/mod.rs")
            ]
        );
    }

    #[test]
    fn directives_inside_code_fences_are_documentation() {
        let text = "\
<!-- ryoko:implements real.rs -->

```markdown
<!-- ryoko:implements example/only.rs -->
```

~~~
<!-- ryoko:implements also/ignored.rs -->
~~~

<!-- ryoko:implements also/real.rs -->
```
unclosed fence hides this: <!-- ryoko:implements after/fence.rs -->
";
        assert_eq!(
            parse_links(text),
            vec![PathBuf::from("real.rs"), PathBuf::from("also/real.rs")]
        );
    }

    #[test]
    fn drops_unsafe_and_duplicate_links() {
        let text = "<!-- ryoko:implements ../outside.rs /etc/passwd .git/config .ryoko/state \
                    src/ok.rs src/ok.rs -->";
        assert_eq!(parse_links(text), vec![PathBuf::from("src/ok.rs")]);
        assert!(parse_links("<!-- not-a-directive src/x.rs -->").is_empty());
        assert!(parse_links("<!-- ryoko:implements -->").is_empty());
        assert!(parse_links("no comments here").is_empty());
        assert!(parse_links("<!-- ryoko:implements unterminated").is_empty());
    }

    #[test]
    fn portable_paths_keep_stable_forward_slash_record_keys_and_drift_labels() {
        let path = Path::new("docs").join("rfc.md");
        assert_eq!(portable_path(&path), "docs/rfc.md");
        let drift = SpecDrift {
            spec: path,
            changed: vec![Path::new("src").join("lib.rs")],
            missing: vec![],
            added: vec![],
            removed: vec![],
            spec_changed: false,
            unrecorded: false,
        };
        assert!(drift.describe().contains("changed: src/lib.rs"));
    }

    #[test]
    fn records_and_reports_clean_drift() {
        let dir = TempDir::new().unwrap();
        let root = dir.path();
        write(
            root,
            "docs/rfc.md",
            "<!-- ryoko:implements src/lib.rs -->\n",
        );
        write(root, "src/lib.rs", "pub fn a() {}\n");
        write(root, "notes.txt", "<!-- ryoko:implements src/lib.rs -->\n");

        let mut index = SpecIndex::default();
        assert_eq!(index.record(root).unwrap(), 1, "only markdown is a spec");
        assert!(index.specs.contains_key("docs/rfc.md"));
        assert_eq!(index.specs_for("src/lib.rs"), vec!["docs/rfc.md"]);
        assert!(index.drift(root).unwrap().is_empty());
    }

    #[test]
    fn a_code_change_is_attributed_to_its_spec() {
        let dir = TempDir::new().unwrap();
        let root = dir.path();
        write(
            root,
            "docs/rfc.md",
            "<!-- ryoko:implements src/lib.rs -->\n",
        );
        write(root, "src/lib.rs", "pub fn a() {}\n");
        let mut index = SpecIndex::default();
        index.record(root).unwrap();

        write(root, "src/lib.rs", "pub fn b() {}\n");
        let drift = index.drift(root).unwrap();
        assert_eq!(drift.len(), 1);
        assert_eq!(drift[0].spec, PathBuf::from("docs/rfc.md"));
        assert_eq!(drift[0].changed, vec![PathBuf::from("src/lib.rs")]);
        assert!(!drift[0].spec_changed);
        assert!(drift[0].describe().contains("changed: src/lib.rs"));
    }

    #[test]
    fn spec_edits_and_link_changes_are_reported() {
        let dir = TempDir::new().unwrap();
        let root = dir.path();
        write(root, "spec.md", "<!-- ryoko:implements a.rs b.rs -->\n");
        write(root, "a.rs", "1\n");
        write(root, "b.rs", "2\n");
        let mut index = SpecIndex::default();
        index.record(root).unwrap();

        // The spec grows a clause and swaps one link for another.
        write(root, "c.rs", "3\n");
        write(root, "spec.md", "<!-- ryoko:implements a.rs c.rs -->\n");
        let drift = index.drift(root).unwrap();
        assert_eq!(drift.len(), 1);
        assert!(drift[0].spec_changed);
        assert_eq!(drift[0].added, vec![PathBuf::from("c.rs")]);
        assert_eq!(drift[0].removed, vec![PathBuf::from("b.rs")]);
        assert_eq!(
            drift[0].files(),
            vec![PathBuf::from("c.rs"), PathBuf::from("b.rs")]
        );
        let tasks = drift_tasks(&drift, AgentRole::SeniorDeveloper);
        assert_eq!(tasks.len(), 1);
        assert!(tasks[0].description.contains("spec.md"));
        assert_eq!(tasks[0].metadata[KIND_METADATA], DRIFT_KIND);
        assert_eq!(tasks[0].metadata[SPEC_METADATA], "spec.md");
        assert_eq!(tasks[0].metadata[FILES_METADATA], "c.rs,b.rs");
    }

    #[test]
    fn a_deleted_linked_file_is_missing() {
        let dir = TempDir::new().unwrap();
        let root = dir.path();
        write(root, "spec.md", "<!-- ryoko:implements src/gone.rs -->\n");
        write(root, "src/gone.rs", "x\n");
        let mut index = SpecIndex::default();
        index.record(root).unwrap();
        std::fs::remove_file(root.join("src/gone.rs")).unwrap();
        let drift = index.drift(root).unwrap();
        assert_eq!(drift[0].missing, vec![PathBuf::from("src/gone.rs")]);
        assert!(drift[0].describe().contains("missing: src/gone.rs"));
    }

    #[test]
    fn unrecorded_specs_are_reported_once_recorded_forgets_removed_specs() {
        let dir = TempDir::new().unwrap();
        let root = dir.path();
        write(root, "spec.md", "<!-- ryoko:implements a.rs -->\n");
        write(root, "a.rs", "1\n");
        let mut index = SpecIndex::default();
        let drift = index.drift(root).unwrap();
        assert!(drift[0].unrecorded);
        assert_eq!(drift[0].files(), vec![PathBuf::from("a.rs")]);
        let tasks = drift_tasks(&drift, AgentRole::SeniorDeveloper);
        assert_eq!(tasks.len(), 1);

        index.record(root).unwrap();
        std::fs::remove_file(root.join("spec.md")).unwrap();
        index.record(root).unwrap();
        assert!(index.specs.is_empty(), "a deleted spec is forgotten");
    }

    #[test]
    fn index_round_trips_through_json() {
        let dir = TempDir::new().unwrap();
        let root = dir.path();
        write(root, "spec.md", "<!-- ryoko:implements a.rs -->\n");
        write(root, "a.rs", "1\n");
        let mut index = SpecIndex::default();
        index.record(root).unwrap();
        let path = root.join(".ryoko/state/specs.json");
        index.save(&path).unwrap();
        let loaded = SpecIndex::load(&path).unwrap();
        assert_eq!(loaded, index);
        assert!(
            SpecIndex::load(&root.join("absent.json"))
                .unwrap()
                .specs
                .is_empty()
        );
        std::fs::write(&path, "not json").unwrap();
        assert!(SpecIndex::load(&path).is_err());
    }
}
