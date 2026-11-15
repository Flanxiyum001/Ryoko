//! Structural context extraction for agent prompts.
//!
//! Reading whole files is expensive and noisy. This module parses a source file
//! with tree-sitter, exposes a compact symbol outline, records the call edges
//! inside the file, and can return only the imports plus the requested symbol
//! bodies. Rust, TypeScript/TSX, JavaScript, Python, Go and C/C++ are supported;
//! the language is chosen from the file extension.

use std::collections::BTreeMap;
use std::path::Path;

use serde::{Deserialize, Serialize};
use tree_sitter::{Language as TreeSitterLanguage, Node, Parser};

/// Source language of a context extraction.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Language {
    Rust,
    TypeScript,
    Tsx,
    JavaScript,
    Python,
    Go,
    C,
    Cpp,
}

impl Language {
    /// Detects the language from a path's extension. Unknown extensions return
    /// `None` instead of silently parsing the file as Rust.
    pub fn from_path(path: &str) -> Option<Self> {
        let extension = Path::new(path).extension()?.to_str()?.to_ascii_lowercase();
        match extension.as_str() {
            "rs" => Some(Self::Rust),
            "ts" | "mts" | "cts" => Some(Self::TypeScript),
            "tsx" => Some(Self::Tsx),
            "js" | "mjs" | "cjs" | "jsx" => Some(Self::JavaScript),
            "py" | "pyi" => Some(Self::Python),
            "go" => Some(Self::Go),
            "c" | "h" => Some(Self::C),
            "cc" | "cpp" | "cxx" | "hpp" | "hh" | "hxx" => Some(Self::Cpp),
            _ => None,
        }
    }

    /// Stable identifier used in [`FileContext::language`].
    pub fn name(self) -> &'static str {
        match self {
            Self::Rust => "rust",
            Self::TypeScript => "typescript",
            Self::Tsx => "tsx",
            Self::JavaScript => "javascript",
            Self::Python => "python",
            Self::Go => "go",
            Self::C => "c",
            Self::Cpp => "cpp",
        }
    }

    fn grammar(self) -> TreeSitterLanguage {
        match self {
            Self::Rust => tree_sitter_rust::LANGUAGE.into(),
            Self::TypeScript => tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into(),
            Self::Tsx => tree_sitter_typescript::LANGUAGE_TSX.into(),
            Self::JavaScript => tree_sitter_javascript::LANGUAGE.into(),
            Self::Python => tree_sitter_python::LANGUAGE.into(),
            Self::Go => tree_sitter_go::LANGUAGE.into(),
            Self::C => tree_sitter_c::LANGUAGE.into(),
            Self::Cpp => tree_sitter_cpp::LANGUAGE.into(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Symbol {
    pub name: String,
    pub kind: String,
    pub signature: String,
    pub start_line: usize,
    pub end_line: usize,
}

/// A call from one symbol in the file to a name it references.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CallEdge {
    /// Enclosing function or method, by declared name.
    pub caller: String,
    /// Best-effort callee name (the last segment of `object.method()`).
    pub callee: String,
    pub line: usize,
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
    /// Intra-file call edges, oldest first.
    #[serde(default)]
    pub calls: Vec<CallEdge>,
    pub snippets: Vec<ContextSnippet>,
}

impl FileContext {
    /// Calls made from `symbol` in this file.
    pub fn callees_of<'a>(&'a self, symbol: &'a str) -> impl Iterator<Item = &'a CallEdge> + 'a {
        self.calls.iter().filter(move |call| call.caller == symbol)
    }

    /// Calls in this file whose target name is `symbol`.
    pub fn callers_of<'a>(&'a self, symbol: &'a str) -> impl Iterator<Item = &'a CallEdge> + 'a {
        self.calls.iter().filter(move |call| call.callee == symbol)
    }

    /// Compact, prompt-ready representation: imports, symbols and call edges,
    /// never symbol bodies.
    pub fn render_outline(&self) -> String {
        let mut rendered = format!("### {} ({})\n", self.path, self.language);
        if !self.imports.is_empty() {
            let imports = self
                .imports
                .iter()
                .map(|import| import.split_whitespace().collect::<Vec<_>>().join(" "))
                .collect::<Vec<_>>()
                .join("; ");
            rendered.push_str("imports: ");
            rendered.push_str(&imports);
            rendered.push('\n');
        }
        for symbol in &self.symbols {
            rendered.push_str(&format!(
                "- {} {} (L{}-L{})\n",
                symbol.kind, symbol.name, symbol.start_line, symbol.end_line
            ));
        }
        if !self.calls.is_empty() {
            let mut by_caller: BTreeMap<&str, Vec<&str>> = BTreeMap::new();
            for call in &self.calls {
                by_caller
                    .entry(call.caller.as_str())
                    .or_default()
                    .push(call.callee.as_str());
            }
            let groups = by_caller
                .iter()
                .map(|(caller, callees)| format!("{caller} -> {}", callees.join(", ")))
                .collect::<Vec<_>>()
                .join("; ");
            rendered.push_str("calls: ");
            rendered.push_str(&groups);
            rendered.push('\n');
        }
        rendered
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ContextError {
    #[error("no tree-sitter grammar is configured for {0}")]
    UnsupportedLanguage(String),
    #[error("could not load the tree-sitter grammar: {0}")]
    Language(String),
    #[error("tree-sitter could not parse the source")]
    Parse,
}

/// Parses source into an outline and optional focused snippets.
pub struct ContextExtractor {
    parser: Parser,
    max_snippet_bytes: usize,
}

impl ContextExtractor {
    pub fn new() -> Result<Self, ContextError> {
        Ok(Self {
            parser: Parser::new(),
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
        let language = Language::from_path(&path)
            .ok_or_else(|| ContextError::UnsupportedLanguage(path.clone()))?;
        self.parser
            .set_language(&language.grammar())
            .map_err(|error| ContextError::Language(format!("{}: {error}", language.name())))?;
        let tree = self.parser.parse(source, None).ok_or(ContextError::Parse)?;
        let mut context = FileContext {
            path,
            language: language.name().to_owned(),
            has_syntax_errors: tree.root_node().has_error(),
            imports: Vec::new(),
            symbols: Vec::new(),
            calls: Vec::new(),
            snippets: Vec::new(),
        };
        visit(
            tree.root_node(),
            source,
            language,
            requested,
            self.max_snippet_bytes,
            &mut context,
            &mut Vec::new(),
        );
        Ok(context)
    }
}

#[allow(clippy::too_many_arguments)]
fn visit(
    node: Node<'_>,
    source: &str,
    language: Language,
    requested: &[&str],
    max_snippet_bytes: usize,
    context: &mut FileContext,
    enclosing: &mut Vec<String>,
) {
    if is_import(language, node.kind()) {
        let import = source[node.byte_range()].trim();
        if !import.is_empty() {
            context.imports.push(import.to_owned());
        }
    }

    let pushed_enclosing = if let Some((kind, name)) = symbol_for(node, source, language) {
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
                symbol: Some(name.clone()),
                text,
                start_line: node.start_position().row + 1,
                end_line: node.end_position().row + 1,
                truncated,
            });
        }
        let callable = kind == "function" || kind == "method";
        if callable {
            enclosing.push(name);
        }
        callable
    } else {
        false
    };

    if is_call(language, node.kind()) {
        if let Some(caller) = enclosing.last() {
            if let Some(callee) = call_target(node, source) {
                context.calls.push(CallEdge {
                    caller: caller.clone(),
                    callee,
                    line: node.start_position().row + 1,
                });
            }
        }
    }

    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        visit(
            child,
            source,
            language,
            requested,
            max_snippet_bytes,
            context,
            enclosing,
        );
    }
    if pushed_enclosing {
        enclosing.pop();
    }
}

fn is_import(language: Language, kind: &str) -> bool {
    match language {
        Language::Rust => kind == "use_declaration",
        Language::TypeScript | Language::Tsx | Language::JavaScript => kind == "import_statement",
        Language::Python => kind == "import_statement" || kind == "import_from_statement",
        Language::Go => kind == "import_declaration",
        Language::C | Language::Cpp => kind == "preproc_include",
    }
}

fn is_call(language: Language, kind: &str) -> bool {
    match language {
        Language::Rust | Language::Go | Language::C | Language::Cpp => kind == "call_expression",
        Language::TypeScript | Language::Tsx | Language::JavaScript => {
            kind == "call_expression" || kind == "new_expression"
        }
        Language::Python => kind == "call",
    }
}

/// Maps an AST node to a symbol kind and declared name. Language-specific node
/// names are kept here so the rest of the traversal stays generic.
fn symbol_for(node: Node<'_>, source: &str, language: Language) -> Option<(&'static str, String)> {
    if language == Language::Go && node.kind() == "type_declaration" {
        let mut cursor = node.walk();
        let spec = node
            .named_children(&mut cursor)
            .find(|child| child.kind() == "type_spec")?;
        let name = spec
            .child_by_field_name("name")
            .map(|name| text(name, source))?;
        if name.is_empty() {
            return None;
        }
        let kind = match spec.child_by_field_name("type").map(|ty| ty.kind()) {
            Some("struct_type") => "struct",
            Some("interface_type") => "interface",
            _ => "type",
        };
        return Some((kind, name));
    }

    let kind = match (language, node.kind()) {
        (Language::Rust, "function_item") => "function",
        (Language::Rust, "struct_item") => "struct",
        (Language::Rust, "enum_item") => "enum",
        (Language::Rust, "trait_item") => "trait",
        (Language::Rust, "impl_item") => "impl",
        (Language::Rust, "mod_item") => "module",
        (Language::Rust, "const_item") => "constant",
        (Language::Rust, "static_item") => "static",
        (Language::Rust, "type_item") => "type",
        (Language::Rust, "macro_definition") => "macro",

        (Language::TypeScript | Language::Tsx | Language::JavaScript, "function_declaration") => {
            "function"
        }
        (Language::TypeScript | Language::Tsx | Language::JavaScript, "class_declaration") => {
            "class"
        }
        (Language::TypeScript | Language::Tsx | Language::JavaScript, "method_definition") => {
            "method"
        }
        (Language::TypeScript | Language::Tsx, "interface_declaration") => "interface",
        (Language::TypeScript | Language::Tsx, "type_alias_declaration") => "type",
        (Language::TypeScript | Language::Tsx | Language::JavaScript, "enum_declaration") => "enum",

        (Language::Python, "function_definition") => {
            if python_method(node) {
                "method"
            } else {
                "function"
            }
        }
        (Language::Python, "class_definition") => "class",

        (Language::Go, "function_declaration") => "function",
        (Language::Go, "method_declaration") => "method",

        (Language::C, "function_definition") => "function",
        (Language::C, "struct_specifier") => "struct",
        (Language::C, "enum_specifier") => "enum",
        (Language::C, "union_specifier") => "union",
        (Language::C, "type_definition") => "type",

        (Language::Cpp, "function_definition") => "function",
        (Language::Cpp, "class_specifier") => "class",
        (Language::Cpp, "struct_specifier") => "struct",
        (Language::Cpp, "enum_specifier") => "enum",
        (Language::Cpp, "union_specifier") => "union",
        (Language::Cpp, "namespace_definition") => "namespace",
        (Language::Cpp, "type_definition") => "type",
        _ => {
            let name = function_variable_name(node, source, language)?;
            return Some(("function", name));
        }
    };

    let name = symbol_name(node, source)?;
    if name.is_empty() {
        None
    } else {
        Some((kind, name))
    }
}

/// Recognizes `const handler = () => {}` and
/// `const handler = function () {}` as function symbols.
fn function_variable_name(node: Node<'_>, source: &str, language: Language) -> Option<String> {
    if !matches!(
        language,
        Language::TypeScript | Language::Tsx | Language::JavaScript
    ) || node.kind() != "variable_declarator"
    {
        return None;
    }
    let value = node.child_by_field_name("value")?;
    if !matches!(
        value.kind(),
        "arrow_function" | "function_expression" | "generator_function"
    ) {
        return None;
    }
    let name = node
        .child_by_field_name("name")
        .map(|name| text(name, source))?;
    if name.is_empty() { None } else { Some(name) }
}

/// A Python method is a `function_definition` nested in a class body rather
/// than in another function.
fn python_method(node: Node<'_>) -> bool {
    let mut parent = node.parent();
    while let Some(current) = parent {
        match current.kind() {
            "class_definition" => return true,
            "function_definition" | "lambda" => return false,
            _ => parent = current.parent(),
        }
    }
    false
}

fn symbol_name(node: Node<'_>, source: &str) -> Option<String> {
    if let Some(name) = node.child_by_field_name("name") {
        let candidate = text(name, source);
        if !candidate.is_empty() {
            return Some(candidate);
        }
    }
    if let Some(declarator) = node.child_by_field_name("declarator") {
        if let Some(name) = declarator_name(declarator, source) {
            return Some(name);
        }
    }
    if node.kind() == "impl_item" {
        if let Some(name) = node.child_by_field_name("type") {
            let candidate = text(name, source);
            if !candidate.is_empty() {
                return Some(candidate);
            }
        }
    }
    None
}

/// C and C++ put the declared name inside nested declarators
/// (`function_declarator`, `pointer_declarator`, `qualified_identifier`, ...).
fn declarator_name(node: Node<'_>, source: &str) -> Option<String> {
    if is_identifier(node.kind()) {
        let candidate = text(node, source);
        return if candidate.is_empty() {
            None
        } else {
            Some(candidate)
        };
    }
    if let Some(inner) = node.child_by_field_name("declarator") {
        if let Some(name) = declarator_name(inner, source) {
            return Some(name);
        }
    }
    if let Some(inner) = node.child_by_field_name("name") {
        if let Some(name) = declarator_name(inner, source) {
            return Some(name);
        }
    }
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        if let Some(name) = declarator_name(child, source) {
            return Some(name);
        }
    }
    None
}

fn is_identifier(kind: &str) -> bool {
    matches!(
        kind,
        "identifier"
            | "field_identifier"
            | "type_identifier"
            | "property_identifier"
            | "namespace_identifier"
            | "operator_name"
            | "destructor_name"
    )
}

fn call_target(node: Node<'_>, source: &str) -> Option<String> {
    let function = node
        .child_by_field_name("function")
        .or_else(|| node.child_by_field_name("constructor"))?;
    callee_name(function, source)
}

/// Returns the last name segment of a call target: `read_file`, `store.save`,
/// `fmt.Println`, `obj.method`, `Path.read_text`.
fn callee_name(node: Node<'_>, source: &str) -> Option<String> {
    if is_identifier(node.kind()) {
        let candidate = text(node, source);
        return if candidate.is_empty() {
            None
        } else {
            Some(candidate)
        };
    }
    for field in ["property", "field", "attribute", "name"] {
        if let Some(child) = node.child_by_field_name(field) {
            if let Some(name) = callee_name(child, source) {
                return Some(name);
            }
        }
    }
    let mut cursor = node.walk();
    let mut last = None;
    for child in node.named_children(&mut cursor) {
        if let Some(name) = callee_name(child, source) {
            last = Some(name);
        }
    }
    last
}

fn text(node: Node<'_>, source: &str) -> String {
    source[node.byte_range()].trim().to_owned()
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

    const RUST_SOURCE: &str = r#"
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

    const TYPESCRIPT_SOURCE: &str = r#"
import { readFile } from "node:fs";

export function loadConfig(path: string): string {
  return readFile(path, "utf8");
}

export class Store {
  save(value: string) {
    this.write(value);
  }

  private write(value: string) {
    console.log(value);
  }
}
"#;

    const TSX_SOURCE: &str = r#"
import { useState } from "react";

type Props = { label: string };

export function Greeting({ label }: Props) {
  return <h1>Hello {label}</h1>;
}

export const Button = ({ label }: Props) => (
  <button onClick={() => save(label)}>{label}</button>
);
"#;

    const JAVASCRIPT_SOURCE: &str = r#"
import { format } from "./format.js";

const greet = (name) => format(name);

function run() {
  greet("ryoko");
}

module.exports = { run };
"#;

    const PYTHON_SOURCE: &str = r#"
import os
from pathlib import Path


class Store:
    def save(self, value):
        self.write(value)

    def write(self, value):
        print(value)


def load(path):
    return Path(path).read_text()
"#;

    const GO_SOURCE: &str = r#"
package demo

import "fmt"

type Store struct {
	name string
}

func (s *Store) Save(value string) {
	fmt.Println(value)
}

func Load(path string) string {
	return readFile(path)
}
"#;

    const C_SOURCE: &str = r#"
#include <stdio.h>

struct Point {
    int x;
    int y;
};

static int add(int a, int b) {
    return a + b;
}

int main(void) {
    printf("%d\n", add(1, 2));
    return 0;
}
"#;

    const CPP_SOURCE: &str = r#"
#include <vector>

namespace demo {

class Store {
public:
    void save(int value) {
        write(value);
    }

    void write(int value) {}
};

int load() {
    Store store;
    store.save(1);
    return 0;
}

}
"#;

    fn has_symbol(context: &FileContext, name: &str, kind: &str) -> bool {
        context
            .symbols
            .iter()
            .any(|symbol| symbol.name == name && symbol.kind == kind)
    }

    fn has_call(context: &FileContext, caller: &str, callee: &str) -> bool {
        context
            .calls
            .iter()
            .any(|call| call.caller == caller && call.callee == callee)
    }

    #[test]
    fn extracts_imports_and_symbol_outline() {
        let mut extractor = ContextExtractor::new().unwrap();
        let context = extractor.extract("src/engine.rs", RUST_SOURCE).unwrap();
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
        assert_eq!(context.language, "rust");
    }

    #[test]
    fn focused_context_keeps_imports_and_requested_bodies_only() {
        let mut extractor = ContextExtractor::new().unwrap();
        let context = extractor
            .extract_symbols("src/engine.rs", RUST_SOURCE, &["run"])
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

    #[test]
    fn maps_extensions_to_languages() {
        assert_eq!(Language::from_path("a.rs"), Some(Language::Rust));
        assert_eq!(Language::from_path("a.ts"), Some(Language::TypeScript));
        assert_eq!(Language::from_path("a.tsx"), Some(Language::Tsx));
        assert_eq!(Language::from_path("a.js"), Some(Language::JavaScript));
        assert_eq!(Language::from_path("a.py"), Some(Language::Python));
        assert_eq!(Language::from_path("a.go"), Some(Language::Go));
        assert_eq!(Language::from_path("a.c"), Some(Language::C));
        assert_eq!(Language::from_path("a.hpp"), Some(Language::Cpp));
        assert_eq!(Language::from_path("a.txt"), None);
    }

    #[test]
    fn rejects_unknown_extensions_instead_of_parsing_as_rust() {
        let mut extractor = ContextExtractor::new().unwrap();
        let error = extractor.extract("notes.txt", "hello").unwrap_err();
        assert!(
            matches!(error, ContextError::UnsupportedLanguage(path) if path.contains("notes.txt"))
        );
    }

    #[test]
    fn extracts_typescript_outline_calls_and_body() {
        let mut extractor = ContextExtractor::new().unwrap();
        let context = extractor
            .extract("src/store.ts", TYPESCRIPT_SOURCE)
            .unwrap();
        assert_eq!(context.language, "typescript");
        assert!(!context.has_syntax_errors);
        assert!(context.imports.iter().any(|line| line.contains("node:fs")));
        assert!(has_symbol(&context, "loadConfig", "function"));
        assert!(has_symbol(&context, "Store", "class"));
        assert!(has_symbol(&context, "save", "method"));
        assert!(has_symbol(&context, "write", "method"));
        assert!(has_call(&context, "loadConfig", "readFile"));
        assert!(has_call(&context, "save", "write"));
        assert!(
            context
                .callers_of("write")
                .any(|call| call.caller == "save")
        );
        assert!(
            context
                .callees_of("save")
                .any(|call| call.callee == "write")
        );

        let focused = extractor
            .extract_symbols("src/store.ts", TYPESCRIPT_SOURCE, &["save"])
            .unwrap();
        assert_eq!(focused.snippets.len(), 1);
        assert!(focused.snippets[0].text.contains("this.write(value)"));
        assert!(!focused.snippets[0].text.contains("loadConfig"));
    }

    #[test]
    fn extracts_tsx_jsx_components_and_calls() {
        let mut extractor = ContextExtractor::new().unwrap();
        let context = extractor.extract("src/App.tsx", TSX_SOURCE).unwrap();
        assert_eq!(context.language, "tsx");
        assert!(!context.has_syntax_errors);
        assert!(context.imports.iter().any(|line| line.contains("react")));
        assert!(has_symbol(&context, "Props", "type"));
        assert!(has_symbol(&context, "Greeting", "function"));
        assert!(has_symbol(&context, "Button", "function"));
        assert!(has_call(&context, "Button", "save"));

        let focused = extractor
            .extract_symbols("src/App.tsx", TSX_SOURCE, &["Greeting"])
            .unwrap();
        assert_eq!(focused.snippets.len(), 1);
        assert!(focused.snippets[0].text.contains("<h1>"));
    }

    #[test]
    fn extracts_javascript_arrow_functions() {
        let mut extractor = ContextExtractor::new().unwrap();
        let context = extractor
            .extract("src/entry.js", JAVASCRIPT_SOURCE)
            .unwrap();
        assert_eq!(context.language, "javascript");
        assert!(
            context
                .imports
                .iter()
                .any(|line| line.contains("./format.js"))
        );
        assert!(has_symbol(&context, "greet", "function"));
        assert!(has_symbol(&context, "run", "function"));
        assert!(has_call(&context, "greet", "format"));
        assert!(has_call(&context, "run", "greet"));
    }

    #[test]
    fn extracts_python_classes_methods_and_calls() {
        let mut extractor = ContextExtractor::new().unwrap();
        let context = extractor.extract("src/store.py", PYTHON_SOURCE).unwrap();
        assert_eq!(context.language, "python");
        assert!(context.imports.iter().any(|line| line.contains("pathlib")));
        assert!(has_symbol(&context, "Store", "class"));
        assert!(has_symbol(&context, "save", "method"));
        assert!(has_symbol(&context, "write", "method"));
        assert!(has_symbol(&context, "load", "function"));
        assert!(has_call(&context, "save", "write"));
        assert!(has_call(&context, "load", "read_text"));
    }

    #[test]
    fn extracts_go_types_methods_and_calls() {
        let mut extractor = ContextExtractor::new().unwrap();
        let context = extractor.extract("store.go", GO_SOURCE).unwrap();
        assert_eq!(context.language, "go");
        assert!(context.imports.iter().any(|line| line.contains("fmt")));
        assert!(has_symbol(&context, "Store", "struct"));
        assert!(has_symbol(&context, "Save", "method"));
        assert!(has_symbol(&context, "Load", "function"));
        assert!(has_call(&context, "Save", "Println"));
        assert!(has_call(&context, "Load", "readFile"));
    }

    #[test]
    fn extracts_c_structs_functions_and_calls() {
        let mut extractor = ContextExtractor::new().unwrap();
        let context = extractor.extract("src/main.c", C_SOURCE).unwrap();
        assert_eq!(context.language, "c");
        assert!(context.imports.iter().any(|line| line.contains("stdio.h")));
        assert!(has_symbol(&context, "Point", "struct"));
        assert!(has_symbol(&context, "add", "function"));
        assert!(has_symbol(&context, "main", "function"));
        assert!(has_call(&context, "main", "printf"));
        assert!(has_call(&context, "main", "add"));
    }

    #[test]
    fn extracts_cpp_classes_namespaces_and_calls() {
        let mut extractor = ContextExtractor::new().unwrap();
        let context = extractor.extract("src/store.cpp", CPP_SOURCE).unwrap();
        assert_eq!(context.language, "cpp");
        assert!(context.imports.iter().any(|line| line.contains("vector")));
        assert!(has_symbol(&context, "demo", "namespace"));
        assert!(has_symbol(&context, "Store", "class"));
        assert!(has_symbol(&context, "save", "function"));
        assert!(has_symbol(&context, "write", "function"));
        assert!(has_symbol(&context, "load", "function"));
        assert!(has_call(&context, "save", "write"));
        assert!(has_call(&context, "load", "save"));
    }
}
