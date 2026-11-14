//! Virtual document storage for editor integrations.
//!
//! The language server owns the canonical open-document state here, not in the
//! client. That keeps incremental edits, snapshots and future virtual files
//! independent of any particular editor.

use std::collections::BTreeMap;
use std::sync::RwLock;

/// Zero-based line and UTF-16 character position, matching LSP positions.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct Position {
    pub line: u32,
    pub character: u32,
}

impl Position {
    pub const fn new(line: u32, character: u32) -> Self {
        Self { line, character }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Range {
    pub start: Position,
    pub end: Position,
}

impl Range {
    pub const fn new(start: Position, end: Position) -> Self {
        Self { start, end }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DocumentChange {
    /// `None` replaces the entire document.
    pub range: Option<Range>,
    pub text: String,
}

impl DocumentChange {
    pub fn full(text: impl Into<String>) -> Self {
        Self {
            range: None,
            text: text.into(),
        }
    }

    pub fn incremental(range: Range, text: impl Into<String>) -> Self {
        Self {
            range: Some(range),
            text: text.into(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DocumentSnapshot {
    pub uri: String,
    pub language_id: String,
    pub version: i32,
    pub text: String,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum DocumentError {
    #[error("document is not open: {uri}")]
    UnknownDocument { uri: String },
    #[error("invalid document range: {0}")]
    InvalidRange(String),
}

#[derive(Default)]
pub struct DocumentStore {
    documents: RwLock<BTreeMap<String, DocumentSnapshot>>,
}

impl std::fmt::Debug for DocumentStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let count = self
            .documents
            .read()
            .map(|documents| documents.len())
            .unwrap_or(0);
        f.debug_struct("DocumentStore")
            .field("documents", &count)
            .finish()
    }
}

impl DocumentStore {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn open(
        &self,
        uri: impl Into<String>,
        language_id: impl Into<String>,
        version: i32,
        text: impl Into<String>,
    ) -> DocumentSnapshot {
        let snapshot = DocumentSnapshot {
            uri: uri.into(),
            language_id: language_id.into(),
            version,
            text: text.into(),
        };
        self.documents
            .write()
            .unwrap_or_else(|error| error.into_inner())
            .insert(snapshot.uri.clone(), snapshot.clone());
        snapshot
    }

    pub fn change(
        &self,
        uri: &str,
        version: i32,
        changes: &[DocumentChange],
    ) -> Result<DocumentSnapshot, DocumentError> {
        let mut documents = self
            .documents
            .write()
            .unwrap_or_else(|error| error.into_inner());
        let document = documents
            .get_mut(uri)
            .ok_or_else(|| DocumentError::UnknownDocument {
                uri: uri.to_owned(),
            })?;

        for change in changes {
            match change.range {
                None => document.text.clone_from(&change.text),
                Some(range) => apply_incremental(&mut document.text, range, &change.text)?,
            }
        }
        document.version = version;
        Ok(document.clone())
    }

    pub fn close(&self, uri: &str) -> Option<DocumentSnapshot> {
        self.documents
            .write()
            .unwrap_or_else(|error| error.into_inner())
            .remove(uri)
    }

    pub fn get(&self, uri: &str) -> Option<DocumentSnapshot> {
        self.documents
            .read()
            .unwrap_or_else(|error| error.into_inner())
            .get(uri)
            .cloned()
    }

    pub fn snapshots(&self) -> Vec<DocumentSnapshot> {
        self.documents
            .read()
            .unwrap_or_else(|error| error.into_inner())
            .values()
            .cloned()
            .collect()
    }

    pub fn len(&self) -> usize {
        self.documents
            .read()
            .unwrap_or_else(|error| error.into_inner())
            .len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

fn apply_incremental(
    text: &mut String,
    range: Range,
    replacement: &str,
) -> Result<(), DocumentError> {
    let start = byte_offset(text, range.start)?;
    let end = byte_offset(text, range.end)?;
    if start > end {
        return Err(DocumentError::InvalidRange(
            "range start is after range end".to_owned(),
        ));
    }
    text.replace_range(start..end, replacement);
    Ok(())
}

/// Resolves an LSP position to a UTF-8 byte offset using UTF-16 columns.
fn byte_offset(text: &str, position: Position) -> Result<usize, DocumentError> {
    let mut line_start = 0;
    for _ in 0..position.line {
        let newline = text[line_start..].find('\n').ok_or_else(|| {
            DocumentError::InvalidRange("line is outside the document".to_owned())
        })?;
        line_start += newline + 1;
    }

    let mut line_end = text[line_start..]
        .find('\n')
        .map(|offset| line_start + offset)
        .unwrap_or(text.len());
    if line_end > line_start && text.as_bytes().get(line_end - 1) == Some(&b'\r') {
        line_end -= 1;
    }

    let mut utf16 = 0;
    for (offset, character) in text[line_start..line_end].char_indices() {
        if utf16 == position.character {
            return Ok(line_start + offset);
        }
        utf16 += character.len_utf16() as u32;
        if utf16 > position.character {
            return Err(DocumentError::InvalidRange(
                "character splits a UTF-16 surrogate pair".to_owned(),
            ));
        }
    }
    if utf16 == position.character {
        Ok(line_end)
    } else {
        Err(DocumentError::InvalidRange(
            "character is outside the line".to_owned(),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn applies_full_and_incremental_changes() {
        let store = DocumentStore::new();
        store.open("file:///demo/src/lib.rs", "rust", 1, "fn main() {}\n");
        let snapshot = store
            .change(
                "file:///demo/src/lib.rs",
                2,
                &[DocumentChange::incremental(
                    Range::new(Position::new(0, 11), Position::new(0, 11)),
                    "println!(\"hi\");",
                )],
            )
            .unwrap();
        assert_eq!(snapshot.text, "fn main() {println!(\"hi\");}\n");
        assert_eq!(snapshot.version, 2);

        let snapshot = store
            .change(
                "file:///demo/src/lib.rs",
                3,
                &[DocumentChange::full("fn answer() -> u32 { 42 }\n")],
            )
            .unwrap();
        assert_eq!(snapshot.text, "fn answer() -> u32 { 42 }\n");
        assert_eq!(snapshot.version, 3);
    }

    #[test]
    fn counts_utf16_columns_without_splitting_surrogates() {
        let store = DocumentStore::new();
        store.open("file:///demo.txt", "plaintext", 1, "a😀b");
        let snapshot = store
            .change(
                "file:///demo.txt",
                2,
                &[DocumentChange::incremental(
                    Range::new(Position::new(0, 1), Position::new(0, 3)),
                    "x",
                )],
            )
            .unwrap();
        assert_eq!(snapshot.text, "axb");

        store.open("file:///demo2.txt", "plaintext", 1, "a😀b");
        let error = store
            .change(
                "file:///demo2.txt",
                3,
                &[DocumentChange::incremental(
                    Range::new(Position::new(0, 2), Position::new(0, 3)),
                    "x",
                )],
            )
            .unwrap_err();
        assert!(matches!(error, DocumentError::InvalidRange(_)));
    }

    #[test]
    fn closes_documents_and_reports_unknown_documents() {
        let store = DocumentStore::new();
        store.open("file:///demo.txt", "plaintext", 1, "hello");
        assert_eq!(store.len(), 1);
        assert!(store.close("file:///demo.txt").is_some());
        assert!(store.is_empty());
        assert!(matches!(
            store.change("file:///missing.txt", 2, &[DocumentChange::full("x")]),
            Err(DocumentError::UnknownDocument { .. })
        ));
    }
}
