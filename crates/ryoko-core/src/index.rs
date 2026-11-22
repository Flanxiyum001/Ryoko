//! Persistent workspace symbol and call index.
//!
//! Tree-sitter still owns parsing. This module stores the resulting compact
//! outlines in SQLite so a prompt can pull a definition and its callers from
//! other files without reading every body again. The database lives beside the
//! worktree lease state at `.ryoko/state/symbols.sqlite3`.

use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use rusqlite::{Connection, OpenFlags, OptionalExtension, params};
use serde::{Deserialize, Serialize};

use crate::context::FileContext;

const MAX_SIGNATURE_CHARS: usize = 500;

/// Computes a fast, stable content hash for change detection (FNV-1a, 64-bit).
/// Not cryptographic; used only for local index invalidation.
pub fn content_hash(bytes: &[u8]) -> u64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in bytes {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
}

/// Current schema version for migrations.
const SCHEMA_VERSION: u32 = 3;

/// Status of a file in the index relative to the working tree.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileIndexStatus {
    Unchanged,
    Modified,
    Added,
    Deleted,
    Renamed,
}

#[derive(Debug, thiserror::Error)]
pub enum IndexError {
    #[error("symbol index failed: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("symbol index I/O failed: {0}")]
    Io(#[from] std::io::Error),
    #[error("symbol index parent directory is invalid")]
    InvalidPath,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SymbolHit {
    pub path: String,
    pub name: String,
    pub kind: String,
    pub signature: String,
    pub start_line: usize,
    pub callers: Vec<CallerHit>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CallerHit {
    pub path: String,
    pub caller: String,
    pub line: usize,
}

pub struct WorkspaceIndex {
    connection: Connection,
}

impl WorkspaceIndex {
    pub fn open(path: &Path) -> Result<Self, IndexError> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let connection = Connection::open_with_flags(
            path,
            OpenFlags::SQLITE_OPEN_READ_WRITE
                | OpenFlags::SQLITE_OPEN_CREATE
                | OpenFlags::SQLITE_OPEN_NOFOLLOW,
        )?;
        let mut index = Self { connection };
        index.initialize()?;
        Ok(index)
    }

    #[cfg(test)]
    fn in_memory() -> Result<Self, IndexError> {
        let connection = Connection::open_in_memory()?;
        let mut index = Self { connection };
        index.initialize()?;
        Ok(index)
    }

    fn initialize(&mut self) -> Result<(), IndexError> {
        // Check if this is a new database (no tables yet) or needs migration
        let needs_migration = self
            .connection
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='files'",
                [],
                |row| row.get(0),
            )
            .unwrap_or(0)
            == 0;

        if needs_migration {
            // New database - create all tables at current schema
            self.connection.execute_batch(
                "PRAGMA journal_mode=WAL;
                 CREATE TABLE IF NOT EXISTS files (
                     path TEXT PRIMARY KEY,
                     language TEXT NOT NULL,
                     content_hash UNSIGNED BIG INT NOT NULL DEFAULT 0,
                     updated_at UNSIGNED BIG INT NOT NULL DEFAULT 0
                 );
                 CREATE TABLE IF NOT EXISTS symbols (
                     path TEXT NOT NULL,
                     name TEXT NOT NULL,
                     kind TEXT NOT NULL,
                     signature TEXT NOT NULL,
                     start_line INTEGER NOT NULL,
                     end_line INTEGER NOT NULL,
                     PRIMARY KEY (path, name, start_line)
                 );
                 CREATE INDEX IF NOT EXISTS symbols_by_name ON symbols(name);
                 CREATE TABLE IF NOT EXISTS calls (
                     caller_path TEXT NOT NULL,
                     caller TEXT NOT NULL,
                     callee TEXT NOT NULL,
                     line INTEGER NOT NULL
                 );
                 CREATE INDEX IF NOT EXISTS calls_by_callee ON calls(callee);
                 CREATE INDEX IF NOT EXISTS calls_by_caller ON calls(caller_path, caller);
                 CREATE TABLE IF NOT EXISTS imports (
                     path TEXT NOT NULL,
                     imported_symbol TEXT NOT NULL,
                     source_path TEXT,
                     PRIMARY KEY (path, imported_symbol)
                 );
                 CREATE INDEX IF NOT EXISTS imports_by_source ON imports(source_path);
                 CREATE TABLE IF NOT EXISTS test_files (
                     path TEXT PRIMARY KEY,
                     test_framework TEXT NOT NULL
                 );",
            )?;
        }
        // Always check for migrations after schema creation
        let version: u32 = self
            .connection
            .query_row(
                "SELECT COALESCE(MAX(value), 0) FROM pragma_user_version",
                [],
                |row| row.get(0),
            )
            .unwrap_or(0);
        if version < SCHEMA_VERSION {
            self.migrate(version)?;
        }
        Ok(())
    }

    fn migrate(&mut self, current_version: u32) -> Result<(), IndexError> {
        if current_version >= SCHEMA_VERSION {
            return Ok(());
        }

        let transaction = self.connection.transaction()?;

        if current_version < 2 {
            // Version 2: Add content_hash, updated_at, imports, test_files tables
            transaction.execute(
                "ALTER TABLE files ADD COLUMN content_hash UNSIGNED BIG INT NOT NULL DEFAULT 0",
                [],
            )?;
            transaction.execute(
                "ALTER TABLE files ADD COLUMN updated_at UNSIGNED BIG INT NOT NULL DEFAULT 0",
                [],
            )?;
            // Add imports table
            transaction.execute(
                "CREATE TABLE IF NOT EXISTS imports (
                    path TEXT NOT NULL,
                    imported_symbol TEXT NOT NULL,
                    source_path TEXT,
                    PRIMARY KEY (path, imported_symbol)
                )",
                [],
            )?;
            transaction.execute(
                "CREATE INDEX IF NOT EXISTS imports_by_source ON imports(source_path)",
                [],
            )?;
            // Add test_files table
            transaction.execute(
                "CREATE TABLE IF NOT EXISTS test_files (
                    path TEXT PRIMARY KEY,
                    test_framework TEXT NOT NULL
                )",
                [],
            )?;
        }

        transaction.commit()?;
        // PRAGMA user_version uses a statement; execute directly on connection
        self.connection.execute("PRAGMA user_version = 3", [])?;
        Ok(())
    }

    /// Replaces the index atomically. Callers should pass bounded, parsed files.
    pub fn rebuild<'a>(
        &mut self,
        files: impl IntoIterator<Item = &'a FileContext>,
    ) -> Result<usize, IndexError> {
        let transaction = self.connection.transaction()?;
        transaction.execute("DELETE FROM files", [])?;
        transaction.execute("DELETE FROM symbols", [])?;
        transaction.execute("DELETE FROM calls", [])?;
        transaction.execute("DELETE FROM imports", [])?;
        transaction.execute("DELETE FROM test_files", [])?;
        let mut count = 0;
        let now = current_timestamp();
        for file in files {
            let hash = content_hash(file.path.as_bytes()); // Use path as fallback, real hash computed externally
            transaction.execute(
                "INSERT OR REPLACE INTO files(path, language, content_hash, updated_at) VALUES (?1, ?2, ?3, ?4)",
                params![file.path, file.language, hash as i64, now as i64],
            )?;
            for symbol in &file.symbols {
                transaction.execute(
                    "INSERT OR REPLACE INTO symbols
                     (path, name, kind, signature, start_line, end_line)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                    params![
                        file.path,
                        symbol.name,
                        symbol.kind,
                        bound(&symbol.signature, MAX_SIGNATURE_CHARS),
                        symbol.start_line as i64,
                        symbol.end_line as i64,
                    ],
                )?;
            }
            for call in &file.calls {
                transaction.execute(
                    "INSERT INTO calls(caller_path, caller, callee, line)
                     VALUES (?1, ?2, ?3, ?4)",
                    params![file.path, call.caller, call.callee, call.line as i64],
                )?;
            }
            // Store imports
            for import in &file.imports {
                transaction.execute(
                    "INSERT OR REPLACE INTO imports(path, imported_symbol, source_path) VALUES (?1, ?2, ?3)",
                    params![file.path, import, ""], // source_path unknown at this level
                )?;
            }
            count += 1;
        }
        transaction.commit()?;
        Ok(count)
    }

    /// Updates or inserts a single file in the index transactionally.
    /// Returns the file's status: Added, Modified, or Unchanged.
    pub fn upsert_file(
        &mut self,
        file: &FileContext,
        content_hash: u64,
    ) -> Result<FileIndexStatus, IndexError> {
        let transaction = self.connection.transaction()?;
        let now = current_timestamp();

        // Check if file exists and get its current hash
        let existing_hash: Option<u64> = transaction
            .query_row(
                "SELECT content_hash FROM files WHERE path = ?1",
                params![file.path],
                |row| row.get::<_, i64>(0).map(|v| v as u64),
            )
            .optional()?;

        let status = match existing_hash {
            Some(h) if h == content_hash => FileIndexStatus::Unchanged,
            Some(_) => FileIndexStatus::Modified,
            None => FileIndexStatus::Added,
        };

        if status != FileIndexStatus::Unchanged {
            // Delete old data for this file
            transaction.execute("DELETE FROM symbols WHERE path = ?1", params![file.path])?;
            transaction.execute(
                "DELETE FROM calls WHERE caller_path = ?1",
                params![file.path],
            )?;
            transaction.execute("DELETE FROM imports WHERE path = ?1", params![file.path])?;

            // Insert new file record
            transaction.execute(
                "INSERT OR REPLACE INTO files(path, language, content_hash, updated_at) VALUES (?1, ?2, ?3, ?4)",
                params![file.path, file.language, content_hash as i64, now as i64],
            )?;

            // Insert symbols
            for symbol in &file.symbols {
                transaction.execute(
                    "INSERT OR REPLACE INTO symbols
                     (path, name, kind, signature, start_line, end_line)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                    params![
                        file.path,
                        symbol.name,
                        symbol.kind,
                        bound(&symbol.signature, MAX_SIGNATURE_CHARS),
                        symbol.start_line as i64,
                        symbol.end_line as i64,
                    ],
                )?;
            }

            // Insert calls
            for call in &file.calls {
                transaction.execute(
                    "INSERT INTO calls(caller_path, caller, callee, line)
                     VALUES (?1, ?2, ?3, ?4)",
                    params![file.path, call.caller, call.callee, call.line as i64],
                )?;
            }

            // Insert imports
            for import in &file.imports {
                transaction.execute(
                    "INSERT OR REPLACE INTO imports(path, imported_symbol, source_path) VALUES (?1, ?2, ?3)",
                    params![file.path, import, ""],
                )?;
            }
        }

        transaction.commit()?;
        Ok(status)
    }

    /// Deletes a file from the index.
    pub fn delete_file(&mut self, path: &str) -> Result<bool, IndexError> {
        let transaction = self.connection.transaction()?;
        let deleted = transaction.execute("DELETE FROM files WHERE path = ?1", params![path])? > 0;
        transaction.execute("DELETE FROM symbols WHERE path = ?1", params![path])?;
        transaction.execute("DELETE FROM calls WHERE caller_path = ?1", params![path])?;
        transaction.execute("DELETE FROM imports WHERE path = ?1", params![path])?;
        transaction.commit()?;
        Ok(deleted)
    }

    /// Gets the content hash and update time for a file in the index.
    pub fn file_info(&self, path: &str) -> Result<Option<(u64, u64)>, IndexError> {
        let row = self
            .connection
            .query_row(
                "SELECT content_hash, updated_at FROM files WHERE path = ?1",
                params![path],
                |row| Ok((row.get::<_, i64>(0)? as u64, row.get::<_, i64>(1)? as u64)),
            )
            .optional()?;
        Ok(row)
    }

    /// Lists all indexed files with their content hashes and update times.
    pub fn list_files(&self) -> Result<Vec<(String, String, u64, u64)>, IndexError> {
        let mut stmt = self
            .connection
            .prepare("SELECT path, language, content_hash, updated_at FROM files ORDER BY path")?;
        let rows = stmt.query_map([], |row| {
            Ok((
                row.get(0)?,
                row.get(1)?,
                row.get::<_, i64>(2)? as u64,
                row.get::<_, i64>(3)? as u64,
            ))
        })?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    /// Finds files that import a given symbol.
    pub fn find_importers(&self, symbol: &str, limit: usize) -> Result<Vec<String>, IndexError> {
        let limit = limit.clamp(1, 100) as i64;
        let mut stmt = self
            .connection
            .prepare("SELECT DISTINCT path FROM imports WHERE imported_symbol = ?1 LIMIT ?2")?;
        let rows = stmt.query_map(params![symbol, limit], |row| row.get(0))?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    /// Marks a file as a test file with its framework.
    pub fn mark_test_file(&mut self, path: &str, framework: &str) -> Result<(), IndexError> {
        self.connection.execute(
            "INSERT OR REPLACE INTO test_files(path, test_framework) VALUES (?1, ?2)",
            params![path, framework],
        )?;
        Ok(())
    }

    /// Gets test files for a given framework.
    pub fn test_files(&self, framework: Option<&str>) -> Result<Vec<(String, String)>, IndexError> {
        match framework {
            Some(fw) => {
                let mut stmt = self.connection.prepare(
                    "SELECT path, test_framework FROM test_files WHERE test_framework = ?1",
                )?;
                let rows = stmt.query_map(params![fw], |row| Ok((row.get(0)?, row.get(1)?)))?;
                rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
            }
            None => {
                let mut stmt = self
                    .connection
                    .prepare("SELECT path, test_framework FROM test_files")?;
                let rows = stmt.query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?;
                rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
            }
        }
    }

    /// Detects test files from symbols and imports heuristically.
    pub fn detect_test_files(&mut self, files: &[FileContext]) -> Result<usize, IndexError> {
        let mut count = 0;
        for file in files {
            let is_test = file.symbols.iter().any(|s| {
                s.name.starts_with("test_")
                    || s.name.ends_with("_test")
                    || s.kind == "test"
                    || file.path.contains("/test")
                    || file.path.contains("/tests/")
            }) || file.imports.iter().any(|imp| {
                imp.contains("test")
                    || imp.contains("testing")
                    || imp.contains("assert")
                    || imp.contains("#[test]")
            });

            if is_test {
                let framework = detect_test_framework(&file.language, &file.imports, &file.symbols);
                self.mark_test_file(&file.path, &framework)?;
                count += 1;
            }
        }
        Ok(count)
    }

    pub fn symbol(&self, name: &str, limit: usize) -> Result<Vec<SymbolHit>, IndexError> {
        let limit = limit.clamp(1, 50) as i64;
        let mut statement = self.connection.prepare(
            "SELECT path, name, kind, signature, start_line
             FROM symbols WHERE name = ?1 ORDER BY path, start_line LIMIT ?2",
        )?;
        let rows = statement.query_map(params![name, limit], |row| {
            Ok(SymbolHit {
                path: row.get(0)?,
                name: row.get(1)?,
                kind: row.get(2)?,
                signature: row.get(3)?,
                start_line: row.get::<_, i64>(4)? as usize,
                callers: Vec::new(),
            })
        })?;
        let mut hits = rows.collect::<Result<Vec<_>, _>>()?;
        let mut callers = self.connection.prepare(
            "SELECT caller_path, caller, line FROM calls
             WHERE callee = ?1 ORDER BY caller_path, line LIMIT 20",
        )?;
        for hit in &mut hits {
            let rows = callers.query_map(params![hit.name], |row| {
                Ok(CallerHit {
                    path: row.get(0)?,
                    caller: row.get(1)?,
                    line: row.get::<_, i64>(2)? as usize,
                })
            })?;
            hit.callers = rows.collect::<Result<Vec<_>, _>>()?;
        }
        Ok(hits)
    }

    /// Compact prompt-ready context for symbol-like goal terms. Duplicate
    /// definitions and callers are removed, and the result is character-bounded.
    pub fn context_for_terms(
        &self,
        terms: &[String],
        max_chars: usize,
    ) -> Result<String, IndexError> {
        let mut rendered = String::new();
        let mut seen = std::collections::BTreeSet::new();
        for term in terms.iter().take(12) {
            let hits = self.symbol(term, 6)?;
            if hits.is_empty() {
                continue;
            }
            let mut header_written = false;
            for hit in hits {
                let key = format!("{}:{}:{}", hit.path, hit.name, hit.start_line);
                if !seen.insert(key) {
                    continue;
                }
                if !header_written {
                    rendered.push_str(&format!("\n### `{term}`\n"));
                    header_written = true;
                }
                rendered.push_str(&format!(
                    "- {} {} at {}:{}\n",
                    hit.kind, hit.signature, hit.path, hit.start_line
                ));
                for caller in hit.callers.iter().take(5) {
                    let caller_key = format!("{}:{}:{}", caller.path, caller.caller, caller.line);
                    if seen.insert(caller_key) {
                        rendered.push_str(&format!(
                            "  caller: {} at {}:{}\n",
                            caller.caller, caller.path, caller.line
                        ));
                    }
                }
            }
            if rendered.chars().count() >= max_chars {
                break;
            }
        }
        if rendered.is_empty() {
            return Ok(rendered);
        }
        Ok(bound(
            &format!("Cross-file symbol index:\n{rendered}"),
            max_chars.max(1),
        ))
    }
}

fn bound(value: &str, max_chars: usize) -> String {
    if value.chars().count() <= max_chars {
        return value.to_owned();
    }
    let mut output = value
        .chars()
        .take(max_chars.saturating_sub(1))
        .collect::<String>();
    output.push('…');
    output
}

/// Extracts useful symbol-search terms from a natural-language goal. This is a
/// deliberately small heuristic, not a tokenizer or embedding model.
pub fn goal_terms(goal: &str) -> Vec<String> {
    const STOP: &[&str] = &[
        "the",
        "and",
        "for",
        "with",
        "from",
        "into",
        "this",
        "that",
        "add",
        "fix",
        "use",
        "using",
        "make",
        "implement",
        "update",
        "create",
        "build",
        "test",
        "tests",
        "code",
        "file",
        "files",
        "should",
        "when",
        "where",
        "what",
        "have",
        "will",
        "your",
        "need",
        "want",
        "please",
    ];
    let mut terms = Vec::new();
    let mut word = String::new();
    for character in goal.chars() {
        if character.is_alphanumeric() || character == '_' {
            word.push(character);
        } else if !word.is_empty() {
            push_term(&mut terms, &word, STOP);
            word.clear();
        }
    }
    if !word.is_empty() {
        push_term(&mut terms, &word, STOP);
    }
    terms.truncate(12);
    terms
}

fn push_term(terms: &mut Vec<String>, word: &str, stop: &[&str]) {
    if (3..=60).contains(&word.len())
        && !stop.contains(&word.to_ascii_lowercase().as_str())
        && !terms.iter().any(|term| term == word)
    {
        terms.push(word.to_owned());
    }
}

fn current_timestamp() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn detect_test_framework(
    language: &str,
    imports: &[String],
    _symbols: &[crate::context::Symbol],
) -> String {
    match language {
        "rust" => {
            if imports.iter().any(|i| i.contains("proptest")) {
                "proptest".to_string()
            } else if imports.iter().any(|i| i.contains("quickcheck")) {
                "quickcheck".to_string()
            } else {
                "builtin".to_string()
            }
        }
        "typescript" | "tsx" | "javascript" => {
            if imports.iter().any(|i| i.contains("vitest")) {
                "vitest".to_string()
            } else if imports.iter().any(|i| i.contains("jest")) {
                "jest".to_string()
            } else if imports.iter().any(|i| i.contains("mocha")) {
                "mocha".to_string()
            } else {
                "unknown".to_string()
            }
        }
        "python" => {
            if imports.iter().any(|i| i.contains("pytest")) {
                "pytest".to_string()
            } else if imports.iter().any(|i| i.contains("unittest")) {
                "unittest".to_string()
            } else {
                "unknown".to_string()
            }
        }
        "go" => "builtin".to_string(),
        "java" => {
            if imports.iter().any(|i| i.contains("junit")) {
                "junit".to_string()
            } else if imports.iter().any(|i| i.contains("testng")) {
                "testng".to_string()
            } else {
                "unknown".to_string()
            }
        }
        "kotlin" => {
            if imports.iter().any(|i| i.contains("junit")) {
                "junit".to_string()
            } else if imports.iter().any(|i| i.contains("kotest")) {
                "kotest".to_string()
            } else {
                "unknown".to_string()
            }
        }
        "csharp" => {
            if imports.iter().any(|i| i.contains("xunit")) {
                "xunit".to_string()
            } else if imports.iter().any(|i| i.contains("nunit")) {
                "nunit".to_string()
            } else if imports.iter().any(|i| i.contains("mstest")) {
                "mstest".to_string()
            } else {
                "unknown".to_string()
            }
        }
        _ => "unknown".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::context::{CallEdge, FileContext, Symbol};

    fn context(path: &str, symbols: &[&str], calls: &[(&str, &str)]) -> FileContext {
        FileContext {
            path: path.to_owned(),
            language: "rust".to_owned(),
            has_syntax_errors: false,
            imports: Vec::new(),
            symbols: symbols
                .iter()
                .map(|name| Symbol {
                    name: (*name).to_owned(),
                    kind: "function".to_owned(),
                    signature: format!("fn {name}()"),
                    start_line: 1,
                    end_line: 2,
                })
                .collect(),
            calls: calls
                .iter()
                .map(|(caller, callee)| CallEdge {
                    caller: (*caller).to_owned(),
                    callee: (*callee).to_owned(),
                    line: 3,
                })
                .collect(),
            snippets: Vec::new(),
        }
    }

    #[test]
    fn finds_definitions_and_cross_file_callers() {
        let mut index = WorkspaceIndex::in_memory().unwrap();
        let files = [
            context("src/lib.rs", &["run"], &[]),
            context("src/main.rs", &["main"], &[("main", "run")]),
        ];
        assert_eq!(index.rebuild(&files).unwrap(), 2);
        let hits = index.symbol("run", 5).unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].path, "src/lib.rs");
        assert_eq!(hits[0].callers[0].path, "src/main.rs");
        assert_eq!(hits[0].callers[0].caller, "main");
    }

    #[test]
    fn context_is_bounded_and_deduplicated() {
        let mut index = WorkspaceIndex::in_memory().unwrap();
        index
            .rebuild([&context(
                "src/lib.rs",
                &["run"],
                &[("main", "run"), ("test", "run")],
            )])
            .unwrap();
        let context = index
            .context_for_terms(&["run".to_owned(), "run".to_owned()], 160)
            .unwrap();
        assert!(context.contains("`run`"));
        assert!(context.contains("caller: main"));
        assert!(context.chars().count() <= 160);
    }

    #[cfg(unix)]
    #[test]
    fn refuses_a_symlinked_database_file() {
        let directory = tempfile::TempDir::new().unwrap();
        let target = directory.path().join("target.sqlite3");
        std::fs::write(&target, []).unwrap();
        let link = directory.path().join("symbols.sqlite3");
        std::os::unix::fs::symlink(&target, &link).unwrap();
        assert!(WorkspaceIndex::open(&link).is_err());
    }

    #[test]
    fn goal_terms_are_bounded_and_ignore_stop_words() {
        let terms =
            goal_terms("Please implement the parser cache and update ContextExtractor tests");
        assert!(terms.contains(&"parser".to_owned()));
        assert!(terms.contains(&"ContextExtractor".to_owned()));
        assert!(!terms.contains(&"the".to_owned()));
    }
}
