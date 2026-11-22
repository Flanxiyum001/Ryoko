//! Prompt construction and answer validation for Cmd+K targeted inline edits.
//!
//! The model sees the current selection plus bounded surrounding context and
//! returns the full replacement text for the edited lines. Everything it
//! returns is untrusted: only plain UTF-8 text within the size bound is
//! accepted, and code fences are tolerated because chat models add them even
//! when asked not to.

pub const MAX_CONTEXT_CHARS: usize = 12_000;
const MAX_ANSWER_BYTES: usize = 512 * 1024;

#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct InlineEditRequest {
    pub instructions: String,
    pub language: String,
    pub path: String,
    pub before: String,
    pub selected: String,
    pub after: String,
}

pub fn build_prompt(request: &InlineEditRequest) -> String {
    let before = bounded(&request.before, MAX_CONTEXT_CHARS / 2);
    let after = bounded(&request.after, MAX_CONTEXT_CHARS / 2);
    format!(
        "Rewrite the selected lines of this {} file. Return ONLY the new text for the selection, in plain UTF-8, with no commentary and no surrounding code fences. Keep the same indentation style. Preserve every other line of the file exactly.\n\nFile: {}\nInstructions: {}\n\nText before the selection (do not repeat it in the answer):\n{before}\n\nSELECTED TEXT TO REWRITE:\n{selected}\n\nText after the selection (do not repeat it):\n{after}",
        request.language,
        request.path,
        bounded(&request.instructions, 4_000),
        selected = bounded(&request.selected, MAX_CONTEXT_CHARS),
    )
}

fn bounded(text: &str, limit: usize) -> String {
    if text.chars().count() <= limit {
        text.to_owned()
    } else {
        text.chars().take(limit).collect::<String>() + "\n\u{2026} [truncated]"
    }
}

/// Strips one wrapping code fence, including its optional language tag. A
/// response that merely contains fenced code is returned unchanged: only a
/// fully-wrapped answer is unwrapped, so partial fences never swallow content.
pub fn strip_wrapping_fence(answer: &str) -> String {
    let trimmed = answer.trim_start_matches('\u{feff}').trim();
    let Some(rest) = trimmed.strip_prefix("```") else {
        return trimmed.trim_end().to_owned();
    };
    let Some((_, body)) = rest.split_once('\n') else {
        return trimmed.to_owned();
    };
    let Some(body) = body.strip_suffix("```") else {
        return trimmed.to_owned();
    };
    let Some(last) = body.rfind('\n') else {
        return trimmed.to_owned();
    };
    body[..last].trim_matches('\n').to_owned()
}

pub fn validate_answer(answer: &str) -> Result<String, String> {
    if answer.len() > MAX_ANSWER_BYTES {
        return Err("inline edit answer exceeds 512 KiB".to_owned());
    }
    Ok(strip_wrapping_fence(answer))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prompt_bounds_each_context_part() {
        let request = InlineEditRequest {
            instructions: "replace with fyodor".repeat(800),
            language: "rust".into(),
            path: "src/main.rs".into(),
            before: "b".repeat(20_000),
            selected: "SELECT".into(),
            after: String::new(),
        };
        let prompt = build_prompt(&request);
        assert!(prompt.contains("src/main.rs"));
        assert!(prompt.contains("SELECT"));
        assert!(prompt.len() < 40_000, "context is bounded");
    }

    #[test]
    fn strips_one_wrapping_fence_with_language() {
        let answer = "```rust\nfn a() {}\n\n```";
        assert_eq!(strip_wrapping_fence(answer), "fn a() {}");
    }

    #[test]
    fn a_partial_fence_is_left_alone() {
        let answer = "text with ``` inside but no wrap";
        assert_eq!(strip_wrapping_fence(answer), answer);
        let answer = "```\nfenced body without close";
        assert_eq!(strip_wrapping_fence(answer), answer);
    }

    #[test]
    fn a_wrapped_answer_containing_fences_is_unwrapped_once() {
        let answer = "```\nouter\n```inner```\n```";
        assert_eq!(strip_wrapping_fence(answer), "outer\n```inner```");
    }

    #[test]
    fn oversized_answers_are_refused() {
        assert!(validate_answer(&"a".repeat(600_000)).is_err());
        assert!(validate_answer("fn ok() {}").is_ok());
    }
}
