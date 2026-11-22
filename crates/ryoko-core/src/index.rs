//! Persistent workspace symbol and call index.
//!
//! Tree-sitter still owns parsing. This module stores the resulting compact
//! outlines in SQLite so a prompt can pull a definition and its callers from
//! other files without reading every body again. The database lives beside the
//! worktree lease state at `.ryoko/state/symbols.sqlite3`.

use std::collections::BTreeSet;
use std::path::Path;

use rusqlite::{Connection, OpenFlags, params};
use serde::{Deserialize, Serialize};

use crate::context::FileContext;

const MAX_SIGNATURE_CHARS: usize = 500;

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
        let index = Self { connection };
        index.initialize()?;
        Ok(index)
    }

    #[cfg(test)]
    fn in_memory() -> Result<Self, IndexError> {
        let index = Self {
            connection: Connection::open_in_memory()?,
        };
        index.initialize()?;
        Ok(index)
    }

    fn initialize(&self) -> Result<(), IndexError> {
        self.connection.execute_batch(
            "PRAGMA journal_mode=WAL;
             CREATE TABLE IF NOT EXISTS files (
                 path TEXT PRIMARY KEY,
                 language TEXT NOT NULL,
                 content_hash TEXT NOT NULL DEFAULT ''
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
             CREATE INDEX IF NOT EXISTS calls_by_caller ON calls(caller_path, caller);",
        )?;

        // Existing databases predate content hashing. SQLite ADD COLUMN with a
        // default is compatible with their existing rows and is safe to repeat.
        let has_hash = {
            let mut statement = self.connection.prepare("PRAGMA table_info(files)")?;
            let rows = statement.query_map([], |row| row.get::<_, String>(1))?;
            rows.collect::<Result<Vec<_>, _>>()?
                .into_iter()
                .any(|name| name == "content_hash")
        };
        if !has_hash {
            self.connection.execute(
                "ALTER TABLE files ADD COLUMN content_hash TEXT NOT NULL DEFAULT ''",
                [],
            )?;
        }
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
        let mut count = 0;
        for file in files {
            transaction.execute(
                "INSERT OR REPLACE INTO files(path, language) VALUES (?1, ?2)",
                params![file.path, file.language],
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
            count += 1;
        }
        transaction.commit()?;
        Ok(count)
    }

    /// A stored content hash, or None when the file has not been indexed.
    /// A blank hash identifies a record written by the older full-rebuild index.
    pub fn content_hash(&self, path: &str) -> Result<Option<String>, IndexError> {
        use rusqlite::OptionalExtension;

        let value = self
            .connection
            .query_row(
                "SELECT content_hash FROM files WHERE path = ?1",
                params![path],
                |row| row.get(0),
            )
            .optional()?;
        Ok(value)
    }

    /// Applies only changed, already-parsed files and removes disappeared paths.
    /// The caller must calculate each hash from source bytes before parsing,
    /// so unchanged files do not need to enter the tree-sitter pipeline.
    pub fn sync_files(
        &mut self,
        changed: &[(FileContext, String)],
        present: &BTreeSet<String>,
    ) -> Result<usize, IndexError> {
        let transaction = self.connection.transaction()?;
        let old_paths = {
            let mut statement = transaction.prepare("SELECT path FROM files")?;
            let rows = statement.query_map([], |row| row.get::<_, String>(0))?;
            rows.collect::<Result<Vec<_>, _>>()?
        };

        for path in old_paths {
            if !present.contains(&path) {
                transaction.execute("DELETE FROM symbols WHERE path = ?1", params![path])?;
                transaction.execute("DELETE FROM calls WHERE caller_path = ?1", params![path])?;
                transaction.execute("DELETE FROM files WHERE path = ?1", params![path])?;
            }
        }

        for (file, hash) in changed {
            if !present.contains(&file.path) {
                continue;
            }
            transaction.execute("DELETE FROM symbols WHERE path = ?1", params![file.path])?;
            transaction.execute(
                "DELETE FROM calls WHERE caller_path = ?1",
                params![file.path],
            )?;
            transaction.execute(
                "INSERT OR REPLACE INTO files(path, language, content_hash) VALUES (?1, ?2, ?3)",
                params![file.path, file.language, hash],
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
        }
        transaction.commit()?;
        Ok(changed.len())
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

    #[test]
    fn incrementally_updates_changed_files_and_removes_deleted_paths() {
        let mut index = WorkspaceIndex::in_memory().unwrap();
        let old = context("src/lib.rs", &["before"], &[]);
        let present = BTreeSet::from(["src/lib.rs".to_owned()]);
        assert_eq!(
            index.sync_files(&[(old, "hash-one".to_owned())], &present).unwrap(),
            1
        );
        assert_eq!(
            index.content_hash("src/lib.rs").unwrap(),
            Some("hash-one".to_owned())
        );
        assert_eq!(index.symbol("before", 5).unwrap().len(), 1);

        // Caller can skip an unchanged file without deleting its symbols.
        assert_eq!(index.sync_files(&[], &present).unwrap(), 0);
        assert_eq!(index.symbol("before", 5).unwrap().len(), 1);

        let new = context("src/lib.rs", &["after"], &[]);
        assert_eq!(
            index.sync_files(&[(new, "hash-two".to_owned())], &present).unwrap(),
            1
        );
        assert!(index.symbol("before", 5).unwrap().is_empty());
        assert_eq!(index.symbol("after", 5).unwrap().len(), 1);

        assert_eq!(index.sync_files(&[], &BTreeSet::new()).unwrap(), 0);
        assert!(index.symbol("after", 5).unwrap().is_empty());
        assert_eq!(index.content_hash("src/lib.rs").unwrap(), None);
    }

    #[test]
    fn legacy_index_schema_migrates_without_losing_file_records() {
        let directory = tempfile::TempDir::new().unwrap();
        let path = directory.path().join("symbols.sqlite3");
        let connection = Connection::open(&path).unwrap();
        connection
            .execute_batch(
                "CREATE TABLE files(path TEXT PRIMARY KEY, language TEXT NOT NULL);
                 INSERT INTO files(path, language) VALUES ('src/lib.rs', 'rust');",
            )
            .unwrap();
        drop(connection);

        let index = WorkspaceIndex::open(&path).unwrap();
        assert_eq!(
            index.content_hash("src/lib.rs").unwrap(),
            Some(String::new())
        );
        drop(index);
        let index = WorkspaceIndex::open(&path).unwrap();
        assert_eq!(
            index.content_hash("src/lib.rs").unwrap(),
            Some(String::new())
        );
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
