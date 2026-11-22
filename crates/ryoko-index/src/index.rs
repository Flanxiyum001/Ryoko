//! Symbol and call index with incremental update support.
//!
//! This module provides a SQLite-backed index that supports:
//! - Incremental updates when files change
//! - Content-hash-based detection of missed events
//! - Cross-file symbol lookup and call discovery
//! - Transactional batch updates

use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;

use rusqlite::{Connection, OpenFlags, params};
use serde::{Deserialize, Serialize};

use crate::hash::ContentHash;
use crate::error::IndexError;

/// Maximum characters in a signature stored in the index.
const MAX_SIGNATURE_CHARS: usize = 500;

/// A symbol definition found in a source file.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct IndexedSymbol {
    /// Symbol name (function, struct, class, etc.)
    pub name: String,
    /// Symbol kind (function, struct, class, method, etc.)
    pub kind: String,
    /// Declaration signature (first line up to `{` or `(`)
    pub signature: String,
    /// Starting line number (1-indexed)
    pub start_line: usize,
    /// Ending line number (1-indexed)
    pub end_line: usize,
}

/// A call from one symbol to another.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CallEdge {
    /// Name of the calling symbol.
    pub caller: String,
    /// Name of the callee (target of call).
    pub callee: String,
    /// Line number where the call occurs.
    pub line: usize,
}

/// Metadata about a tracked file in the index.
#[derive(Debug, Clone)]
struct FileRecord {
    /// Path relative to the workspace root.
    path: String,
    /// Language identifier (rust, typescript, python, etc.)
    language: String,
    /// Content hash for change detection.
    hash: ContentHash,
    /// Last modified timestamp.
    modified_ms: i64,
}

/// Result of looking up a symbol.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SymbolHit {
    /// File path.
    pub path: String,
    /// Symbol name.
    pub name: String,
    /// Symbol kind.
    pub kind: String,
    /// Declaration signature.
    pub signature: String,
    /// Starting line.
    pub start_line: usize,
    /// Callers that reference this symbol.
    pub callers: Vec<CallerHit>,
}

/// A call reference to a symbol.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CallerHit {
    /// File containing the call.
    pub path: String,
    /// Calling symbol name.
    pub caller: String,
    /// Line where the call occurs.
    pub line: usize,
}

/// Interface for a file context extractable by tree-sitter.
pub trait FileContextSource: Send + Sync {
    /// Returns the file path relative to workspace root.
    fn path(&self) -> &str;

    /// Returns the detected language identifier.
    fn language(&self) -> &str;

    /// Returns the list of symbols defined in this file.
    fn symbols(&self) -> &[IndexedSymbol];

    /// Returns the list of intra-file calls.
    fn calls(&self) -> &[CallEdge];

    /// Returns the raw source content.
    fn source(&self) -> &str;
}

/// The main symbol index with incremental update support.
pub struct SymbolIndex {
    connection: Connection,
    /// Cache of file hashes for reconciliation.
    file_hashes: HashMap<String, ContentHash>,
}

impl SymbolIndex {
    /// Opens or creates the index at the given path.
    pub fn open<P: AsRef<Path>>(path: P) -> Result<Self, IndexError> {
        let path = path.as_ref();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let connection = Connection::open_with_flags(
            path,
            OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_CREATE | OpenFlags::SQLITE_OPEN_NOFOLLOW,
        )?;
        let mut index = Self {
            connection,
            file_hashes: HashMap::new(),
        };
        index.initialize()?;
        Ok(index)
    }

    /// Creates an in-memory index for testing.
    #[cfg(test)]
    pub fn in_memory() -> Result<Self, IndexError> {
        let connection = Connection::open_in_memory()?;
        let mut index = Self {
            connection,
            file_hashes: HashMap::new(),
        };
        index.initialize()?;
        Ok(index)
    }

    fn initialize(&mut self) -> Result<(), IndexError> {
        self.connection.execute_batch(
            "PRAGMA journal_mode=WAL;
             CREATE TABLE IF NOT EXISTS files (
                 path TEXT PRIMARY KEY,
                 language TEXT NOT NULL,
                 hash TEXT NOT NULL,
                 modified_ms INTEGER NOT NULL
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
             CREATE TABLE IF NOT EXISTS symbols_xrefs (
                 name TEXT NOT NULL,
                 path TEXT NOT NULL,
                 PRIMARY KEY (name, path)
             );
             CREATE INDEX IF NOT EXISTS symbols_xrefs_by_name ON symbols_xrefs(name);",
        )?;

        // Load existing hashes for reconciliation
        self.load_hashes()?;
        Ok(())
    }

    fn load_hashes(&mut self) -> Result<(), IndexError> {
        let mut stmt = self.connection.prepare(
            "SELECT path, hash FROM files"
        )?;
        let rows = stmt.query_map(params![], |row| {
            let path: String = row.get(0)?;
            let hash: String = row.get(1)?;
            Ok((path, ContentHash::from(hash)))
        })?;
        for row in rows {
            let (path, hash) = row?;
            self.file_hashes.insert(path, hash);
        }
        Ok(())
    }

    /// Computes the content hash for a file.
    pub fn compute_hash(content: &str) -> ContentHash {
        ContentHash::from_str(content)
    }

    /// Updates or inserts a file's symbols in the index.
    /// This is the main incremental update method.
    pub fn update_file(&mut self, file: &dyn FileContextSource) -> Result<bool, IndexError> {
        let content = file.source();
        let new_hash = Self::compute_hash(content);
        let path = file.path().to_owned();

        // Check if this file has actually changed
        let old_hash = self.file_hashes.get(&path);
        if let Some(old) = old_hash {
            if old == &new_hash {
                // File unchanged, no update needed
                return Ok(false);
            }
        }

        let transaction = self.connection.transaction()?;

        // Delete old symbols and calls for this file
        transaction.execute("DELETE FROM symbols WHERE path = ?1", params![&path])?;
        transaction.execute("DELETE FROM calls WHERE caller_path = ?1", params![&path])?;
        transaction.execute("DELETE FROM symbols_xrefs WHERE path = ?1", params![&path])?;

        // Clear old file record
        transaction.execute("DELETE FROM files WHERE path = ?1", params![&path])?;

        // Insert new file record
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as i64;
        transaction.execute(
            "INSERT INTO files(path, language, hash, modified_ms) VALUES (?1, ?2, ?3, ?4)",
            params![&path, file.language(), new_hash.as_str(), now],
        )?;

        // Insert symbols
        for symbol in file.symbols() {
            let signature = if symbol.signature.len() > MAX_SIGNATURE_CHARS {
                format!("{}...", &symbol.signature[..MAX_SIGNATURE_CHARS.saturating_sub(3)])
            } else {
                symbol.signature.clone()
            };
            transaction.execute(
                "INSERT INTO symbols(path, name, kind, signature, start_line, end_line) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                params![&path, &symbol.name, &symbol.kind, &signature, symbol.start_line as i64, symbol.end_line as i64],
            )?;

            // Update cross-reference table
            transaction.execute(
                "INSERT OR REPLACE INTO symbols_xrefs(name, path) VALUES (?1, ?2)",
                params![&symbol.name, &path],
            )?;
        }

        // Insert call edges
        for call in file.calls() {
            transaction.execute(
                "INSERT INTO calls(caller_path, caller, callee, line) VALUES (?1, ?2, ?3, ?4)",
                params![&path, &call.caller, &call.callee, call.line as i64],
            )?;
        }

        transaction.commit()?;

        // Update hash cache
        self.file_hashes.insert(path, new_hash);
        Ok(true)
    }

    /// Removes a file from the index (deletion handling).
    pub fn remove_file(&mut self, path: &str) -> Result<bool, IndexError> {
        let count = self.connection.execute("DELETE FROM files WHERE path = ?1", params![path])?;
        self.connection.execute("DELETE FROM symbols WHERE path = ?1", params![path])?;
        self.connection.execute("DELETE FROM calls WHERE caller_path = ?1", params![path])?;
        self.connection.execute("DELETE FROM symbols_xrefs WHERE path = ?1", params![path])?;

        if count > 0 {
            self.file_hashes.remove(path);
            Ok(true)
        } else {
            Ok(false)
        }
    }

    /// Looks up a symbol by name across all files.
    pub fn symbol(&self, name: &str, limit: usize) -> Result<Vec<SymbolHit>, IndexError> {
        let limit = limit.clamp(1, 50).max(1);

        let mut statement = self.connection.prepare(
            "SELECT path, name, kind, signature, start_line FROM symbols WHERE name = ?1 ORDER BY path, start_line LIMIT ?2"
        )?;

        let rows = statement.query_map(params![name, limit as i64], |row| {
            Ok(SymbolHit {
                path: row.get(0)?,
                name: row.get(1)?,
                kind: row.get(2)?,
                signature: row.get(3)?,
                start_line: row.get::<_, i64>(4)? as usize,
                callers: Vec::new(),
            })
        })?;

        let mut hits: Vec<_> = rows.collect::<Result<Vec<_>, _>>()?;
        let mut callers = self.connection.prepare(
            "SELECT caller_path, caller, line FROM calls WHERE callee = ?1 ORDER BY caller_path, line LIMIT 20"
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

    /// Returns all files currently in the index.
    pub fn list_files(&self) -> Result<Vec<String>, IndexError> {
        let mut stmt = self.connection.prepare("SELECT path FROM files ORDER BY path")?;
        let rows = stmt.query_map(params![], |row| row.get(0))?;
        let files: Result<Vec<String>, rusqlite::Error> = rows.collect();
        files.map_err(IndexError::from)
    }

    /// Returns the hash of a file if it exists in the index.
    pub fn get_file_hash(&self, path: &str) -> Option<ContentHash> {
        self.connection
            .prepare("SELECT hash FROM files WHERE path = ?1")
            .ok()?
            .query_row(params![path], |row| Ok(ContentHash::from(row.get::<_, String>(0)?)))
            .ok()
    }

    /// Context for symbol-like goal terms (bounded and deduplicated).
    pub fn context_for_terms(&self, terms: &[String], max_chars: usize) -> Result<String, IndexError> {
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
                    "- {} {} at {}:{} ({})\n",
                    hit.kind, hit.signature, hit.path, hit.start_line, hit.start_line
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
        Ok(bound(&format!("Cross-file symbol index:\n{rendered}"), max_chars.max(1)))
    }

    /// Returns statistics about the index.
    pub fn stats(&self) -> Result<IndexStats, IndexError> {
        let file_count: i64 = self.connection.query_row(
            "SELECT COUNT(*) FROM files", [], |row| row.get(0)
        )?;
        let symbol_count: i64 = self.connection.query_row(
            "SELECT COUNT(*) FROM symbols", [], |row| row.get(0)
        )?;
        let call_count: i64 = self.connection.query_row(
            "SELECT COUNT(*) FROM calls", [], |row| row.get(0)
        )?;

        Ok(IndexStats {
            file_count: file_count as usize,
            symbol_count: symbol_count as usize,
            call_count: call_count as usize,
        })
    }

    /// Clears all index data (for testing/rebuild).
    pub fn clear(&mut self) -> Result<(), IndexError> {
        self.connection.execute_batch(
            "DELETE FROM files; DELETE FROM symbols; DELETE FROM calls; DELETE FROM symbols_xrefs;"
        )?;
        self.file_hashes.clear();
        Ok(())
    }
}

/// Statistics about the index contents.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IndexStats {
    /// Number of files tracked.
    pub file_count: usize,
    /// Number of symbols indexed.
    pub symbol_count: usize,
    /// Number of call edges.
    pub call_count: usize,
}

impl IndexStats {
    /// Returns true if the index is empty.
    pub fn is_empty(&self) -> bool {
        self.file_count == 0 && self.symbol_count == 0
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

/// Extracts goal terms from a natural language description.
pub fn goal_terms(goal: &str) -> Vec<String> {
    const STOP: &[&str] = &[
        "the", "and", "for", "with", "from", "into", "this", "that",
        "add", "fix", "use", "using", "make", "implement", "update",
        "create", "build", "test", "tests", "code", "file", "files",
        "should", "when", "where", "what", "have", "will", "your",
        "need", "want", "please",
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

    #[test]
    fn goal_terms_extracts_meaningful_words() {
        let terms = goal_terms("Please implement the parser cache and update ContextExtractor tests");
        assert!(terms.contains(&"parser".to_owned()));
        assert!(terms.contains(&"ContextExtractor".to_owned()));
        assert!(!terms.contains(&"the".to_owned()));
    }

    #[test]
    fn symbol_hit_serialization_works() {
        let hit = SymbolHit {
            path: "src/lib.rs".to_owned(),
            name: "run".to_owned(),
            kind: "function".to_owned(),
            signature: "fn run()".to_owned(),
            start_line: 42,
            callers: vec![
                CallerHit {
                    path: "src/main.rs".to_owned(),
                    caller: "main".to_owned(),
                    line: 10,
                }
            ],
        };
        let json = serde_json::to_string(&hit).unwrap();
        let recovered: SymbolHit = serde_json::from_str(&json).unwrap();
        assert_eq!(hit, recovered);
    }

    #[test]
    fn index_stats_reports_zeros_for_empty_index() {
        let index = SymbolIndex::in_memory().unwrap();
        let stats = index.stats().unwrap();
        assert_eq!(stats.file_count, 0);
        assert_eq!(stats.symbol_count, 0);
        assert!(stats.is_empty());
    }

    // Test file context implementations
    struct TestFileContext {
        path: String,
        language: String,
        symbols: Vec<IndexedSymbol>,
        calls: Vec<CallEdge>,
        source: String,
    }

    impl FileContextSource for TestFileContext {
        fn path(&self) -> &str { &self.path }
        fn language(&self) -> &str { &self.language }
        fn symbols(&self) -> &[IndexedSymbol] { &self.symbols }
        fn calls(&self) -> &[CallEdge] { &self.calls }
        fn source(&self) -> &str { &self.source }
    }

    #[test]
    fn incremental_update_only_when_changed() {
        let mut index = SymbolIndex::in_memory().unwrap();

        let file = TestFileContext {
            path: "src/lib.rs".to_owned(),
            language: "rust".to_owned(),
            symbols: vec![IndexedSymbol {
                name: "run".to_owned(),
                kind: "function".to_owned(),
                signature: "fn run()".to_owned(),
                start_line: 1,
                end_line: 5,
            }],
            calls: vec![],
            source: "pub fn run() {}".to_owned(),
        };

        // First update should succeed
        let changed = index.update_file(&file).unwrap();
        assert!(changed);

        // Second update with same content should be a no-op
        let changed = index.update_file(&file).unwrap();
        assert!(!changed);
    }

    #[test]
    fn symbol_lookup_finds_definitions_and_callers() {
        let mut index = SymbolIndex::in_memory().unwrap();

        // Add a symbol in one file
        let lib = TestFileContext {
            path: "src/lib.rs".to_owned(),
            language: "rust".to_owned(),
            symbols: vec![IndexedSymbol {
                name: "run".to_owned(),
                kind: "function".to_owned(),
                signature: "fn run()".to_owned(),
                start_line: 1,
                end_line: 5,
            }],
            calls: vec![],
            source: "pub fn run() {}".to_owned(),
        };
        index.update_file(&lib).unwrap();

        // Add a caller in another file
        let main = TestFileContext {
            path: "src/main.rs".to_owned(),
            language: "rust".to_owned(),
            symbols: vec![IndexedSymbol {
                name: "main".to_owned(),
                kind: "function".to_owned(),
                signature: "fn main()".to_owned(),
                start_line: 1,
                end_line: 5,
            }],
            calls: vec![CallEdge {
                caller: "main".to_owned(),
                callee: "run".to_owned(),
                line: 3,
            }],
            source: "fn main() { run(); }".to_owned(),
        };
        index.update_file(&main).unwrap();

        // Lookup should find the symbol
        let hits = index.symbol("run", 5).unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].path, "src/lib.rs");
        assert_eq!(hits[0].callers.len(), 1);
        assert_eq!(hits[0].callers[0].caller, "main");
    }

    #[test]
    fn file_deletion_removes_from_index() {
        let mut index = SymbolIndex::in_memory().unwrap();

        let file = TestFileContext {
            path: "src/lib.rs".to_owned(),
            language: "rust".to_owned(),
            symbols: vec![IndexedSymbol {
                name: "run".to_owned(),
                kind: "function".to_owned(),
                signature: "fn run()".to_owned(),
                start_line: 1,
                end_line: 5,
            }],
            calls: vec![],
            source: "pub fn run() {}".to_owned(),
        };
        index.update_file(&file).unwrap();

        assert!(index.remove_file("src/lib.rs").unwrap());
        assert!(!index.remove_file("src/lib.rs").unwrap()); // Already removed

        let hits = index.symbol("run", 5).unwrap();
        assert!(hits.is_empty());
    }
}