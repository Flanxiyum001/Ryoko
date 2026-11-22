//! Restricted, read-only source inspection tools for future agent adapters.
//!
//! These tools cannot execute commands, write to disk or access the network.

use std::path::{Component, Path, PathBuf};

use ignore::WalkBuilder;
use serde::{Deserialize, Serialize};

pub const MAX_READ_BYTES: u64 = 256 * 1024;
pub const MAX_SCAN_FILES: usize = 2_000;
pub const MAX_RESULTS: usize = 100;

#[derive(Debug, thiserror::Error)]
pub enum ToolError {
    #[error("invalid or unsafe workspace path")]
    InvalidPath,
    #[error("path is outside the workspace")]
    OutsideWorkspace,
    #[error("file is too large or unsupported")]
    UnsupportedFile,
    #[error("query must be 1..128 characters")]
    InvalidQuery,
    #[error("file is not UTF-8")]
    InvalidUtf8,
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "operation", rename_all = "snake_case")]
pub enum ToolRequest {
    ReadFile { path: String },
    ListFiles { limit: usize },
    SearchText { needle: String, limit: usize },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SearchHit {
    pub path: String,
    pub line: usize,
    pub column: usize,
    pub excerpt: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ToolResponse {
    File { path: String, content: String },
    Files { paths: Vec<String>, truncated: bool },
    Matches { matches: Vec<SearchHit>, truncated: bool },
}

pub struct ReadOnlyTools {
    root: PathBuf,
}

impl ReadOnlyTools {
    pub fn new(root: &Path) -> Result<Self, ToolError> {
        let canonical = root.canonicalize()?;
        if !canonical.is_dir() {
            return Err(ToolError::InvalidPath);
        }
        Ok(Self { root: canonical })
    }

    pub fn execute(&self, request: ToolRequest) -> Result<ToolResponse, ToolError> {
        match request {
            ToolRequest::ReadFile { path } => self.read_file(&path),
            ToolRequest::ListFiles { limit } => self.list_files(limit),
            ToolRequest::SearchText { needle, limit } => self.search_text(&needle, limit),
        }
    }

    fn resolve(&self, path: &str) -> Result<PathBuf, ToolError> {
        if path.is_empty() || path.contains('\\') {
            return Err(ToolError::InvalidPath);
        }
        let relative = Path::new(path);
        if relative.is_absolute()
            || relative.components().any(|component| match component {
                Component::Normal(name) => name.to_string_lossy().starts_with('.'),
                _ => true,
            })
        {
            return Err(ToolError::InvalidPath);
        }
        let resolved = self.root.join(relative).canonicalize()?;
        if !resolved.starts_with(&self.root) {
            return Err(ToolError::OutsideWorkspace);
        }
        if !resolved.is_file() || !supported_source(&resolved) {
            return Err(ToolError::UnsupportedFile);
        }
        Ok(resolved)
    }

    fn read_file(&self, path: &str) -> Result<ToolResponse, ToolError> {
        let resolved = self.resolve(path)?;
        if std::fs::metadata(&resolved)?.len() > MAX_READ_BYTES {
            return Err(ToolError::UnsupportedFile);
        }
        let data = std::fs::read(&resolved)?;
        if data.len() as u64 > MAX_READ_BYTES {
            return Err(ToolError::UnsupportedFile);
        }
        let content = String::from_utf8(data).map_err(|_| ToolError::InvalidUtf8)?;
        Ok(ToolResponse::File {
            path: path.to_owned(),
            content,
        })
    }

    fn candidates(&self) -> Vec<String> {
        let mut builder = WalkBuilder::new(&self.root);
        builder
            .hidden(true)
            .git_ignore(true)
            .follow_links(false)
            .filter_entry(|entry| {
                entry.depth() == 0
                    || !matches!(
                        entry.file_name().to_str(),
                        Some(".git" | ".ryoko" | "target" | "node_modules" | "dist")
                    )
            });
        let mut paths = Vec::new();
        for entry in builder.build().filter_map(Result::ok) {
            if paths.len() >= MAX_SCAN_FILES {
                break;
            }
            if !entry.file_type().is_some_and(|kind| kind.is_file())
                || !supported_source(entry.path())
            {
                continue;
            }
            let Ok(relative) = entry.path().strip_prefix(&self.root) else {
                continue;
            };
            let Some(path) = relative.to_str() else {
                continue;
            };
            paths.push(path.replace('\\', "/"));
        }
        paths.sort();
        paths
    }

    fn list_files(&self, limit: usize) -> Result<ToolResponse, ToolError> {
        let all = self.candidates();
        let limit = limit.clamp(1, MAX_RESULTS);
        let truncated = all.len() > limit || all.len() >= MAX_SCAN_FILES;
        Ok(ToolResponse::Files {
            paths: all.into_iter().take(limit).collect(),
            truncated,
        })
    }

    fn search_text(&self, needle: &str, limit: usize) -> Result<ToolResponse, ToolError> {
        if needle.is_empty() || needle.chars().count() > 128 {
            return Err(ToolError::InvalidQuery);
        }
        let paths = self.candidates();
        let limit = limit.clamp(1, MAX_RESULTS);
        let mut matches = Vec::new();
        let mut truncated = paths.len() >= MAX_SCAN_FILES;
        'files: for path in paths {
            let resolved = match self.resolve(&path) {
                Ok(value) => value,
                Err(_) => continue,
            };
            let Ok(metadata) = std::fs::metadata(&resolved) else {
                continue;
            };
            if metadata.len() > MAX_READ_BYTES {
                continue;
            }
            let Ok(data) = std::fs::read_to_string(&resolved) else {
                continue;
            };
            for (index, line) in data.lines().enumerate() {
                if let Some(column) = line.find(needle) {
                    if matches.len() >= limit {
                        truncated = true;
                        break 'files;
                    }
                    matches.push(SearchHit {
                        path: path.clone(),
                        line: index + 1,
                        column: line[..column].chars().count() + 1,
                        excerpt: line.chars().take(240).collect(),
                    });
                }
            }
        }
        Ok(ToolResponse::Matches { matches, truncated })
    }
}

fn supported_source(path: &Path) -> bool {
    matches!(
        path.extension().and_then(|extension| extension.to_str()),
        Some(
            "rs" | "ts" | "tsx" | "js" | "jsx" | "py" | "go" | "java" | "c"
                | "cpp" | "h" | "hpp" | "md" | "toml" | "json" | "yaml" | "yml"
                | "html" | "css"
        )
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn searches_known_source_without_reading_hidden_files() {
        let temp = tempfile::TempDir::new().unwrap();
        std::fs::create_dir(temp.path().join("src")).unwrap();
        std::fs::write(
            temp.path().join("src/main.rs"),
            "pub fn hello() {\n    let result = 42;\n}\n",
        )
        .unwrap();
        std::fs::write(temp.path().join(".env"), "SECRET_TOKEN=unsafe").unwrap();
        let tools = ReadOnlyTools::new(temp.path()).unwrap();
        let response = tools
            .execute(ToolRequest::SearchText {
                needle: "result".to_owned(),
                limit: 10,
            })
            .unwrap();
        let ToolResponse::Matches { matches, truncated } = response else {
            panic!("expected matches");
        };
        assert_eq!(matches.len(), 1);
        assert!(!truncated);
        assert_eq!(matches[0].line, 2);
        assert_eq!(matches[0].column, 9);
        assert_eq!(matches[0].path, "src/main.rs");
        assert!(matches!(
            tools.execute(ToolRequest::ReadFile {
                path: ".env".to_owned()
            }),
            Err(ToolError::InvalidPath)
        ));
    }

    #[test]
    fn rejects_parent_paths_and_large_files() {
        let temp = tempfile::TempDir::new().unwrap();
        std::fs::write(
            temp.path().join("large.rs"),
            vec![b'a'; MAX_READ_BYTES as usize + 1],
        )
        .unwrap();
        let tools = ReadOnlyTools::new(temp.path()).unwrap();
        for path in ["../secret.rs", "/etc/passwd", "a\\..\\secret.rs"] {
            assert!(tools.resolve(path).is_err(), "{path}");
        }
        assert!(matches!(
            tools.execute(ToolRequest::ReadFile {
                path: "large.rs".to_owned()
            }),
            Err(ToolError::UnsupportedFile)
        ));
    }

    #[cfg(unix)]
    #[test]
    fn blocks_symlink_escape_from_workspace() {
        let root = tempfile::TempDir::new().unwrap();
        let outside = tempfile::TempDir::new().unwrap();
        let target = outside.path().join("secret.rs");
        std::fs::write(&target, "secret").unwrap();
        std::os::unix::fs::symlink(&target, root.path().join("source.rs")).unwrap();
        let tools = ReadOnlyTools::new(root.path()).unwrap();
        assert!(matches!(
            tools.execute(ToolRequest::ReadFile {
                path: "source.rs".to_owned()
            }),
            Err(ToolError::OutsideWorkspace)
        ));
    }
}
