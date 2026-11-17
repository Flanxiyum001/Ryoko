//! Benchmark and size regression gates for pre-flight validation.
//!
//! A gate runs one operator-supplied command inside the same sandbox as the
//! project's test command, reads one named metric from its output and compares
//! it with a recorded baseline. The comparison fails closed: a missing or
//! unparsable baseline is reported, never treated as "no regression".

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

use ryoko_sandbox::{BuildSandbox, BuildSandboxError};

const MAX_BASELINE_BYTES: u64 = 64 * 1024;
const MAX_METRICS: usize = 64;

/// Operator configuration for one gate. All of it is supplied on the command
/// line; nothing here can be influenced by an agent.
#[derive(Debug, Clone, PartialEq)]
pub struct PerfConfig {
    /// Command that prints `<metric>=<number>` on stdout.
    pub command: String,
    /// Metric name to read, for example `latency_ms` or `bundle_bytes`.
    pub metric: String,
    /// Allowed increase, in percent. Zero allows no regression at all.
    pub max_regression_pct: f64,
    /// Baseline file, relative to the project root.
    pub baseline: PathBuf,
    /// Record the current value instead of comparing, creating or updating the
    /// baseline. Intended for the operator's own branch, not for a PR gate.
    pub update_baseline: bool,
}

/// Resolves an operator-supplied baseline path inside the worktree. Unlike the
/// agent-facing path validator this permits `.ryoko/` state files, because the
/// path comes from the command line; it still cannot leave the worktree.
fn safe_relative(path: &str) -> Result<PathBuf, String> {
    let path = Path::new(path);
    let invalid = || "perf baseline path must be a safe project-relative path".to_owned();
    if path.is_absolute() || path.as_os_str().is_empty() || path.as_os_str().len() > 512 {
        return Err(invalid());
    }
    for component in path.components() {
        match component {
            std::path::Component::Normal(part) => {
                let part = part.to_string_lossy();
                if part.is_empty() || part.contains('\\') || part.chars().any(char::is_control) {
                    return Err(invalid());
                }
            }
            _ => return Err(invalid()),
        }
    }
    Ok(path.to_path_buf())
}

impl PerfConfig {
    /// Rejects configurations that could not produce a trustworthy gate.
    pub fn validate(&self) -> Result<(), String> {
        if self.command.trim().is_empty() {
            return Err("perf command cannot be empty".to_owned());
        }
        if self.metric.trim().is_empty()
            || self.metric.len() > 64
            || !self
                .metric
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-' | b'.'))
        {
            return Err("perf metric must be a simple name like latency_ms".to_owned());
        }
        if !self.max_regression_pct.is_finite() || self.max_regression_pct < 0.0 {
            return Err("perf max regression percent must be a non-negative number".to_owned());
        }
        safe_relative(&self.baseline.to_string_lossy())?;
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum PerfOutcome {
    Passed {
        current: f64,
        baseline: Option<f64>,
        delta_pct: Option<f64>,
    },
    Blocked {
        current: f64,
        baseline: f64,
        delta_pct: f64,
        limit: f64,
    },
    /// The gate could not produce a comparable number. Always a failure.
    Unusable(String),
}

impl PerfOutcome {
    pub fn passed(&self) -> bool {
        matches!(self, Self::Passed { .. })
    }

    /// One log line per outcome, without the raw command output.
    pub fn describe(&self, metric: &str) -> Vec<String> {
        match self {
            Self::Passed {
                current,
                baseline,
                delta_pct,
            } => {
                let mut lines = vec![format!("perf gate `{metric}`: {current}")];
                match (baseline, delta_pct) {
                    (Some(baseline), Some(delta)) => lines.push(format!(
                        "perf gate `{metric}`: baseline {baseline}, change {delta:+.2}%"
                    )),
                    _ => lines.push(format!("perf gate `{metric}`: baseline recorded")),
                }
                lines
            }
            Self::Blocked {
                current,
                baseline,
                delta_pct,
                limit,
            } => vec![format!(
                "perf gate `{metric}`: {current} vs baseline {baseline} is {delta_pct:+.2}%, over the {limit:.2}% limit"
            )],
            Self::Unusable(reason) => vec![format!("perf gate `{metric}`: {reason}")],
        }
    }
}

/// Reads `<metric>=<number>` from the last matching line. The value must be a
/// finite, non-negative number; anything else is not a measurement.
pub fn parse_metric(output: &str, metric: &str) -> Option<f64> {
    let mut found = None;
    for line in output.lines() {
        let line = line.trim();
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        if key.trim() != metric {
            continue;
        }
        let value = value.trim();
        let Ok(number) = value.parse::<f64>() else {
            continue;
        };
        if number.is_finite() && number >= 0.0 {
            found = Some(number);
        }
    }
    found
}

fn load_baseline(path: &Path) -> Result<BTreeMap<String, f64>, String> {
    let metadata = match std::fs::metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(BTreeMap::new()),
        Err(error) => return Err(format!("cannot inspect perf baseline: {error}")),
    };
    if !metadata.is_file() || metadata.len() > MAX_BASELINE_BYTES {
        return Err("perf baseline must be a regular file under 64 KiB".to_owned());
    }
    let text = std::fs::read_to_string(path)
        .map_err(|error| format!("cannot read perf baseline: {error}"))?;
    let parsed: BTreeMap<String, f64> = serde_json::from_str(&text)
        .map_err(|error| format!("perf baseline is not a name-to-number JSON object: {error}"))?;
    if parsed.len() > MAX_METRICS {
        return Err(format!(
            "perf baseline holds more than {MAX_METRICS} metrics"
        ));
    }
    if parsed
        .values()
        .any(|value| !value.is_finite() || *value < 0.0)
    {
        return Err("perf baseline holds a non-finite or negative value".to_owned());
    }
    Ok(parsed)
}

fn store_baseline(path: &Path, values: &BTreeMap<String, f64>) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|error| format!("cannot create perf baseline directory: {error}"))?;
    }
    let text = serde_json::to_string_pretty(values)
        .map_err(|error| format!("cannot encode perf baseline: {error}"))?;
    std::fs::write(path, text + "\n")
        .map_err(|error| format!("cannot write perf baseline: {error}"))
}

fn regression_pct(current: f64, baseline: f64) -> f64 {
    if baseline == 0.0 {
        if current == 0.0 { 0.0 } else { f64::INFINITY }
    } else {
        (current - baseline) / baseline * 100.0
    }
}

/// Runs the gate. `root` must be the isolated worktree, so the measurement
/// describes the proposed change rather than the developer's checkout.
pub async fn run_gate(
    sandbox: &BuildSandbox,
    root: &Path,
    config: &PerfConfig,
    timeout: Duration,
) -> Result<PerfOutcome, String> {
    config.validate()?;
    let output = match sandbox.run(&config.command, root, timeout).await {
        Ok(output) => output,
        Err(BuildSandboxError::Timeout(seconds)) => {
            return Ok(PerfOutcome::Unusable(format!(
                "perf command timed out after {seconds}s"
            )));
        }
        Err(error) => {
            return Ok(PerfOutcome::Unusable(format!(
                "perf command could not run: {error}"
            )));
        }
    };
    if !output.status.success() {
        let code = output
            .status
            .code()
            .map(|code| code.to_string())
            .unwrap_or_else(|| "signal".to_owned());
        return Ok(PerfOutcome::Unusable(format!(
            "perf command failed (exit {code})"
        )));
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    let Some(current) = parse_metric(&stdout, &config.metric) else {
        return Ok(PerfOutcome::Unusable(format!(
            "command printed no `{}=<number>` line",
            config.metric
        )));
    };

    let path = root.join(safe_relative(&config.baseline.to_string_lossy())?);
    let mut baselines = load_baseline(&path)?;

    if config.update_baseline {
        baselines.insert(config.metric.clone(), current);
        store_baseline(&path, &baselines)?;
        return Ok(PerfOutcome::Passed {
            current,
            baseline: Some(current),
            delta_pct: Some(0.0),
        });
    }

    let Some(baseline) = baselines.get(&config.metric).copied() else {
        return Ok(PerfOutcome::Unusable(format!(
            "no recorded baseline for `{}`; record one with --perf-update-baseline",
            config.metric
        )));
    };
    let delta = regression_pct(current, baseline);
    if delta > config.max_regression_pct {
        return Ok(PerfOutcome::Blocked {
            current,
            baseline,
            delta_pct: delta,
            limit: config.max_regression_pct,
        });
    }
    Ok(PerfOutcome::Passed {
        current,
        baseline: Some(baseline),
        delta_pct: Some(delta),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use ryoko_sandbox::BuildBackend;
    use tempfile::TempDir;

    fn host() -> BuildSandbox {
        BuildSandbox::new(BuildBackend::UnsafeHost, None)
    }

    fn config(metric: &str, limit: f64) -> PerfConfig {
        PerfConfig {
            command: format!("printf '{metric}=100\\n'"),
            metric: metric.to_owned(),
            max_regression_pct: limit,
            baseline: PathBuf::from(".ryoko/perf-baseline.json"),
            update_baseline: false,
        }
    }

    #[test]
    fn parses_the_last_matching_metric_line() {
        let output = "building benchmark\nlatency_ms=12.5\nlatency_ms=9.75\nnoise=3\n";
        assert_eq!(parse_metric(output, "latency_ms"), Some(9.75));
        assert_eq!(parse_metric(output, "bundle_bytes"), None);
        assert_eq!(parse_metric("latency_ms=abc\n", "latency_ms"), None);
        assert_eq!(parse_metric("latency_ms=-1\n", "latency_ms"), None);
        assert_eq!(parse_metric("latency_ms=nan\n", "latency_ms"), None);
        assert_eq!(parse_metric("latency_ms = 4\n", "latency_ms"), Some(4.0));
    }

    #[test]
    fn rejects_unsafe_or_ambiguous_configuration() {
        let mut bad = config("latency_ms", 5.0);
        bad.baseline = PathBuf::from("../outside.json");
        assert!(bad.validate().is_err());
        let mut bad = config("latency_ms", 5.0);
        bad.baseline = PathBuf::from("/etc/perf.json");
        assert!(bad.validate().is_err());
        let mut bad = config("latency_ms", -1.0);
        bad.command = "true".into();
        assert!(bad.validate().is_err());
        let mut bad = config("latency_ms", 5.0);
        bad.metric = "latency ms".into();
        assert!(bad.validate().is_err());
        assert!(config("latency_ms", 5.0).validate().is_ok());
    }

    #[test]
    fn a_zero_baseline_blocks_any_new_work() {
        assert_eq!(regression_pct(0.0, 0.0), 0.0);
        assert!(regression_pct(1.0, 0.0).is_infinite());
    }

    #[tokio::test]
    async fn records_a_baseline_then_blocks_a_regression() {
        let dir = TempDir::new().unwrap();
        let mut record = config("latency_ms", 5.0);
        record.update_baseline = true;
        let outcome = run_gate(&host(), dir.path(), &record, Duration::from_secs(5))
            .await
            .unwrap();
        assert!(outcome.passed(), "{outcome:?}");
        let stored = std::fs::read_to_string(dir.path().join(".ryoko/perf-baseline.json")).unwrap();
        assert!(stored.contains("\"latency_ms\": 100"));

        // Within the threshold: +5% is allowed by a 5% limit.
        let mut within = config("latency_ms", 5.0);
        within.command = "printf 'latency_ms=105\\n'".into();
        let outcome = run_gate(&host(), dir.path(), &within, Duration::from_secs(5))
            .await
            .unwrap();
        assert!(outcome.passed(), "{outcome:?}");

        // Over the threshold: blocked, and the log names both numbers.
        let mut over = config("latency_ms", 5.0);
        over.command = "printf 'latency_ms=120\\n'".into();
        let outcome = run_gate(&host(), dir.path(), &over, Duration::from_secs(5))
            .await
            .unwrap();
        match &outcome {
            PerfOutcome::Blocked {
                current,
                baseline,
                delta_pct,
                limit,
            } => {
                assert_eq!(*current, 120.0);
                assert_eq!(*baseline, 100.0);
                assert!((delta_pct - 20.0).abs() < 1e-9);
                assert_eq!(*limit, 5.0);
            }
            other => panic!("expected a blocked gate, got {other:?}"),
        }
        assert!(!outcome.passed());
        assert!(outcome.describe("latency_ms")[0].contains("over the 5.00% limit"));
    }

    #[tokio::test]
    async fn an_improvement_passes() {
        let dir = TempDir::new().unwrap();
        let mut record = config("bundle_bytes", 1.0);
        record.update_baseline = true;
        run_gate(&host(), dir.path(), &record, Duration::from_secs(5))
            .await
            .unwrap();
        let mut faster = config("bundle_bytes", 1.0);
        faster.command = "printf 'bundle_bytes=80\\n'".into();
        let outcome = run_gate(&host(), dir.path(), &faster, Duration::from_secs(5))
            .await
            .unwrap();
        assert!(outcome.passed(), "{outcome:?}");
    }

    #[tokio::test]
    async fn a_missing_baseline_fails_closed() {
        let dir = TempDir::new().unwrap();
        let outcome = run_gate(
            &host(),
            dir.path(),
            &config("latency_ms", 5.0),
            Duration::from_secs(5),
        )
        .await
        .unwrap();
        match &outcome {
            PerfOutcome::Unusable(reason) => assert!(reason.contains("no recorded baseline")),
            other => panic!("missing baseline must not pass: {other:?}"),
        }
        assert!(!outcome.passed());
    }

    #[tokio::test]
    async fn missing_metric_and_failing_command_are_unusable() {
        let dir = TempDir::new().unwrap();
        let mut record = config("latency_ms", 5.0);
        record.update_baseline = true;
        run_gate(&host(), dir.path(), &record, Duration::from_secs(5))
            .await
            .unwrap();

        let mut silent = config("latency_ms", 5.0);
        silent.command = "true".into();
        let outcome = run_gate(&host(), dir.path(), &silent, Duration::from_secs(5))
            .await
            .unwrap();
        assert!(
            matches!(&outcome, PerfOutcome::Unusable(reason) if reason.contains("no `latency_ms"))
        );

        let mut broken = config("latency_ms", 5.0);
        broken.command = "exit 3".into();
        let outcome = run_gate(&host(), dir.path(), &broken, Duration::from_secs(5))
            .await
            .unwrap();
        assert!(matches!(&outcome, PerfOutcome::Unusable(reason) if reason.contains("exit 3")));
    }

    #[tokio::test]
    async fn an_unreadable_baseline_is_an_error_not_a_pass() {
        let dir = TempDir::new().unwrap();
        std::fs::create_dir_all(dir.path().join(".ryoko")).unwrap();
        std::fs::write(dir.path().join(".ryoko/perf-baseline.json"), "not json").unwrap();
        let error = run_gate(
            &host(),
            dir.path(),
            &config("latency_ms", 5.0),
            Duration::from_secs(5),
        )
        .await
        .unwrap_err();
        assert!(
            error.contains("not a name-to-number JSON object"),
            "{error}"
        );
    }
}
