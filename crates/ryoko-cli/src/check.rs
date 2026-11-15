//! Pre-flight validation for isolated worktrees.

use std::path::Path;
use std::process::Output;
use std::time::{Duration, Instant};

use async_trait::async_trait;
use ryoko_core::validation::{ChangeValidator, StagedChanges, ValidationReport};
use ryoko_core::CoreResult;
use ryoko_sandbox::{BuildBackend, BuildSandbox, SpecPolicy};

/// Runs a project's own build/test command in the task's isolated worktree.
/// Spec evidence is checked first, so a source-only proposal is rejected before
/// any command is executed.
#[derive(Debug, Clone)]
pub struct CommandValidator {
    command: String,
    policy: SpecPolicy,
    timeout: Duration,
    sandbox: BuildSandbox,
}

impl CommandValidator {
    pub fn new(command: impl Into<String>, policy: SpecPolicy, timeout: Duration) -> Self {
        Self {
            command: command.into(),
            policy,
            timeout: timeout.max(Duration::from_secs(1)),
            sandbox: BuildSandbox::new(BuildBackend::Auto, None),
        }
    }

    /// Isolation selection is an operator decision, never agent-controlled.
    pub fn with_sandbox(mut self, sandbox: BuildSandbox) -> Self {
        self.sandbox = sandbox;
        self
    }
}

#[async_trait]
impl ChangeValidator for CommandValidator {
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
        let Some(root) = changes.root.as_deref() else {
            return Ok(ValidationReport::failed(
                "pre-flight check failed",
                vec!["the staged changes have no worktree root".to_owned()],
                started.elapsed(),
            ));
        };
        if !root.is_dir() {
            return Ok(ValidationReport::failed(
                "pre-flight check failed",
                vec![format!("worktree root does not exist: {}", root.display())],
                started.elapsed(),
            ));
        }

        let output = match self.sandbox.run(&self.command, root, self.timeout).await {
            Ok(output) => output,
            Err(error) => {
                return Ok(ValidationReport::failed(
                    "pre-flight check could not run",
                    vec![error.to_string()],
                    started.elapsed(),
                ));
            }
        };
        let mut logs = summarize_output(&output);
        if output.status.success() {
            logs.insert(
                0,
                format!(
                    "spec test files: {}; command: {}",
                    evidence.test_files.join(", "),
                    self.command
                ),
            );
            let audit = crate::audit::run_security_audit(root, changes).await;
            logs.push(format!("security audit: {}", audit.summary()));
            logs.extend(
                audit
                    .notes
                    .iter()
                    .map(|note| format!("security audit: {note}")),
            );
            if !audit.passed() {
                for finding in &audit.findings {
                    logs.push(format!(
                        "security audit: {} [{}] {}",
                        finding.source, finding.severity, finding.message
                    ));
                }
                return Ok(ValidationReport::failed(
                    format!("security audit failed for {}", self.command),
                    logs,
                    started.elapsed(),
                ));
            }
            Ok(ValidationReport::passed(
                format!(
                    "pre-flight passed: {}; security audit: {}",
                    self.command,
                    audit.summary()
                ),
                logs,
                started.elapsed(),
            ))
        } else {
            let status = output
                .status
                .code()
                .map(|code| code.to_string())
                .unwrap_or_else(|| "signal".to_owned());
            Ok(ValidationReport::failed(
                format!("pre-flight failed: {} (exit {status})", self.command),
                logs,
                started.elapsed(),
            ))
        }
    }
}

fn summarize_output(output: &Output) -> Vec<String> {
    let mut lines = Vec::new();
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    for line in stdout.lines().chain(stderr.lines()) {
        let line = line.trim_end();
        if !line.is_empty() {
            lines.push(line.to_owned());
        }
    }
    if lines.is_empty() {
        return vec!["command produced no output".to_owned()];
    }
    if lines.len() > 120 {
        lines = lines.split_off(lines.len() - 120);
        lines.insert(0, "... earlier output truncated ...".to_owned());
    }
    let mut total = 0;
    let mut kept = Vec::new();
    for line in lines.iter().rev() {
        total += line.len() + 1;
        if total > 16_000 {
            kept.push("... output truncated ...".to_owned());
            break;
        }
        kept.push(line.clone());
    }
    kept.reverse();
    kept
}

/// Picks the conventional test command for a project. An explicit
/// `--check-command` always wins; this is only a convenience default.
pub fn detect_check_command(root: &Path) -> Option<&'static str> {
    if root.join("Cargo.toml").is_file() {
        Some("cargo test --workspace --all-targets")
    } else if root.join("package.json").is_file() {
        Some("npm test")
    } else if root.join("pyproject.toml").is_file() {
        Some("python -m pytest")
    } else if root.join("go.mod").is_file() {
        Some("go test ./...")
    } else if root.join("Makefile").is_file() {
        Some("make test")
    } else {
        None
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use ryoko_core::{AgentRole, ProposedChange, Task};
    use tempfile::TempDir;

    fn changes(root: &Path, with_tests: bool) -> StagedChanges {
        let task = Task::new("check", AgentRole::SeniorDeveloper);
        let mut files = vec![ProposedChange::new(
            "src/lib.rs",
            "pub fn value() -> bool { true }\n",
        )];
        if with_tests {
            files.push(ProposedChange::new(
                "tests/lib.rs",
                "#[test]\nfn value() { assert!(true); }\n",
            ));
        }
        StagedChanges::new(task.id, files).with_root(root)
    }

    #[tokio::test]
    async fn passing_command_is_a_passed_report() {
        let dir = TempDir::new().unwrap();
        let validator =
            CommandValidator::new("true", SpecPolicy::default(), Duration::from_secs(5))
                .with_sandbox(BuildSandbox::new(BuildBackend::UnsafeHost, None));
        let report = validator
            .validate(&changes(dir.path(), true))
            .await
            .unwrap();
        assert!(report.passed);
        assert!(report.summary.contains("pre-flight passed"));
        assert!(report.logs[0].contains("tests/lib.rs"));
    }

    #[tokio::test]
    async fn failing_command_captures_compiler_output() {
        let dir = TempDir::new().unwrap();
        let validator = CommandValidator::new(
            "echo 'error[E0425]: missing' >&2; exit 7",
            SpecPolicy::default(),
            Duration::from_secs(5),
        )
        .with_sandbox(BuildSandbox::new(BuildBackend::UnsafeHost, None));
        let report = validator
            .validate(&changes(dir.path(), true))
            .await
            .unwrap();
        assert!(!report.passed);
        assert!(report.summary.contains("exit 7"));
        assert!(report.logs.iter().any(|line| line.contains("E0425")));
    }

    #[tokio::test]
    async fn passing_command_still_fails_on_secret() {
        let dir = TempDir::new().unwrap();
        // Built at runtime so the source does not contain a token-shaped string
        // that repository secret scanners would flag.
        let token = format!("ghp_{}", "a".repeat(36));
        let task = Task::new("check", AgentRole::SeniorDeveloper);
        let staged = StagedChanges::new(
            task.id,
            vec![
                ProposedChange::new(
                    "src/lib.rs",
                    format!("pub const TOKEN: &str = \"{token}\";\n"),
                ),
                ProposedChange::new("tests/lib.rs", "#[test]\nfn value() { assert!(true); }\n"),
            ],
        )
        .with_root(dir.path());
        let validator =
            CommandValidator::new("true", SpecPolicy::default(), Duration::from_secs(5))
                .with_sandbox(BuildSandbox::new(BuildBackend::UnsafeHost, None));
        let report = validator.validate(&staged).await.unwrap();
        assert!(!report.passed);
        assert!(report.summary.contains("security audit"));
        assert!(report.logs.iter().any(|line| line.contains("GitHub token")));
        assert!(!report.logs.iter().any(|line| line.contains(&token)));
    }

    #[tokio::test]
    async fn source_without_tests_never_runs_the_command() {
        let dir = TempDir::new().unwrap();
        let validator = CommandValidator::new(
            "sh -c 'touch should-not-exist'",
            SpecPolicy::default(),
            Duration::from_secs(5),
        );
        let report = validator
            .validate(&changes(dir.path(), false))
            .await
            .unwrap();
        assert!(!report.passed);
        assert!(report.summary.contains("spec check failed"));
        assert!(!dir.path().join("should-not-exist").exists());
    }
}
