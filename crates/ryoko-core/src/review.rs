//! Structured, independently evidenced review contracts.
//!
//! A model can propose findings, but only the orchestrator can mint trusted
//! validation evidence after an isolated build/test gate actually succeeds.
//! A review has no authority to write files, approve deployments, or merge.
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::model::Task;
use crate::validation::ValidationReport;
use crate::workspace::validate_relative_path;

pub const INDEPENDENT_REVIEW_METADATA: &str = "ryoko.review.independent";
pub const REVIEW_TARGET_METADATA: &str = "ryoko.review.target";
pub const CODE_REVIEW_ROLE: &str = "independent-code-reviewer";
pub const SECURITY_REVIEW_ROLE: &str = "independent-security-reviewer";

pub fn validation_evidence_key(task_id: Uuid) -> String {
    format!("ryoko.task.{task_id}.validation_evidence")
}

/// Only a successful execution gate with a recorded command produces a proof.
/// In particular, AllowAllValidator and a model's assertion of success do not.
pub fn trusted_validation_evidence(
    task_id: Uuid,
    report: &ValidationReport,
) -> Option<String> {
    if !report.passed
        || !report
            .logs
            .iter()
            .any(|line| line.starts_with("spec test files: ") && line.contains("; command: "))
    {
        return None;
    }
    Some(format!("validator:{task_id}"))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReviewVerdict {
    Pass,
    Fail,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReviewSeverity {
    Critical,
    High,
    Medium,
    Low,
    Info,
}

impl ReviewSeverity {
    fn blocks(self) -> bool {
        matches!(self, Self::Critical | Self::High)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReviewFinding {
    /// Stable short machine-readable code, e.g. RYOKO-SEC-001.
    pub code: String,
    pub severity: ReviewSeverity,
    pub path: String,
    #[serde(default)]
    pub line: Option<u32>,
    pub message: String,
    pub recommendation: String,
    /// The trusted evidence reference supplied to the reviewer.
    pub evidence_ref: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReviewRecord {
    pub verdict: ReviewVerdict,
    pub summary: String,
    #[serde(default)]
    pub findings: Vec<ReviewFinding>,
    #[serde(default)]
    pub evidence_refs: Vec<String>,
}

impl ReviewRecord {
    pub fn parse(text: &str) -> Result<Self, String> {
        if text.len() > 64 * 1024 {
            return Err("review record exceeds 64 KiB".to_owned());
        }
        serde_json::from_str(text.trim())
            .map_err(|error| format!("review must be a single structured JSON object: {error}"))
    }

    /// Fail closed on stale or fabricated references and ambiguous verdicts.
    /// This is enforced by the orchestrator, not only the model parser.
    pub fn verify(&self, task: &Task, trusted_reference: Option<&str>) -> Result<(), String> {
        if !task.metadata.contains_key(INDEPENDENT_REVIEW_METADATA) {
            return Err("task is not marked as an independent review".to_owned());
        }
        let target: Uuid = task
            .metadata
            .get(REVIEW_TARGET_METADATA)
            .ok_or_else(|| "review target is missing".to_owned())?
            .parse()
            .map_err(|_| "invalid review target ID".to_owned())?;
        if !task.dependencies.contains(&target) {
            return Err("review target must be a direct dependency".to_owned());
        }
        let expected = trusted_reference
            .ok_or_else(|| "no executed test evidence exists for the reviewed change".to_owned())?;
        if self.evidence_refs.len() != 1 || self.evidence_refs[0] != expected {
            return Err("review did not cite the exact trusted validation evidence".to_owned());
        }
        if self.summary.trim().is_empty() || self.summary.len() > 2_000 {
            return Err("review summary must contain 1..=2000 characters".to_owned());
        }
        if self.findings.len() > 64 {
            return Err("review exceeds the 64-finding limit".to_owned());
        }
        let mut blockers = 0;
        for finding in &self.findings {
            if finding.code.is_empty()
                || finding.code.len() > 64
                || !finding
                    .code
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_')
                || finding.path.len() > 1_024
                || validate_relative_path(&finding.path).is_err()
                || finding.line == Some(0)
                || finding.message.trim().is_empty()
                || finding.message.len() > 2_000
                || finding.recommendation.trim().is_empty()
                || finding.recommendation.len() > 2_000
                || finding.evidence_ref != expected
            {
                return Err("review contains an invalid or unevidenced finding".to_owned());
            }
            if finding.severity.blocks() {
                blockers += 1;
            }
        }
        match self.verdict {
            ReviewVerdict::Pass if blockers != 0 => {
                Err("a passing review cannot contain critical/high findings".to_owned())
            }
            ReviewVerdict::Fail if blockers == 0 => {
                Err("a failed review must identify at least one critical/high finding".to_owned())
            }
            _ => Ok(()),
        }
    }

    pub fn failure_feedback(&self) -> String {
        let findings = self
            .findings
            .iter()
            .filter(|finding| finding.severity.blocks())
            .map(|finding| {
                format!(
                    "{} {:?} {}:{}: {}. Fix: {}",
                    finding.code,
                    finding.severity,
                    finding.path,
                    finding.line.unwrap_or(1),
                    finding.message,
                    finding.recommendation
                )
            })
            .collect::<Vec<_>>()
            .join("\n");
        format!("independent review failed: {}\n{findings}", self.summary)
    }
}

/// Explicit, per-run reviewer selection; no role grants additional permissions.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ReviewConfig {
    pub code: bool,
    pub security: bool,
}

impl Default for ReviewConfig {
    fn default() -> Self {
        Self {
            code: true,
            security: true,
        }
    }
}

pub const REVIEW_SYSTEM_PROMPT: &str = r#"You are an independent, read-only engineering reviewer.
Return exactly one JSON object, with no Markdown, comments or code changes:
{
  "verdict": "pass",
  "summary": "what was verified and any remaining uncertainty",
  "findings": [],
  "evidence_refs": ["validator:<task-uuid>"]
}
For a failing review set verdict to "fail" and include at least one finding:
{"code":"RYOKO-001","severity":"high","path":"src/file.rs","line":10,
"message":"reproducible defect","recommendation":"specific fix",
"evidence_ref":"validator:<task-uuid>"}
Only cite the trusted validator reference supplied in the task's evidence context.
Do not pretend that an unexecuted test or an uninspected file was checked.
Critical/high findings block approval. A file proposal is forbidden.
If evidence is insufficient, report a high-severity actionable finding.
"#;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::AgentRole;
    use std::time::Duration;

    fn reviewer() -> Task {
        let target = Uuid::new_v4();
        Task::new(
            "independent review",
            AgentRole::Custom(CODE_REVIEW_ROLE.to_owned()),
        )
        .with_dependency(target)
        .with_metadata(INDEPENDENT_REVIEW_METADATA, "true")
        .with_metadata(REVIEW_TARGET_METADATA, target.to_string())
    }

    fn passing(reference: &str) -> ReviewRecord {
        ReviewRecord {
            verdict: ReviewVerdict::Pass,
            summary: "tests and diff inspected".to_owned(),
            findings: Vec::new(),
            evidence_refs: vec![reference.to_owned()],
        }
    }

    #[test]
    fn validator_only_mints_evidence_for_real_executed_gate() {
        let task_id = Uuid::new_v4();
        let allow_all = ValidationReport::passed(
            "guardrails disabled",
            vec!["AllowAllValidator is active".to_owned()],
            Duration::ZERO,
        );
        assert!(trusted_validation_evidence(task_id, &allow_all).is_none());
        let executed = ValidationReport::passed(
            "pre-flight passed",
            vec!["spec test files: src/lib.rs; command: cargo test".to_owned()],
            Duration::ZERO,
        );
        assert_eq!(
            trusted_validation_evidence(task_id, &executed),
            Some(format!("validator:{task_id}"))
        );
        let rejected = ValidationReport::failed(
            "failed",
            executed.logs,
            Duration::ZERO,
        );
        assert!(trusted_validation_evidence(task_id, &rejected).is_none());
    }

    #[test]
    fn review_refuses_missing_stale_or_fabricated_proof() {
        let task = reviewer();
        let proof = format!("validator:{}", task.dependencies[0]);
        assert!(passing(&proof).verify(&task, Some(&proof)).is_ok());
        assert!(passing(&proof).verify(&task, None).is_err());
        assert!(passing("validator:other").verify(&task, Some(&proof)).is_err());
        let mut unbound = task.clone();
        unbound.dependencies.clear();
        assert!(passing(&proof).verify(&unbound, Some(&proof)).is_err());
    }

    #[test]
    fn blocking_findings_must_fail_with_actionable_evidence() {
        let task = reviewer();
        let proof = format!("validator:{}", task.dependencies[0]);
        let mut record = passing(&proof);
        record.findings.push(ReviewFinding {
            code: "RYOKO-001".to_owned(),
            severity: ReviewSeverity::High,
            path: "src/lib.rs".to_owned(),
            line: Some(4),
            message: "missing error handling".to_owned(),
            recommendation: "return the error".to_owned(),
            evidence_ref: proof.clone(),
        });
        assert!(record.verify(&task, Some(&proof)).is_err());
        record.verdict = ReviewVerdict::Fail;
        assert!(record.verify(&task, Some(&proof)).is_ok());
        assert!(record.failure_feedback().contains("RYOKO-001"));
        record.findings[0].path = "../escape".to_owned();
        assert!(record.verify(&task, Some(&proof)).is_err());
    }

    #[test]
    fn rejects_malformed_or_unbounded_model_output() {
        assert!(ReviewRecord::parse("not json").is_err());
        assert!(ReviewRecord::parse(&"x".repeat(65_537)).is_err());
        assert!(ReviewRecord::parse(
            r#"{"verdict":"pass","summary":"ok","findings":[],"evidence_refs":[],"files":[]}"#
        )
        .is_err());
    }
}
