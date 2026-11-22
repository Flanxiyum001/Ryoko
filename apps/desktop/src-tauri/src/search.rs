//! Workspace-wide text search and bounded, confirmed replace.
//!
//! Search mirrors the explorer's walker rules (`ignore` crate: respects
//! `.gitignore`, skips `.git`, `.ryoko`, `target`, `node_modules`, never
//! follows links) and rejects oversized or non-UTF-8 files. Replace is a
//! two-phase, dry-run-first operation: the plan is built per file, verified at
//! apply time against a re-read of the file, and a drifted file is skipped
//! instead of silently rewritten.

use regex::{Regex, RegexBuilder};

pub const MAX_RESULTS: usize = 1_000;
pub const MAX_HITS_PER_FILE: usize = 200;
const MAX_LINE_CHARS: usize = 240;

#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SearchHit {
    pub path: String,
    pub line: u32,
    /// 0-based UTF-16 code-unit offsets within the preview line, matching
    /// JavaScript strings and Monaco's text coordinate system.
    pub start: usize,
    pub end: usize,
    pub preview: String,
}

pub fn compile_query(query: &str, case_sensitive: bool) -> Result<Regex, String> {
    if query.is_empty() {
        return Err("search query cannot be empty".to_owned());
    }
    if query.len() > 512 {
        return Err("search query must be at most 512 bytes".to_owned());
    }
    RegexBuilder::new(query)
        .case_insensitive(!case_sensitive)
        .size_limit(4 * 1024 * 1024)
        .build()
        .map_err(|error| format!("invalid search query: {error}"))
}

/// Matches inside one file's text: a 1-based line number and 0-based
/// UTF-16 code-unit offsets within that line (for Monaco and DOM highlighting).
pub fn hit_spans(regex: &Regex, text: &str, limit: usize) -> Vec<(u32, usize, usize)> {
    let mut spans = Vec::new();
    for (number, line) in text.lines().enumerate().take(65_536) {
        if spans.len() >= limit {
            break;
        }
        for matched in regex.find_iter(line) {
            spans.push((
                number as u32 + 1,
                line[..matched.start()].encode_utf16().count(),
                line[..matched.end()].encode_utf16().count(),
            ));
            if spans.len() >= limit {
                break;
            }
        }
    }
    spans
}

pub fn preview_of(line: &str) -> String {
    if line.chars().count() <= MAX_LINE_CHARS {
        line.to_owned()
    } else {
        line.chars().take(MAX_LINE_CHARS).collect::<String>() + "\u{2026}"
    }
}

/// Builds the replacement text and the match count it was computed from.
/// `replacement` follows regex-crate expansion syntax, so `$1` refers to
/// capture groups; escape a literal `$` as `$$`.
pub fn replacement(text: &str, regex: &Regex, template: &str) -> Option<Replacement> {
    let mut count = 0usize;
    for matched in regex.find_iter(text) {
        if matched.end() == matched.start() && !matched.as_str().is_empty() {
            continue;
        }
        count += 1;
        if count > MAX_HITS_PER_FILE {
            return None;
        }
    }
    if count == 0 {
        return None;
    }
    let new_text = regex.replace_all(text, template).into_owned();
    if new_text.len() > 2 * 1024 * 1024 {
        return None;
    }
    Some(Replacement {
        new_text,
        match_count: count,
    })
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Replacement {
    pub new_text: String,
    pub match_count: usize,
}

pub fn line_preview(text: &str, line_number: u32) -> Option<String> {
    text.lines()
        .nth(line_number.checked_sub(1)? as usize)
        .map(preview_of)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn case_insensitivity_is_opt_in() {
        let regex = compile_query("TODO", false).expect("compiles");
        assert_eq!(hit_spans(&regex, "todo here\nDONE\nTODO?", 10).len(), 2);
        let regex = compile_query("TODO", true).expect("compiles");
        assert_eq!(hit_spans(&regex, "todo here\nDONE\nTODO?", 10).len(), 1);
    }

    #[test]
    fn spans_carry_line_relative_offsets_and_line_numbers() {
        let regex = compile_query("needle", true).expect("compiles");
        let text = "first line\nneedle in line two\nnope\nneedle again";
        let spans = hit_spans(&regex, text, 10);
        assert_eq!(spans.len(), 2);
        assert_eq!(spans[0].0, 2);
        let (start, end) = (spans[0].1, spans[0].2);
        assert_eq!(start, 0);
        assert_eq!(end, 6);
    }

    #[test]
    fn preview_positions_use_utf16_offsets_with_multibyte_characters() {
        let regex = compile_query("needle", true).unwrap();
        let spans = hit_spans(&regex, "first line\n🦀 needle\nfinal", 10);
        assert_eq!(spans, vec![(2, 3, 9)]);
    }

    #[test]
    fn per_file_hits_are_limited() {
        let regex = compile_query("a", false).expect("compiles");
        let spans = hit_spans(&regex, "aaaaaaaaaaaaaaaaaaaa", 5);
        assert_eq!(spans.len(), 5);
    }

    #[test]
    fn replacement_reports_the_exact_match_count() {
        let regex = compile_query("teh", true).expect("compiles");
        let plan = replacement("the teh teh", &regex, "the").expect("plan");
        assert_eq!(plan.match_count, 2);
        assert_eq!(plan.new_text, "the the the");
    }

    #[test]
    fn replacement_expands_capture_groups() {
        let regex = compile_query("(\\w+)@(\\w+)", true).expect("compiles");
        let plan = replacement("user@host", &regex, "$2:$1").expect("plan");
        assert_eq!(plan.new_text, "host:user");
    }

    #[test]
    fn rejects_replacements_that_expand_a_file_beyond_the_limit() {
        let regex = compile_query("needle", true).unwrap();
        assert!(replacement("needle", &regex, &"x".repeat(2 * 1024 * 1024 + 1)).is_none());
    }

    #[test]
    fn a_file_without_matches_has_no_plan() {
        let regex = compile_query("zzz", true).expect("compiles");
        assert!(replacement("nothing", &regex, "x").is_none());
    }

    #[test]
    fn queries_are_bounded() {
        assert!(compile_query("", false).is_err());
        assert!(compile_query(&"a".repeat(513), false).is_err());
    }

    #[test]
    fn previews_are_bounded() {
        let long = "x".repeat(400);
        let preview = preview_of(&long);
        assert!(preview.chars().count() == MAX_LINE_CHARS + 1);
        assert!(preview_of("short") == "short");
    }
}
