//! Fast Context: an in-memory code search index that answers "where is the
//! code for X?" in milliseconds.
//!
//! The index is built once per workspace from text files on disk (respecting
//! `.gitignore`). Each line is broken into search terms: identifiers are kept
//! whole and also split at `camelCase` and `snake_case` boundaries, so
//! `parseUserToken` matches "user token". A query is ranked by the rarest
//! terms it shares with a small window of lines. Lines that look like
//! definitions, and files whose path matches, rank higher.

use std::collections::{HashMap, HashSet};
use std::fs;
use std::io::Read;
use std::path::Path;
use std::time::{Duration, Instant};

use ignore::WalkBuilder;
use serde::Serialize;

pub const MAX_FILES: usize = 20_000;
pub const MAX_FILE_BYTES: u64 = 512 * 1024;
pub const MAX_TOTAL_BYTES: u64 = 128 * 1024 * 1024;
const WINDOW: usize = 4;
const SNIPPET_BEFORE: usize = 2;
const SNIPPET_AFTER: usize = 6;

/// One ranked place in the code.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ContextHit {
    pub path: String,
    /// 1-based, inclusive.
    pub start_line: usize,
    pub end_line: usize,
    pub score: f32,
    pub snippet: String,
}

struct IndexedFile {
    path: String,
    path_terms: HashSet<String>,
    lines: Vec<String>,
}

pub struct FastContext {
    files: Vec<IndexedFile>,
    /// term → (file, line) occurrences, one per line.
    postings: HashMap<String, Vec<(u32, u32)>>,
    /// term → number of files containing it.
    file_frequency: HashMap<String, u32>,
    build_time: Duration,
    truncated: bool,
}

impl FastContext {
    /// Indexes text files under `root`.
    pub fn build(root: &Path) -> std::io::Result<Self> {
        let started = Instant::now();
        let root = root.canonicalize()?;
        let mut walker = WalkBuilder::new(&root);
        walker
            .hidden(true)
            .git_ignore(true)
            .follow_links(false)
            .filter_entry(|entry| {
                entry.depth() == 0
                    || !matches!(
                        entry.file_name().to_str(),
                        Some(".git" | ".ryoko" | "target" | "node_modules" | "dist" | "build")
                    )
            });
        let mut index = Self {
            files: Vec::new(),
            postings: HashMap::new(),
            file_frequency: HashMap::new(),
            build_time: Duration::ZERO,
            truncated: false,
        };
        let mut total = 0_u64;
        for entry in walker.build().filter_map(Result::ok) {
            if !entry.file_type().is_some_and(|kind| kind.is_file()) {
                continue;
            }
            if index.files.len() >= MAX_FILES || total >= MAX_TOTAL_BYTES {
                index.truncated = true;
                break;
            }
            let Some(text) = read_text(entry.path()) else {
                continue;
            };
            let Ok(relative) = entry.path().strip_prefix(&root) else {
                continue;
            };
            total += text.len() as u64;
            index.add_file(relative.to_string_lossy().replace('\\', "/"), &text);
        }
        index.build_time = started.elapsed();
        Ok(index)
    }

    fn add_file(&mut self, path: String, text: &str) {
        let file_id = self.files.len() as u32;
        let lines: Vec<String> = text.lines().map(str::to_owned).collect();
        let mut seen_in_file = HashSet::new();
        for (line_no, line) in lines.iter().enumerate() {
            let mut seen_in_line = HashSet::new();
            for term in terms(line) {
                if seen_in_line.insert(term.clone()) {
                    self.postings
                        .entry(term.clone())
                        .or_default()
                        .push((file_id, line_no as u32));
                    if seen_in_file.insert(term.clone()) {
                        *self.file_frequency.entry(term).or_default() += 1;
                    }
                }
            }
        }
        self.files.push(IndexedFile {
            path_terms: terms(&path).into_iter().collect(),
            path,
            lines,
        });
    }

    pub fn file_count(&self) -> usize {
        self.files.len()
    }

    pub fn build_time(&self) -> Duration {
        self.build_time
    }

    /// True when the size caps stopped indexing early.
    pub fn truncated(&self) -> bool {
        self.truncated
    }

    /// Returns up to `limit` ranked hits, best first, at most two per file.
    pub fn search(&self, query: &str, limit: usize) -> Vec<ContextHit> {
        let mut query_terms: Vec<String> = Vec::new();
        for term in terms(query) {
            if !query_terms.contains(&term) {
                query_terms.push(term);
            }
        }
        query_terms.retain(|term| self.postings.contains_key(term));
        if query_terms.is_empty() || limit == 0 {
            return Vec::new();
        }
        let total_files = self.files.len().max(1) as f32;
        let idf: Vec<f32> = query_terms
            .iter()
            .map(|term| {
                let df = *self.file_frequency.get(term).unwrap_or(&1) as f32;
                (1.0 + total_files / df).ln()
            })
            .collect();
        let phrase = query.trim().to_lowercase();

        // file → line → bitmask of matched query terms (first 64 terms).
        let mut hits: HashMap<u32, HashMap<u32, u64>> = HashMap::new();
        for (index, term) in query_terms.iter().enumerate().take(64) {
            for &(file, line) in &self.postings[term] {
                *hits.entry(file).or_default().entry(line).or_default() |= 1 << index;
            }
        }

        let mut ranked = Vec::new();
        for (file_id, lines) in hits {
            let file = &self.files[file_id as usize];
            // Prose ranks below code for the same words.
            let prose = [".md", ".txt", ".rst", ".adoc"]
                .iter()
                .any(|ext| file.path.ends_with(ext));
            let kind_weight = if prose { 0.7 } else { 1.0 };
            let path_bonus: f32 = query_terms
                .iter()
                .zip(&idf)
                .filter(|(term, _)| file.path_terms.contains(*term))
                .map(|(_, weight)| weight * 0.6)
                .sum();
            let mut sorted: Vec<(u32, u64)> = lines.into_iter().collect();
            sorted.sort_unstable_by_key(|(line, _)| *line);
            let mut candidates: Vec<(f32, usize, usize)> = Vec::new();
            for (position, &(line, _)) in sorted.iter().enumerate() {
                // Collect the terms matched within ±WINDOW lines of this hit.
                let mut mask = 0_u64;
                let (mut first, mut last) = (line, line);
                for &(other, other_mask) in sorted[..=position].iter().rev() {
                    if line - other > WINDOW as u32 {
                        break;
                    }
                    mask |= other_mask;
                    first = other;
                }
                for &(other, other_mask) in &sorted[position + 1..] {
                    if other - line > WINDOW as u32 {
                        break;
                    }
                    mask |= other_mask;
                    last = other;
                }
                let coverage: f32 = idf
                    .iter()
                    .enumerate()
                    .filter(|(index, _)| *index < 64 && mask & (1 << index) != 0)
                    .map(|(_, weight)| weight)
                    .sum();
                let text = &file.lines[line as usize];
                let mut score = coverage + path_bonus;
                if looks_like_definition(text) {
                    score *= 1.5;
                }
                if phrase.len() > 3 && text.to_lowercase().contains(&phrase) {
                    score *= 1.4;
                }
                candidates.push((score * kind_weight, first as usize, last as usize));
            }
            candidates.sort_by(|a, b| b.0.total_cmp(&a.0));
            let mut kept: Vec<(f32, usize, usize)> = Vec::new();
            for candidate in candidates {
                if kept.len() == 2 {
                    break;
                }
                let overlaps = kept.iter().any(|existing| {
                    candidate.1 <= existing.2 + SNIPPET_AFTER
                        && existing.1 <= candidate.2 + SNIPPET_AFTER
                });
                if !overlaps {
                    kept.push(candidate);
                }
            }
            for (score, first, last) in kept {
                ranked.push((score, file_id, first, last));
            }
        }
        ranked.sort_by(|a, b| {
            b.0.total_cmp(&a.0).then_with(|| {
                self.files[a.1 as usize]
                    .path
                    .cmp(&self.files[b.1 as usize].path)
            })
        });
        ranked
            .into_iter()
            .take(limit)
            .map(|(score, file_id, first, last)| {
                let file = &self.files[file_id as usize];
                let start = first.saturating_sub(SNIPPET_BEFORE);
                let end = (last + SNIPPET_AFTER).min(file.lines.len().saturating_sub(1));
                let snippet = file.lines[start..=end]
                    .iter()
                    .map(|line| line.chars().take(240).collect::<String>())
                    .collect::<Vec<_>>()
                    .join("\n");
                ContextHit {
                    path: file.path.clone(),
                    start_line: start + 1,
                    end_line: end + 1,
                    score,
                    snippet,
                }
            })
            .collect()
    }
}

/// Formats hits for a model prompt.
pub fn format_hits(hits: &[ContextHit]) -> String {
    let mut out = String::new();
    for hit in hits {
        out.push_str(&format!(
            "{}:{}-{}\n```\n{}\n```\n",
            hit.path, hit.start_line, hit.end_line, hit.snippet
        ));
    }
    out
}

fn read_text(path: &Path) -> Option<String> {
    let metadata = fs::metadata(path).ok()?;
    if metadata.len() > MAX_FILE_BYTES {
        return None;
    }
    let mut bytes = Vec::with_capacity(metadata.len() as usize);
    fs::File::open(path)
        .ok()?
        .take(MAX_FILE_BYTES)
        .read_to_end(&mut bytes)
        .ok()?;
    // Skip binaries.
    if bytes.iter().take(8192).any(|byte| *byte == 0) {
        return None;
    }
    String::from_utf8(bytes).ok()
}

const STOPWORDS: &[&str] = &[
    "the", "and", "for", "with", "this", "that", "from", "into", "are", "was", "where", "what",
    "how", "does", "is", "in", "of", "to", "a", "an", "it", "be", "on", "or", "as", "by", "at",
    "do", "if", "find", "code", "show", "me",
];

/// Splits text into lowercase search terms, including identifier parts.
pub fn terms(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    for token in text.split(|ch: char| !(ch.is_alphanumeric() || ch == '_')) {
        if token.is_empty() || token.len() > 64 {
            continue;
        }
        let whole = token.to_lowercase();
        let mut parts = Vec::new();
        for chunk in token.split('_').filter(|chunk| !chunk.is_empty()) {
            let mut current = String::new();
            let chars: Vec<char> = chunk.chars().collect();
            for (index, &ch) in chars.iter().enumerate() {
                let boundary = index > 0
                    && ch.is_uppercase()
                    && (chars[index - 1].is_lowercase()
                        || chars.get(index + 1).is_some_and(|next| next.is_lowercase()));
                if boundary && !current.is_empty() {
                    parts.push(std::mem::take(&mut current).to_lowercase());
                }
                current.push(ch);
            }
            if !current.is_empty() {
                parts.push(current.to_lowercase());
            }
        }
        for term in std::iter::once(whole).chain(parts) {
            if term.chars().count() >= 2
                && !term.chars().all(|ch| ch.is_ascii_digit())
                && !STOPWORDS.contains(&term.as_str())
                && !out.contains(&term)
            {
                out.push(term);
            }
        }
    }
    out
}

fn looks_like_definition(line: &str) -> bool {
    let trimmed = line.trim_start();
    const PREFIXES: &[&str] = &[
        "fn ",
        "pub fn ",
        "pub(crate) fn ",
        "async fn ",
        "pub async fn ",
        "struct ",
        "pub struct ",
        "enum ",
        "pub enum ",
        "trait ",
        "pub trait ",
        "impl ",
        "type ",
        "pub type ",
        "mod ",
        "def ",
        "class ",
        "async def ",
        "function ",
        "export function ",
        "export async function ",
        "export class ",
        "export interface ",
        "interface ",
        "export type ",
        "export const ",
        "func ",
        "public class ",
        "public interface ",
        "private ",
        "public ",
        "protected ",
        "const ",
        "let ",
        "var ",
    ];
    PREFIXES.iter().any(|prefix| trimmed.starts_with(prefix))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn workspace() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        let write = |path: &str, text: &str| {
            let full = dir.path().join(path);
            fs::create_dir_all(full.parent().unwrap()).unwrap();
            fs::write(full, text).unwrap();
        };
        write(
            "src/auth/token.rs",
            "use crate::db;\n\n/// Parses a bearer header.\npub fn parse_user_token(header: &str) -> Option<Token> {\n    header.strip_prefix(\"Bearer \").map(Token::new)\n}\n",
        );
        write(
            "src/ui/button.ts",
            "export function renderButton(label: string) {\n  return `<button>${label}</button>`;\n}\n",
        );
        write(
            "src/notes.md",
            "We should look at the user token flow later. Token token token.\n",
        );
        write("node_modules/x/token.js", "parseUserToken()\n");
        write("assets/logo.bin", "\0\0binary");
        dir
    }

    #[test]
    fn splits_identifiers_into_terms() {
        let found = terms("parseUserToken HTTPServer snake_case_name");
        for expected in [
            "parseusertoken",
            "parse",
            "user",
            "token",
            "httpserver",
            "http",
            "server",
            "snake",
            "case",
            "name",
        ] {
            assert!(
                found.contains(&expected.to_owned()),
                "{expected} in {found:?}"
            );
        }
    }

    #[test]
    fn ranks_definitions_and_matching_paths_first() {
        let dir = workspace();
        let index = FastContext::build(dir.path()).unwrap();
        assert_eq!(index.file_count(), 3, "skips node_modules and binaries");
        let hits = index.search("where do we parse the user token", 5);
        assert_eq!(hits[0].path, "src/auth/token.rs");
        assert!(hits[0].start_line <= 4 && hits[0].end_line >= 4);
        assert!(hits[0].snippet.contains("pub fn parse_user_token"));
        assert!(
            index.search("renderButton", 5)[0]
                .path
                .ends_with("button.ts")
        );
        assert!(index.search("zzzz nothing", 5).is_empty());
    }

    #[test]
    fn searches_a_large_index_quickly() {
        let dir = tempfile::tempdir().unwrap();
        for index in 0..800 {
            let body: String = (0..60)
                .map(|line| {
                    format!(
                        "fn handler_{index}_{line}(request: Request) -> Response {{ todo!() }}\n"
                    )
                })
                .collect();
            fs::write(dir.path().join(format!("f{index}.rs")), body).unwrap();
        }
        fs::write(
            dir.path().join("special.rs"),
            "pub fn reconcile_ledger_entries() {}\n",
        )
        .unwrap();
        let index = FastContext::build(dir.path()).unwrap();
        let started = Instant::now();
        let hits = index.search("reconcile ledger entries", 10);
        let elapsed = started.elapsed();
        assert_eq!(hits[0].path, "special.rs");
        assert!(
            elapsed < Duration::from_millis(200),
            "search took {elapsed:?}"
        );
    }
}
