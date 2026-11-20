//! Multimodal UI verification: harness artifacts in, structured critique out.
//!
//! The `verify-ui.mjs` harness writes screenshots, an accessibility tree and a
//! runtime error log per route. This module reads those artifacts, decides what
//! the configured model can actually consume, and parses the UI/UX agent's
//! critique into a schema the orchestrator can route back as repair feedback.
//!
//! Two rules hold everywhere here: artifacts are untrusted input, so every
//! string is bounded before it reaches a prompt; and a model that is not known to
//! accept images never receives base64 PNGs.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use ryoko_models::{ContentBlock, Message, Role};

use crate::encoding::base64_encode;
use crate::error::{CoreError, CoreResult};
use crate::validation::ValidationReport;

pub const ARIA_TREE_FILE: &str = "aria-tree.json";
pub const CONSOLE_ERRORS_FILE: &str = "console-errors.json";
/// Metadata key the orchestrator reads to route a visual critique back to the
/// implementing agent.
pub const VISUAL_FEEDBACK_METADATA: &str = "ryoko.visual-feedback";
/// Metadata flag that marks a task as a visual review of the tasks it depends
/// on. Only a task carrying it answers with a critique: the UI/UX role is also
/// a producer, and a producer must still return files.
pub const VISUAL_REVIEW_METADATA: &str = "ryoko.visual-review";

const MAX_CAPTURES: usize = 25;
const MAX_ARTIFACT_BYTES: u64 = 4 * 1024 * 1024;
const MAX_ARIA_CHARS: usize = 24_000;
const MAX_ISSUES: usize = 50;
const MAX_ISSUE_CHARS: usize = 500;
const MAX_INSTRUCTIONS_CHARS: usize = 4_000;
const MAX_PROMPT_CHARS: usize = 60_000;
const MAX_IMAGE_BYTES: u64 = 6 * 1024 * 1024;

/// One route's captured evidence.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RouteCapture {
    pub route: String,
    pub directory: PathBuf,
    pub desktop_png: Option<PathBuf>,
    pub mobile_png: Option<PathBuf>,
    pub aria_tree: Option<String>,
    pub aria_source: Option<String>,
    pub console_errors: Vec<String>,
    pub page_errors: Vec<String>,
    pub failed_requests: Vec<String>,
    pub horizontal_overflow: bool,
}

impl RouteCapture {
    /// Everything the harness recorded as wrong, in one bounded list.
    pub fn issues(&self) -> Vec<String> {
        let mut issues = Vec::new();
        for error in &self.console_errors {
            issues.push(format!("console error: {}", trim(error, MAX_ISSUE_CHARS)));
        }
        for error in &self.page_errors {
            issues.push(format!(
                "uncaught exception: {}",
                trim(error, MAX_ISSUE_CHARS)
            ));
        }
        for request in &self.failed_requests {
            issues.push(format!(
                "failed request: {}",
                trim(request, MAX_ISSUE_CHARS)
            ));
        }
        if self.horizontal_overflow {
            issues.push("horizontal overflow: content is wider than the viewport".to_owned());
        }
        issues.truncate(MAX_ISSUES);
        issues
    }

    pub fn is_clean(&self) -> bool {
        self.issues().is_empty()
    }
}

fn trim(text: &str, limit: usize) -> String {
    let cleaned: String = text
        .chars()
        .filter(|c| !c.is_control() || *c == '\n')
        .collect();
    if cleaned.chars().count() <= limit {
        return cleaned;
    }
    cleaned.chars().take(limit).collect::<String>() + "…"
}

fn read_bounded(path: &Path) -> CoreResult<Option<String>> {
    let metadata = match std::fs::metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(CoreError::Workspace(format!(
                "cannot read {}: {error}",
                path.display()
            )));
        }
    };
    if !metadata.is_file() || metadata.len() > MAX_ARTIFACT_BYTES {
        return Ok(None);
    }
    std::fs::read_to_string(path)
        .map(Some)
        .map_err(|error| CoreError::Workspace(format!("cannot read {}: {error}", path.display())))
}

fn strings(value: Option<&serde_json::Value>) -> Vec<String> {
    value
        .and_then(|value| value.as_array())
        .map(|items| {
            items
                .iter()
                .filter_map(|item| item.as_str())
                .map(|item| trim(item, MAX_ISSUE_CHARS))
                .collect()
        })
        .unwrap_or_default()
}

fn failed_requests(value: Option<&serde_json::Value>) -> Vec<String> {
    value
        .and_then(|value| value.as_array())
        .map(|items| {
            items
                .iter()
                .filter_map(|item| {
                    let url = item.get("url")?.as_str()?;
                    let detail = match (item.get("status"), item.get("reason")) {
                        (Some(status), _) => format!("HTTP {status}"),
                        (_, Some(reason)) => reason.as_str().unwrap_or("failed").to_owned(),
                        _ => "failed".to_owned(),
                    };
                    Some(format!("{} ({detail})", trim(url, MAX_ISSUE_CHARS)))
                })
                .collect()
        })
        .unwrap_or_default()
}

/// Merges several artifact directories, as a review task sees the captures of
/// every task it depends on. The first capture of a route wins, so the task under
/// review keeps precedence over a name collision.
pub fn read_captures_from(dirs: &[PathBuf]) -> CoreResult<Vec<RouteCapture>> {
    let mut merged: Vec<RouteCapture> = Vec::new();
    for dir in dirs {
        for capture in read_captures(dir)? {
            if merged.iter().any(|kept| kept.route == capture.route) {
                continue;
            }
            if merged.len() >= MAX_CAPTURES {
                break;
            }
            merged.push(capture);
        }
    }
    merged.sort_by(|left, right| left.route.cmp(&right.route));
    Ok(merged)
}

/// Reads one artifact directory, sorted by route directory name.
pub fn read_captures(dir: &Path) -> CoreResult<Vec<RouteCapture>> {
    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => {
            return Err(CoreError::Workspace(format!(
                "cannot read {}: {error}",
                dir.display()
            )));
        }
    };
    let mut directories: Vec<PathBuf> = entries
        .flatten()
        .filter(|entry| entry.file_type().map(|kind| kind.is_dir()).unwrap_or(false))
        .map(|entry| entry.path())
        .collect();
    directories.sort();
    if directories.len() > MAX_CAPTURES {
        return Err(CoreError::Workspace(format!(
            "more than {MAX_CAPTURES} captured routes; verify fewer routes per run"
        )));
    }

    let mut captures = Vec::new();
    for directory in directories {
        let Some(text) = read_bounded(&directory.join(CONSOLE_ERRORS_FILE))? else {
            continue;
        };
        let report: serde_json::Value = serde_json::from_str(&text).map_err(|error| {
            CoreError::Workspace(format!(
                "{} is not valid JSON: {error}",
                CONSOLE_ERRORS_FILE
            ))
        })?;
        let aria = read_bounded(&directory.join(ARIA_TREE_FILE))?
            .and_then(|text| serde_json::from_str::<serde_json::Value>(&text).ok());
        let aria_tree = aria
            .as_ref()
            .and_then(|value| value.get("tree"))
            .map(|tree| {
                let text = match tree {
                    serde_json::Value::String(text) => text.clone(),
                    other => other.to_string(),
                };
                trim(&text, MAX_ARIA_CHARS)
            });
        let aria_source = aria
            .as_ref()
            .and_then(|value| value.get("source"))
            .and_then(|value| value.as_str())
            .map(str::to_owned);
        let overflow = report
            .get("horizontalOverflow")
            .and_then(|value| value.as_bool())
            .unwrap_or(false)
            || ["desktop", "mobile"].iter().any(|viewport| {
                report
                    .get("overflow")
                    .and_then(|value| value.get(viewport))
                    .and_then(|value| value.get("overflowing"))
                    .and_then(|value| value.as_bool())
                    .unwrap_or(false)
            });
        let desktop = directory.join("desktop.png");
        let mobile = directory.join("mobile.png");
        captures.push(RouteCapture {
            route: report
                .get("route")
                .and_then(|value| value.as_str())
                .map(|route| trim(route, 200))
                .unwrap_or_else(|| {
                    directory
                        .file_name()
                        .map(|name| name.to_string_lossy().to_string())
                        .unwrap_or_default()
                }),
            desktop_png: usable_png(&desktop),
            mobile_png: usable_png(&mobile),
            aria_tree,
            aria_source,
            console_errors: strings(report.get("consoleErrors")),
            page_errors: strings(report.get("pageErrors")),
            failed_requests: failed_requests(report.get("failedRequests")),
            horizontal_overflow: overflow,
            directory,
        });
    }
    // Route order is what the prompt shows, so it must not depend on directory
    // names on disk.
    captures.sort_by(|left, right| left.route.cmp(&right.route));
    Ok(captures)
}

fn usable_png(path: &Path) -> Option<PathBuf> {
    std::fs::metadata(path)
        .ok()
        .filter(|metadata| {
            metadata.is_file() && metadata.len() > 0 && metadata.len() <= MAX_IMAGE_BYTES
        })
        .map(|_| path.to_path_buf())
}

/// Whether a model id is known to accept images. Anything unrecognised is
/// treated as text-only: sending image bytes to a model that cannot read them is
/// a hard error at best and a silent, expensive no-op at worst.
pub fn supports_vision(model: &str) -> bool {
    let model = model.to_ascii_lowercase();
    const VISION_MARKERS: [&str; 14] = [
        "gpt-4o",
        "gpt-4.1",
        "gpt-4-turbo",
        "gpt-4-vision",
        "claude-3",
        "claude-sonnet",
        "claude-opus",
        "claude-haiku",
        "gemini",
        "qwen2-vl",
        "qwen2.5-vl",
        "llava",
        "pixtral",
        "internvl",
    ];
    VISION_MARKERS.iter().any(|marker| model.contains(marker))
}

/// Renders the artifact set as a prompt. Vision models receive the screenshots;
/// text-only models receive the accessibility tree and the measured overflow
/// report instead, so offline verification still works.
pub fn vision_message(captures: &[RouteCapture], model: &str) -> CoreResult<Message> {
    let vision = supports_vision(model);
    let mut text = String::new();
    text.push_str(&format!(
        "UI verification for {} route(s).\n",
        captures.len()
    ));
    if !vision {
        text.push_str(&format!(
            "Screenshots are omitted because `{model}` is treated as text-only; \
             use the accessibility tree and the overflow report.\n"
        ));
    }
    for capture in captures {
        text.push_str(&format!(
            "\n## Route {}\n- desktop screenshot: {}\n- mobile screenshot: {}\n- horizontal overflow: {}\n",
            capture.route,
            if capture.desktop_png.is_some() { "captured" } else { "missing" },
            if capture.mobile_png.is_some() { "captured" } else { "missing" },
            if capture.horizontal_overflow { "yes" } else { "no" },
        ));
        let issues = capture.issues();
        if issues.is_empty() {
            text.push_str("- runtime issues: none\n");
        } else {
            text.push_str("- runtime issues:\n");
            for issue in &issues {
                text.push_str(&format!("  - {issue}\n"));
            }
        }
        if let Some(tree) = &capture.aria_tree {
            text.push_str(&format!(
                "- accessibility tree ({}):\n{}\n",
                capture.aria_source.as_deref().unwrap_or("unknown"),
                tree
            ));
        }
    }
    text.truncate(MAX_PROMPT_CHARS);

    let mut content = vec![ContentBlock::Text { text }];
    if vision {
        for capture in captures {
            for (label, path) in [
                ("desktop", &capture.desktop_png),
                ("mobile", &capture.mobile_png),
            ] {
                let Some(path) = path else { continue };
                let bytes = std::fs::read(path).map_err(|error| {
                    CoreError::Workspace(format!("cannot read {}: {error}", path.display()))
                })?;
                if bytes.len() as u64 > MAX_IMAGE_BYTES {
                    continue;
                }
                let encoded = base64_encode(&bytes);
                content.push(
                    ContentBlock::image(
                        "image/png",
                        &encoded,
                        Some(format!("{} {label}", capture.route)),
                    )
                    .map_err(|error| CoreError::Workspace(error.to_string()))?,
                );
            }
        }
    }
    Ok(Message {
        role: Role::User,
        content,
    })
}

/// The UI/UX agent's structured review. Text-only models produce this from the
/// accessibility tree; vision models from the screenshots. Either way the shape
/// is fixed, so the orchestrator can route it without reading prose.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VisualCritique {
    pub passed: bool,
    #[serde(default)]
    pub layout_issues: Vec<String>,
    #[serde(default)]
    pub revision_instructions: Option<String>,
}

impl VisualCritique {
    /// Parses a critique out of model output that may wrap it in prose or a
    /// fenced block. Only the first JSON object is considered, and it is bounded.
    pub fn parse(raw: &str) -> CoreResult<Self> {
        let start = raw
            .find('{')
            .ok_or_else(|| CoreError::Workspace("visual critique has no JSON object".to_owned()))?;
        let end = raw.rfind('}').filter(|end| *end > start).ok_or_else(|| {
            CoreError::Workspace("visual critique JSON is unterminated".to_owned())
        })?;
        if end - start > MAX_PROMPT_CHARS {
            return Err(CoreError::Workspace(
                "visual critique JSON is too large".to_owned(),
            ));
        }
        let critique: Self = serde_json::from_str(&raw[start..=end]).map_err(|error| {
            CoreError::Workspace(format!("visual critique is malformed: {error}"))
        })?;
        critique.validate()?;
        Ok(critique)
    }

    pub fn validate(&self) -> CoreResult<()> {
        if self.layout_issues.len() > MAX_ISSUES {
            return Err(CoreError::Workspace(format!(
                "visual critique has more than {MAX_ISSUES} issues"
            )));
        }
        if self
            .layout_issues
            .iter()
            .any(|issue| issue.chars().count() > MAX_ISSUE_CHARS)
        {
            return Err(CoreError::Workspace(
                "a visual critique issue is too long".to_owned(),
            ));
        }
        if self
            .revision_instructions
            .as_ref()
            .is_some_and(|text| text.chars().count() > MAX_INSTRUCTIONS_CHARS)
        {
            return Err(CoreError::Workspace(
                "visual critique instructions are too long".to_owned(),
            ));
        }
        if !self.passed && self.layout_issues.is_empty() && self.revision_instructions.is_none() {
            return Err(CoreError::Workspace(
                "a failed visual critique needs at least one issue or instruction".to_owned(),
            ));
        }
        Ok(())
    }

    /// The text routed back to the implementing agent on a failed review.
    pub fn feedback(&self) -> Option<String> {
        if self.passed {
            return None;
        }
        let mut text = String::from("Visual review failed:");
        for issue in &self.layout_issues {
            text.push_str(&format!("\n- {}", trim(issue, MAX_ISSUE_CHARS)));
        }
        if let Some(instructions) = &self.revision_instructions {
            text.push_str(&format!(
                "\nInstructions: {}",
                trim(instructions, MAX_INSTRUCTIONS_CHARS)
            ));
        }
        Some(trim(&text, MAX_PROMPT_CHARS))
    }

    /// A validation report the orchestrator can treat like any other check.
    pub fn report(&self) -> ValidationReport {
        if self.passed {
            return ValidationReport::passed(
                "visual review passed",
                Vec::new(),
                std::time::Duration::from_millis(0),
            );
        }
        ValidationReport::failed(
            "visual review failed",
            self.layout_issues.clone(),
            std::time::Duration::from_millis(0),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ryoko_models::ContentBlock;
    use tempfile::TempDir;

    fn write(dir: &Path, route: &str, report: serde_json::Value, aria: Option<&str>, png: bool) {
        // Mirrors the harness slugging: `/` becomes `root`.
        let slug = route
            .split('/')
            .filter(|part| !part.is_empty())
            .collect::<Vec<_>>()
            .join("-");
        let folder = dir.join(if slug.is_empty() {
            "root".to_owned()
        } else {
            slug
        });
        std::fs::create_dir_all(&folder).unwrap();
        std::fs::write(
            folder.join(CONSOLE_ERRORS_FILE),
            serde_json::to_string(&report).unwrap(),
        )
        .unwrap();
        if let Some(aria) = aria {
            std::fs::write(
                folder.join(ARIA_TREE_FILE),
                serde_json::json!({"source": "ariaSnapshot", "tree": aria}).to_string(),
            )
            .unwrap();
        }
        if png {
            std::fs::write(folder.join("desktop.png"), b"\x89PNG\r\n\x1a\nfake").unwrap();
            std::fs::write(folder.join("mobile.png"), b"\x89PNG\r\n\x1a\nfake").unwrap();
        }
    }

    #[test]
    fn reads_harness_artifacts_per_route() {
        let dir = TempDir::new().unwrap();
        write(
            dir.path(),
            "/",
            serde_json::json!({
                "route": "/",
                "consoleErrors": ["Warning: each child needs a key"],
                "pageErrors": ["TypeError: x is not a function"],
                "failedRequests": [
                    {"url": "http://127.0.0.1:4173/app.js", "status": 404},
                    {"url": "http://127.0.0.1:4173/api", "reason": "net::ERR_FAILED"}
                ],
                "overflow": {"desktop": {"overflowing": true}, "mobile": {"overflowing": false}},
                "horizontalOverflow": true
            }),
            Some("button[aria-label=\"Buy\"]"),
            true,
        );
        write(
            dir.path(),
            "/about",
            serde_json::json!({"route": "/about", "consoleErrors": []}),
            None,
            false,
        );
        let captures = read_captures(dir.path()).unwrap();
        assert_eq!(captures.len(), 2);
        assert_eq!(captures[0].route, "/");
        assert!(captures[0].horizontal_overflow);
        assert!(captures[0].aria_tree.as_ref().unwrap().contains("Buy"));
        assert!(captures[0].desktop_png.is_some());
        assert_eq!(captures[1].mobile_png, None);
        let issues = captures[0].issues();
        assert!(issues.iter().any(|issue| issue.contains("HTTP 404")));
        assert!(
            issues
                .iter()
                .any(|issue| issue.contains("uncaught exception"))
        );
        assert!(
            issues
                .iter()
                .any(|issue| issue.contains("horizontal overflow"))
        );
        assert!(captures[1].is_clean());
        assert!(
            read_captures(&dir.path().join("absent"))
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn a_malformed_artifact_is_an_error_not_silence() {
        let dir = TempDir::new().unwrap();
        let folder = dir.path().join("root");
        std::fs::create_dir_all(&folder).unwrap();
        std::fs::write(folder.join(CONSOLE_ERRORS_FILE), "not json").unwrap();
        assert!(read_captures(dir.path()).is_err());
    }

    #[test]
    fn vision_capability_is_conservative() {
        for model in [
            "openai/gpt-4o",
            "anthropic/claude-sonnet-4",
            "google/gemini-2.0-flash",
            "local/qwen2.5-vl-7b",
            "local/llava:13b",
        ] {
            assert!(supports_vision(model), "{model}");
        }
        for model in [
            "local/qwen2.5-coder",
            "openai/gpt-3.5-turbo",
            "mock/deterministic",
        ] {
            assert!(!supports_vision(model), "{model}");
        }
    }

    #[test]
    fn a_vision_model_receives_images_and_a_text_model_does_not() {
        let dir = TempDir::new().unwrap();
        write(
            dir.path(),
            "/",
            serde_json::json!({"route": "/", "consoleErrors": []}),
            Some("main(button \"Buy\")"),
            true,
        );
        let captures = read_captures(dir.path()).unwrap();

        let message = vision_message(&captures, "anthropic/claude-sonnet-4").unwrap();
        let images = message
            .content
            .iter()
            .filter(|block| matches!(block, ContentBlock::Image { .. }))
            .count();
        assert_eq!(images, 2, "desktop and mobile screenshots");

        let message = vision_message(&captures, "local/qwen2.5-coder").unwrap();
        assert!(
            message
                .content
                .iter()
                .all(|block| !matches!(block, ContentBlock::Image { .. }))
        );
        let ContentBlock::Text { text } = &message.content[0] else {
            panic!("expected a text block");
        };
        assert!(text.contains("text-only"));
        assert!(
            text.contains("Buy"),
            "the ARIA tree must reach a text model"
        );
        assert!(text.contains("horizontal overflow: no"));
    }

    #[test]
    fn critiques_are_parsed_from_prose_and_fences() {
        let raw = "Here is my review:\n```json\n{\"passed\": false, \"layout_issues\": [\"CTA overlaps the footer on mobile\"], \"revision_instructions\": \"Stack the footer below the CTA\"}\n```\n";
        let critique = VisualCritique::parse(raw).unwrap();
        assert!(!critique.passed);
        assert_eq!(critique.layout_issues.len(), 1);
        let feedback = critique.feedback().unwrap();
        assert!(feedback.contains("CTA overlaps"));
        assert!(feedback.contains("Stack the footer"));
        assert!(!critique.report().passed);

        let raw = r#"{"passed": true, "layout_issues": [], "revision_instructions": null}"#;
        let critique = VisualCritique::parse(raw).unwrap();
        assert!(critique.feedback().is_none());
        assert!(critique.report().passed);
    }

    #[test]
    fn unbounded_or_empty_critiques_are_rejected() {
        assert!(VisualCritique::parse("no json here").is_err());
        assert!(
            VisualCritique::parse("{\"passed\": false}").is_err(),
            "needs an issue"
        );
        let too_many = serde_json::json!({
            "passed": false,
            "layout_issues": vec!["x"; MAX_ISSUES + 1],
            "revision_instructions": "fix it"
        });
        assert!(VisualCritique::parse(&too_many.to_string()).is_err());
        let long_issue = "y".repeat(MAX_ISSUE_CHARS + 1);
        let oversized = serde_json::json!({"passed": false, "layout_issues": [long_issue]});
        assert!(VisualCritique::parse(&oversized.to_string()).is_err());
    }
}
