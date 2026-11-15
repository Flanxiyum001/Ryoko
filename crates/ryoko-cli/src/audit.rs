//! Pre-merge security audits for proposed changes.
//!
//! The secret scan always runs on the proposed file contents and reports only
//! the rule and location, never the matched value. Dependency audits run only
//! when the worktree has the matching manifest and lockfile. A missing audit
//! tool is reported as skipped, not as a clean result; an audit tool that runs
//! but cannot be read fails closed.

use std::io::ErrorKind;
use std::path::Path;
use std::process::Output;
use std::time::Duration;

use ryoko_core::validation::StagedChanges;
use serde_json::Value;
use tokio::process::Command;

const MAX_FINDINGS: usize = 50;
const MAX_SCAN_BYTES: usize = 1_000_000;
const TOOL_TIMEOUT: Duration = Duration::from_secs(120);

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuditFinding {
    pub source: String,
    pub severity: String,
    pub message: String,
}

#[derive(Debug, Default)]
pub struct SecurityAuditReport {
    pub findings: Vec<AuditFinding>,
    pub notes: Vec<String>,
}

impl SecurityAuditReport {
    pub fn passed(&self) -> bool {
        self.findings.is_empty()
    }

    pub fn summary(&self) -> String {
        if !self.passed() {
            return format!("found {} finding(s)", self.findings.len());
        }
        if self.notes.iter().any(|note| note.contains("skipped")) {
            "completed with skipped tools".to_owned()
        } else {
            "passed".to_owned()
        }
    }
}

/// Runs every audit that applies to the worktree. The result fails closed when
/// an audit reports a finding or an audit command runs but cannot be parsed.
pub async fn run_security_audit(root: &Path, changes: &StagedChanges) -> SecurityAuditReport {
    let mut report = SecurityAuditReport::default();
    scan_secrets(changes, &mut report);
    audit_cargo(root, &mut report).await;
    audit_npm(root, &mut report).await;
    if report.findings.len() > MAX_FINDINGS {
        report.findings.truncate(MAX_FINDINGS);
        report
            .notes
            .push(format!("audit findings truncated to {MAX_FINDINGS}"));
    }
    report
}

async fn audit_cargo(root: &Path, report: &mut SecurityAuditReport) {
    if !root.join("Cargo.toml").is_file() || !root.join("Cargo.lock").is_file() {
        report
            .notes
            .push("cargo audit skipped: no Cargo.toml/Cargo.lock at the worktree root".to_owned());
        return;
    }
    match run_tool("cargo", &["audit", "--json"], root).await {
        ToolOutcome::Unavailable => report
            .notes
            .push("cargo audit skipped: cargo-audit is not installed".to_owned()),
        ToolOutcome::Failed(error) => push_finding(
            report,
            "cargo audit",
            "unknown",
            format!("cargo audit could not run: {error}"),
        ),
        ToolOutcome::Output(output) => {
            let stdout = String::from_utf8_lossy(&output.stdout);
            let stderr = String::from_utf8_lossy(&output.stderr);
            match parse_cargo_audit(&stdout) {
                Ok(findings) if !findings.is_empty() => report.findings.extend(findings),
                Ok(_) if output.status.success() => report
                    .notes
                    .push("cargo audit found no vulnerabilities".to_owned()),
                Ok(_) if stderr.contains("no such command") => report
                    .notes
                    .push("cargo audit skipped: cargo-audit is not installed".to_owned()),
                Ok(_) => push_finding(
                    report,
                    "cargo audit",
                    "unknown",
                    format!("cargo audit exited with {}", status_label(&output)),
                ),
                Err(error) => push_finding(
                    report,
                    "cargo audit",
                    "unknown",
                    format!("cargo audit returned unreadable JSON: {error}"),
                ),
            }
        }
    }
}

async fn audit_npm(root: &Path, report: &mut SecurityAuditReport) {
    if !root.join("package.json").is_file() || !root.join("package-lock.json").is_file() {
        report.notes.push(
            "npm audit skipped: no package.json/package-lock.json at the worktree root".to_owned(),
        );
        return;
    }
    match run_tool("npm", &["audit", "--json"], root).await {
        ToolOutcome::Unavailable => report
            .notes
            .push("npm audit skipped: npm is not installed".to_owned()),
        ToolOutcome::Failed(error) => push_finding(
            report,
            "npm audit",
            "unknown",
            format!("npm audit could not run: {error}"),
        ),
        ToolOutcome::Output(output) => {
            let stdout = String::from_utf8_lossy(&output.stdout);
            match parse_npm_audit(&stdout) {
                Ok(findings) if !findings.is_empty() => report.findings.extend(findings),
                Ok(_) if output.status.success() => report
                    .notes
                    .push("npm audit found no vulnerabilities".to_owned()),
                Ok(_) => push_finding(
                    report,
                    "npm audit",
                    "unknown",
                    format!("npm audit exited with {}", status_label(&output)),
                ),
                Err(error) => push_finding(
                    report,
                    "npm audit",
                    "unknown",
                    format!("npm audit returned unreadable JSON: {error}"),
                ),
            }
        }
    }
}

fn status_label(output: &Output) -> String {
    output
        .status
        .code()
        .map(|code| format!("exit code {code}"))
        .unwrap_or_else(|| "a signal".to_owned())
}

fn push_finding(report: &mut SecurityAuditReport, source: &str, severity: &str, message: String) {
    if report.findings.len() >= MAX_FINDINGS {
        return;
    }
    report.findings.push(AuditFinding {
        source: source.to_owned(),
        severity: severity.to_owned(),
        message,
    });
}

enum ToolOutcome {
    Unavailable,
    Failed(String),
    Output(Output),
}

async fn run_tool(program: &str, args: &[&str], root: &Path) -> ToolOutcome {
    let mut process = Command::new(program);
    process
        .args(args)
        .current_dir(root)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true);
    match tokio::time::timeout(TOOL_TIMEOUT, process.output()).await {
        Ok(Ok(output)) => ToolOutcome::Output(output),
        Ok(Err(error)) if error.kind() == ErrorKind::NotFound => ToolOutcome::Unavailable,
        Ok(Err(error)) => ToolOutcome::Failed(error.to_string()),
        Err(_) => ToolOutcome::Failed(format!("timed out after {}s", TOOL_TIMEOUT.as_secs())),
    }
}

fn parse_cargo_audit(json: &str) -> Result<Vec<AuditFinding>, String> {
    let value: Value = serde_json::from_str(json).map_err(|error| error.to_string())?;
    let Some(list) = value
        .pointer("/vulnerabilities/list")
        .and_then(Value::as_array)
    else {
        return Ok(Vec::new());
    };
    Ok(list
        .iter()
        .take(MAX_FINDINGS)
        .map(|item| {
            let advisory = item.get("advisory");
            let field = |name: &str| {
                advisory
                    .and_then(|advisory| advisory.get(name))
                    .and_then(Value::as_str)
                    .unwrap_or("unknown")
            };
            AuditFinding {
                source: "cargo audit".to_owned(),
                severity: field("severity").to_owned(),
                message: format!("{}: {}: {}", field("id"), field("package"), field("title")),
            }
        })
        .collect())
}

fn parse_npm_audit(json: &str) -> Result<Vec<AuditFinding>, String> {
    let value: Value = serde_json::from_str(json).map_err(|error| error.to_string())?;
    let Some(vulnerabilities) = value.get("vulnerabilities").and_then(Value::as_object) else {
        return Ok(Vec::new());
    };
    let mut findings = Vec::new();
    for (name, details) in vulnerabilities {
        let severity = details
            .get("severity")
            .and_then(Value::as_str)
            .unwrap_or("unknown");
        let via = details
            .get("via")
            .and_then(Value::as_array)
            .map(|items| {
                items
                    .iter()
                    .filter_map(|item| {
                        item.get("title")
                            .and_then(Value::as_str)
                            .or_else(|| item.as_str())
                    })
                    .collect::<Vec<_>>()
                    .join(", ")
            })
            .unwrap_or_default();
        let message = if via.is_empty() {
            format!("{name} ({severity})")
        } else {
            format!("{name} ({severity}): {via}")
        };
        findings.push(AuditFinding {
            source: "npm audit".to_owned(),
            severity: severity.to_owned(),
            message,
        });
        if findings.len() >= MAX_FINDINGS {
            break;
        }
    }
    Ok(findings)
}

fn scan_secrets(changes: &StagedChanges, report: &mut SecurityAuditReport) {
    for change in &changes.files {
        if change.content.len() > MAX_SCAN_BYTES {
            report.notes.push(format!(
                "secret scan skipped {}: file exceeds {} bytes",
                change.path, MAX_SCAN_BYTES
            ));
            continue;
        }
        for (index, line) in change.content.lines().enumerate() {
            for rule in secret_rules(line) {
                if report.findings.len() >= MAX_FINDINGS {
                    report
                        .notes
                        .push(format!("secret scan stopped after {MAX_FINDINGS} findings"));
                    return;
                }
                report.findings.push(AuditFinding {
                    source: "secret scan".to_owned(),
                    severity: "high".to_owned(),
                    message: format!(
                        "{}:{}: possible {rule} (value redacted)",
                        change.path,
                        index + 1
                    ),
                });
            }
        }
    }
    report.notes.push(format!(
        "secret scan checked {} proposed file(s)",
        changes.files.len()
    ));
}

fn secret_rules(line: &str) -> Vec<&'static str> {
    let mut rules = Vec::new();
    if line.contains("BEGIN") && line.contains("PRIVATE KEY-----") {
        rules.push("private key");
    }
    if ["ghp_", "gho_", "ghu_", "ghs_", "ghr_", "github_pat_"]
        .iter()
        .any(|prefix| has_prefixed_token(line, prefix, 20))
    {
        rules.push("GitHub token");
    }
    if line.match_indices("AKIA").any(|(start, _)| {
        line[start + 4..]
            .chars()
            .take_while(|character| character.is_ascii_uppercase() || character.is_ascii_digit())
            .count()
            == 16
    }) {
        rules.push("AWS access key");
    }
    if ["xoxb-", "xoxp-", "xoxa-", "xoxs-"]
        .iter()
        .any(|prefix| has_prefixed_token(line, prefix, 10))
    {
        rules.push("Slack token");
    }
    if has_prefixed_token(line, "sk_live_", 10) {
        rules.push("Stripe secret key");
    }
    if assigned_secret(line) {
        rules.push("assigned secret");
    }
    rules
}

fn has_prefixed_token(line: &str, prefix: &str, min_length: usize) -> bool {
    line.match_indices(prefix).any(|(start, _)| {
        line[start + prefix.len()..]
            .chars()
            .take_while(|character| {
                character.is_ascii_alphanumeric() || *character == '_' || *character == '-'
            })
            .count()
            >= min_length
    })
}

fn assigned_secret(line: &str) -> bool {
    const KEYS: [&str; 9] = [
        "password",
        "passwd",
        "secret",
        "api_key",
        "apikey",
        "access_token",
        "auth_token",
        "private_key",
        "client_secret",
    ];
    let lower = line.to_ascii_lowercase();
    if !KEYS.iter().any(|key| lower.contains(key)) {
        return false;
    }
    let Some((_, raw_value)) = line.split_once('=').or_else(|| line.split_once(':')) else {
        return false;
    };
    let raw_value = raw_value.trim();
    let quoted = raw_value.starts_with('"') || raw_value.starts_with('\'');
    let value = raw_value
        .trim_end_matches([';', ','])
        .trim()
        .trim_matches(|character| character == '"' || character == '\'');
    if value.len() < 8 || is_placeholder(value) {
        return false;
    }
    // References such as `std::env::var("API_KEY")` are not literals.
    quoted
        || value
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || "_-.".contains(character))
}

fn is_placeholder(value: &str) -> bool {
    let lower = value.to_ascii_lowercase();
    value.starts_with("${")
        || value.starts_with('<')
        || value.starts_with("your_")
        || value.starts_with("your-")
        || matches!(
            lower.as_str(),
            "changeme"
                | "change-me"
                | "example"
                | "password"
                | "secret"
                | "test"
                | "dummy"
                | "redacted"
                | "placeholder"
        )
}

#[cfg(test)]
mod tests {
    use super::*;
    use ryoko_core::ProposedChange;
    use uuid::Uuid;

    fn staged(files: &[(&str, &str)]) -> StagedChanges {
        StagedChanges::new(
            Uuid::new_v4(),
            files
                .iter()
                .map(|(path, content)| ProposedChange::new(*path, *content))
                .collect(),
        )
    }

    #[tokio::test]
    async fn finds_and_redacts_secrets() {
        // Built at runtime so the source does not contain a token-shaped string
        // that repository secret scanners would flag.
        let token = format!("ghp_{}", "a".repeat(36));
        let content = format!("const TOKEN: &str = \"{token}\";\npassword = \"hunter2secret\";\n");
        let dir = tempfile::TempDir::new().unwrap();
        let report =
            run_security_audit(dir.path(), &staged(&[("src/config.rs", content.as_str())])).await;

        assert!(!report.passed());
        assert!(
            report
                .findings
                .iter()
                .any(|finding| finding.message.contains("GitHub token"))
        );
        assert!(
            report
                .findings
                .iter()
                .any(|finding| finding.message.contains("assigned secret"))
        );
        for finding in &report.findings {
            assert!(!finding.message.contains(&token));
            assert!(!finding.message.contains("hunter2secret"));
        }
    }

    #[tokio::test]
    async fn accepts_placeholders_and_clean_files() {
        let dir = tempfile::TempDir::new().unwrap();
        let report = run_security_audit(
            dir.path(),
            &staged(&[(
                "src/config.rs",
                "const KEY: &str = \"${API_KEY}\";\npassword = \"changeme\";\nlet api_key = std::env::var(\"API_KEY\")?;\n",
            )]),
        )
        .await;
        assert!(report.passed(), "{:?}", report.findings);
    }

    #[test]
    fn parses_cargo_audit_findings() {
        let json = r#"{"vulnerabilities":{"list":[{"advisory":{"id":"RUSTSEC-1","package":"demo","title":"bad","severity":"high"}}]}}"#;
        let findings = parse_cargo_audit(json).unwrap();
        assert_eq!(findings.len(), 1);
        assert!(findings[0].message.contains("RUSTSEC-1"));
        assert_eq!(findings[0].severity, "high");
    }

    #[test]
    fn parses_npm_audit_findings() {
        let json = r#"{"vulnerabilities":{"left-pad":{"severity":"moderate","via":["prototype pollution"]}}}"#;
        let findings = parse_npm_audit(json).unwrap();
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].severity, "moderate");
        assert!(findings[0].message.contains("prototype pollution"));
    }
}
