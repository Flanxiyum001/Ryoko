//! Bounded structural context for agent prompts.
//!
//! Scans a project for supported source files and renders their imports,
//! symbol outlines and call edges with the `ryoko-core` multi-language
//! extractor. Only root-confined regular files are read; symlinks, hidden
//! paths, build/dependency directories and oversized files are skipped.
//! Input is bounded by file count, depth and per-file size; callers truncate
//! the rendered outline to their own prompt budget.

use std::path::{Path, PathBuf};

use ryoko_core::context::{Language, SourceFile, extract_files};

const MAX_FILES: usize = 60;
const MAX_DEPTH: usize = 6;
const MAX_FILE_BYTES: u64 = 1_000_000;

const IGNORED_DIRS: &[&str] = &[
    "target",
    "node_modules",
    "dist",
    "build",
    "out",
    "vendor",
    "venv",
    "__pycache__",
    "coverage",
];

/// Renders imports, symbols and call edges for the source files under `root`.
/// Returns an empty string when there is nothing worth including.
pub fn structural_context(root: &Path) -> String {
    let mut files = Vec::new();
    walk(root, 0, &mut files);
    files.sort();

    let mut sources: Vec<SourceFile> = Vec::with_capacity(files.len());
    for path in files {
        let Ok(relative) = path.strip_prefix(root) else {
            continue;
        };
        let Some(relative) = relative.to_str() else {
            continue;
        };
        // Keep prompt paths stable across platforms.
        sources.push((relative.replace('\\', "/"), path));
    }

    // One parse per file, spread over the available workers. The sorted input
    // keeps the rendered outline stable.
    let mut rendered = String::new();
    for context in extract_files(sources) {
        if context.symbols.is_empty() && context.imports.is_empty() {
            continue;
        }
        rendered.push_str(&context.render_outline());
        rendered.push('\n');
    }
    rendered
}

fn walk(directory: &Path, depth: usize, files: &mut Vec<PathBuf>) {
    if depth > MAX_DEPTH || files.len() >= MAX_FILES {
        return;
    }
    let Ok(entries) = std::fs::read_dir(directory) else {
        return;
    };
    let mut entries: Vec<_> = entries.filter_map(Result::ok).collect();
    entries.sort_by_key(|entry| entry.file_name());
    for entry in entries {
        if files.len() >= MAX_FILES {
            break;
        }
        let name = entry.file_name();
        let Some(name) = name.to_str() else {
            continue;
        };
        if name.starts_with('.') {
            continue;
        }
        let Ok(file_type) = entry.file_type() else {
            continue;
        };
        if file_type.is_symlink() {
            continue;
        }
        if file_type.is_dir() {
            if IGNORED_DIRS
                .iter()
                .any(|ignored| ignored.eq_ignore_ascii_case(name))
            {
                continue;
            }
            walk(&entry.path(), depth + 1, files);
        } else if file_type.is_file() && is_supported_source(&entry.path()) {
            let size = entry
                .metadata()
                .map(|metadata| metadata.len())
                .unwrap_or(u64::MAX);
            if size <= MAX_FILE_BYTES {
                files.push(entry.path());
            }
        }
    }
}

fn is_supported_source(path: &Path) -> bool {
    path.to_str().and_then(Language::from_path).is_some()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn renders_supported_sources_and_skips_build_directories() {
        let dir = tempfile::TempDir::new().unwrap();
        std::fs::create_dir_all(dir.path().join("src")).unwrap();
        std::fs::create_dir_all(dir.path().join("target")).unwrap();
        std::fs::write(
            dir.path().join("src/App.tsx"),
            "export function Greeting() {\n  return <h1>Hello</h1>;\n}\n",
        )
        .unwrap();
        std::fs::write(
            dir.path().join("src/service.py"),
            "class Store:\n    def save(self, value):\n        return value\n",
        )
        .unwrap();
        std::fs::write(dir.path().join("target/ignored.rs"), "fn ignored() {}\n").unwrap();
        std::fs::write(dir.path().join("README.md"), "readme\n").unwrap();

        let context = structural_context(dir.path());
        assert!(context.contains("### src/App.tsx (tsx)"));
        assert!(context.contains("function Greeting"));
        assert!(context.contains("### src/service.py (python)"));
        assert!(context.contains("method save"));
        assert!(!context.contains("ignored"));
        assert!(!context.contains("README"));
    }
}
