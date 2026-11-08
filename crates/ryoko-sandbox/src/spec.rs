//! Spec-driven proposals.
//!
//! A proposal is only reviewable when it carries tests and the same WASM test
//! artifact that guards normal publication exits cleanly. This gate sits in
//! front of [`SandboxValidator`] and refuses empty test evidence before any
//! artifact is built, so a source-only change can never reach a pull request.

use std::sync::Arc;
use std::time::Instant;

use async_trait::async_trait;
use ryoko_core::CoreResult;
use ryoko_core::validation::{ChangeValidator, StagedChanges, ValidationReport};

use crate::{SandboxError, SandboxValidator, TestArtifact, WasmSandbox};

/// Minimum test evidence a proposal must carry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SpecPolicy {
    /// Number of test-bearing files required. Defaults to one.
    pub minimum_test_files: usize,
    /// When true, a proposal made only of tests is rejected.
    pub require_source_file: bool,
}

impl Default for SpecPolicy {
    fn default() -> Self {
        Self {
            minimum_test_files: 1,
            require_source_file: false,
        }
    }
}

/// Files classified while checking a proposal.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SpecEvidence {
    pub test_files: Vec<String>,
    pub source_files: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum SpecError {
    #[error("proposal has no test file or inline test block")]
    MissingTests,
    #[error("proposal has {found} test file(s), but at least {required} are required")]
    TooFewTests { found: usize, required: usize },
    #[error("proposal has no non-test source file")]
    MissingSource,
}

impl SpecPolicy {
    pub fn inspect(&self, changes: &StagedChanges) -> Result<SpecEvidence, SpecError> {
        let mut test_files = Vec::new();
        let mut source_files = Vec::new();
        for change in &changes.files {
            if change.is_test() {
                test_files.push(change.path.clone());
            } else {
                source_files.push(change.path.clone());
            }
        }
        if test_files.is_empty() && self.minimum_test_files > 0 {
            return Err(SpecError::MissingTests);
        }
        if test_files.len() < self.minimum_test_files {
            return Err(SpecError::TooFewTests {
                found: test_files.len(),
                required: self.minimum_test_files,
            });
        }
        if self.require_source_file && source_files.is_empty() {
            return Err(SpecError::MissingSource);
        }
        Ok(SpecEvidence {
            test_files,
            source_files,
        })
    }
}

/// [`ChangeValidator`] that requires tests and then runs the normal sandbox
/// guardrail. The inner validator is always [`SandboxValidator`], so a passing
/// report means the test artifact actually executed in the capability-free
/// wasmtime store.
pub struct SandboxSpecValidator {
    validator: SandboxValidator,
    policy: SpecPolicy,
}

impl SandboxSpecValidator {
    pub fn new(validator: SandboxValidator) -> Self {
        Self {
            validator,
            policy: SpecPolicy::default(),
        }
    }

    pub fn from_parts(
        sandbox: WasmSandbox,
        artifact: Arc<dyn TestArtifact>,
        policy: SpecPolicy,
    ) -> Self {
        Self {
            validator: SandboxValidator::new(sandbox, artifact),
            policy,
        }
    }

    pub fn with_policy(mut self, policy: SpecPolicy) -> Self {
        self.policy = policy;
        self
    }

    pub fn policy(&self) -> &SpecPolicy {
        &self.policy
    }

    /// Convenience constructor for tests and dry-run harnesses.
    pub fn static_module(
        module: impl Into<Vec<u8>>,
        policy: SpecPolicy,
    ) -> Result<Self, SandboxError> {
        Ok(Self::new(SandboxValidator::static_module(module)?).with_policy(policy))
    }
}

#[async_trait]
impl ChangeValidator for SandboxSpecValidator {
    async fn validate(&self, changes: &StagedChanges) -> CoreResult<ValidationReport> {
        let started = Instant::now();
        let evidence = match self.policy.inspect(changes) {
            Ok(evidence) => evidence,
            Err(error) => {
                return Ok(ValidationReport::failed(
                    "spec check failed",
                    vec![error.to_string()],
                    started.elapsed(),
                ));
            }
        };

        let mut report = self.validator.validate(changes).await?;
        if report.passed {
            report.summary = format!(
                "spec satisfied: {} test file(s); {}",
                evidence.test_files.len(),
                report.summary
            );
            report.logs.insert(
                0,
                format!("spec test files: {}", evidence.test_files.join(", ")),
            );
        } else {
            report.summary = format!("spec tests failed in sandbox: {}", report.summary);
            report.logs.insert(
                0,
                format!(
                    "spec test files: {}; source files: {}",
                    evidence.test_files.join(", "),
                    evidence.source_files.join(", ")
                ),
            );
        }
        Ok(report)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ryoko_core::ProposedChange;
    use uuid::Uuid;

    fn changes(files: Vec<ProposedChange>) -> StagedChanges {
        StagedChanges::new(Uuid::new_v4(), files)
    }

    fn source() -> ProposedChange {
        ProposedChange::new("src/api.rs", "pub fn health() -> bool { true }")
    }

    fn test_file() -> ProposedChange {
        ProposedChange::new("tests/api.rs", "#[test]\nfn health() { assert!(true); }")
    }

    #[test]
    fn policy_requires_test_evidence() {
        let policy = SpecPolicy::default();
        let error = policy
            .inspect(&changes(vec![source()]))
            .expect_err("source-only proposal must fail");
        assert_eq!(error, SpecError::MissingTests);

        let evidence = policy
            .inspect(&changes(vec![source(), test_file()]))
            .unwrap();
        assert_eq!(evidence.test_files, vec!["tests/api.rs"]);
        assert_eq!(evidence.source_files, vec!["src/api.rs"]);
    }

    #[test]
    fn inline_tests_satisfy_policy() {
        let inline = ProposedChange::new(
            "src/api.rs",
            "pub fn health() -> bool { true }\n#[cfg(test)]\nmod tests { #[test] fn works() {} }",
        );
        let evidence = SpecPolicy::default()
            .inspect(&changes(vec![inline]))
            .unwrap();
        assert_eq!(evidence.test_files, vec!["src/api.rs"]);
    }

    #[tokio::test]
    async fn source_without_tests_is_rejected_before_sandbox() {
        let validator = SandboxSpecValidator::static_module(
            br#"(module (func (export "_start") unreachable))"#,
            SpecPolicy::default(),
        )
        .unwrap();
        let report = validator.validate(&changes(vec![source()])).await.unwrap();
        assert!(!report.passed);
        assert_eq!(report.summary, "spec check failed");
        assert!(report.logs.join(" ").contains("no test file"));
    }

    #[tokio::test]
    async fn test_evidence_runs_the_sandbox() {
        let validator = SandboxSpecValidator::static_module(
            br#"(module (func (export "_start")))"#,
            SpecPolicy::default(),
        )
        .unwrap();
        let report = validator
            .validate(&changes(vec![source(), test_file()]))
            .await
            .unwrap();
        assert!(report.passed);
        assert!(report.summary.contains("spec satisfied"));
        assert!(report.logs.iter().any(|line| line.contains("tests/api.rs")));
    }

    #[tokio::test]
    async fn failing_tests_are_rejected_as_spec_failures() {
        let validator = SandboxSpecValidator::static_module(
            br#"(module (func (export "_start") unreachable))"#,
            SpecPolicy::default(),
        )
        .unwrap();
        let report = validator
            .validate(&changes(vec![source(), test_file()]))
            .await
            .unwrap();
        assert!(!report.passed);
        assert!(report.summary.starts_with("spec tests failed in sandbox"));
    }
}
