//! Structural context extraction for agent prompts.
//!
//! Reading whole files is expensive and noisy. This module parses a Rust file
//! with tree-sitter, exposes a compact symbol outline, and can return only the
//! imports plus the requested symbol bodies.

use serde::{Deserialize, Serialize};
use tree_sitter::{Node, Parser};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Symbol {
    pub name: String,
    pub kind: String,
    pub signature: String,
    pub start_line: usize,
    pub end_line: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContextSnippet {
    pub symbol: Option<String>,
    pub text: String,
    pub start_line: usize,
    pub end_line: usize,
    pub truncated: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileContext {
    pub path: String,
    pub language: String,
    pub has_syntax_errors: bool,
    pub imports: Vec<String>,
    pub symbols: Vec<Symbol>,
    pub snippets: Vec<ContextSnippet>,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ContextError {
    #[error("could not load the Rust tree-sitter grammar: {0}")]
    Language(String),
    #[error("tree-sitter could not parse the source")]
    Parse,
}

/// Parses Rust source into an outline and optional focused snippets.
pub struct ContextExtractor {
    parser: Parser,
    max_snippet_bytes: usize,
}

impl ContextExtractor {
    pub fn new() -> Result<Self, ContextError> {
        let mut parser = Parser::new();
        parser
            .set_language(&tree_sitter_rust::LANGUAGE.into())
            .map_err(|error| ContextError::Language(error.to_string()))?;
        Ok(Self {
            parser,
            max_snippet_bytes: 16_000,
        })
    }

    pub fn with_max_snippet_bytes(mut self, max_snippet_bytes: usize) -> Self {
        self.max_snippet_bytes = max_snippet_bytes.max(1);
        self
    }

    /// Returns imports and the full symbol outline without bodies.
    pub fn extract(
        &mut self,
        path: impl Into<String>,
        source: &str,
    ) -> Result<FileContext, ContextError> {
        self.collect(path.into(), source, &[])
    }

    /// Returns imports plus the exact source bodies for requested symbol names.
    /// The outline still describes the rest of the file so an agent can see
    /// what else exists without paying for every body.
    pub fn extract_symbols(
        &mut self,
        path: impl Into<String>,
        source: &str,
        requested: &[&str],
    ) -> Result<FileContext, ContextError> {
        self.collect(path.into(), source, requested)
    }

    fn collect(
        &mut self,
        path: String,
        source: &str,
        requested: &[&str],
    ) -> Result<FileContext, ContextError> {
        let tree = self.parser.parse(source, None).ok_or(ContextError::Parse)?;
        let mut context = FileContext {
            path,
            language: "rust".to_owned(),
            has_syntax_errors: tree.root_node().has_error(),
            imports: Vec::new(),
            symbols: Vec::new(),
            snippets: Vec::new(),
        };
        visit(
            tree.root_node(),
            source,
            requested,
            self.max_snippet_bytes,
            &mut context,
        );
        Ok(context)
    }
}

fn visit(
    node: Node<'_>,
    source: &str,
    requested: &[&str],
    max_snippet_bytes: usize,
    context: &mut FileContext,
) {
    if node.kind() == "use_declaration" {
        let import = source[node.byte_range()].trim();
        if !import.is_empty() {
            context.imports.push(import.to_owned());
        }
    }

    if let Some(kind) = symbol_kind(node.kind()) {
        if let Some(name) = symbol_name(node, source) {
            let signature = signature(node, source);
            context.symbols.push(Symbol {
                name: name.clone(),
                kind: kind.to_owned(),
                signature,
                start_line: node.start_position().row + 1,
                end_line: node.end_position().row + 1,
            });
            if requested.iter().any(|requested| *requested == name) {
                let raw = &source[node.byte_range()];
                let (text, truncated) = truncate(raw, max_snippet_bytes);
                context.snippets.push(ContextSnippet {
                    symbol: Some(name),
                    text,
                    start_line: node.start_position().row + 1,
                    end_line: node.end_position().row + 1,
                    truncated,
                });
            }
        }
    }

    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        visit(child, source, requested, max_snippet_bytes, context);
    }
}

fn symbol_kind(kind: &str) -> Option<&'static str> {
    match kind {
        "function_item" => Some("function"),
        "struct_item" => Some("struct"),
        "enum_item" => Some("enum"),
        "trait_item" => Some("trait"),
        "impl_item" => Some("impl"),
        "mod_item" => Some("module"),
        "const_item" => Some("constant"),
        "static_item" => Some("static"),
        "type_item" => Some("type"),
        "macro_definition" => Some("macro"),
        _ => None,
    }
}

fn symbol_name(node: Node<'_>, source: &str) -> Option<String> {
    node.child_by_field_name("name")
        .or_else(|| node.child_by_field_name("type"))
        .map(|name| source[name.byte_range()].trim().to_owned())
        .filter(|name| !name.is_empty())
}

fn signature(node: Node<'_>, source: &str) -> String {
    let end = node
        .child_by_field_name("body")
        .map(|body| body.start_byte())
        .unwrap_or_else(|| node.end_byte());
    let text = source[node.start_byte()..end].trim();
    let first_line = text.lines().next().unwrap_or(text).trim();
    first_line.trim_end_matches('{').trim().to_owned()
}

fn truncate(text: &str, max_bytes: usize) -> (String, bool) {
    if text.len() <= max_bytes {
        return (text.to_owned(), false);
    }
    let mut end = max_bytes;
    while end > 0 && !text.is_char_boundary(end) {
        end -= 1;
    }
    (format!("{}...", &text[..end]), true)
}

#[cfg(test)]
mod tests {
    use super::*;

    const SOURCE: &str = r#"
use std::collections::HashMap;
use crate::model::Task;

pub struct Engine {
    tasks: HashMap<String, Task>,
}

impl Engine {
    pub fn new() -> Self {
        Self { tasks: HashMap::new() }
    }

    pub fn run(&mut self, goal: &str) -> Result<(), String> {
        let _ = goal;
        Ok(())
    }
}

fn private_helper() {}
"#;

    #[test]
    fn extracts_imports_and_symbol_outline() {
        let mut extractor = ContextExtractor::new().unwrap();
        let context = extractor.extract("src/engine.rs", SOURCE).unwrap();
        assert_eq!(context.imports.len(), 2);
        assert!(context.imports[0].contains("HashMap"));
        assert!(context.symbols.iter().any(|symbol| symbol.name == "Engine"));
        assert!(context.symbols.iter().any(|symbol| symbol.name == "run"));
        assert!(
            context
                .symbols
                .iter()
                .any(|symbol| symbol.name == "private_helper")
        );
        assert!(context.snippets.is_empty());
    }

    #[test]
    fn focused_context_keeps_imports_and_requested_bodies_only() {
        let mut extractor = ContextExtractor::new().unwrap();
        let context = extractor
            .extract_symbols("src/engine.rs", SOURCE, &["run"])
            .unwrap();
        assert_eq!(context.snippets.len(), 1);
        assert!(context.snippets[0].text.contains("pub fn run"));
        assert!(!context.snippets[0].text.contains("private_helper"));
        assert_eq!(context.imports.len(), 2);
    }

    #[test]
    fn malformed_source_is_flagged_but_still_returns_outline() {
        let mut extractor = ContextExtractor::new().unwrap();
        let context = extractor
            .extract_symbols("src/broken.rs", "fn broken( {", &["broken"])
            .unwrap();
        assert!(context.has_syntax_errors);
    }
}
