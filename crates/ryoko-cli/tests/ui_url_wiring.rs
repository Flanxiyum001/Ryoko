//! CLI-level coverage for `--ui-url` on `run`.
//!
//! The in-process tests in `main.rs` stop at the parsed arguments, so a flag that
//! parses but never reaches the capture configuration would still pass. These
//! spawn the real binary against a scratch checkout and assert on its exit status
//! and message instead.
//!
//! `run` configures the capture before it allocates a worktree or calls a model,
//! so an accepted loopback origin stops at the next operator check
//! (`--max-concurrency`) rather than touching Git or the network. `review` builds
//! the same capture behind a published worktree session, which a scratch checkout
//! cannot provide, so that path stays covered in-process.

use std::path::Path;
use std::process::{Command, Output};

use tempfile::TempDir;

const BINARY: &str = env!("CARGO_BIN_EXE_ryoko");

/// Runs `ryoko run` in `repo` with everything the capture configuration needs up
/// front, so only the UI flags under test decide the outcome.
fn run_in(repo: &Path, arguments: &[&str]) -> Output {
    Command::new(BINARY)
        .arg("run")
        .arg("ship the change")
        .arg("--repo")
        .arg(repo)
        .arg("--model")
        .arg("openai/gpt-4.1")
        .arg("--check-command")
        .arg("true")
        // Never contacted: sandbox resolution only validates the reference, and
        // the UI flags are checked straight after it.
        .arg("--sandbox-image")
        .arg("ryoko/sandbox:test")
        .args(arguments)
        .output()
        .expect("spawn the ryoko binary")
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

#[test]
fn ui_url_without_a_dev_command_is_rejected_by_the_binary() {
    let repo = TempDir::new().unwrap();
    let output = run_in(repo.path(), &["--ui-url", "http://localhost:3000"]);
    let stderr = stderr(&output);
    assert_eq!(output.status.code(), Some(1), "{stderr}");
    assert!(
        stderr.contains("--ui-url requires --ui-command"),
        "expected the capture error, got: {stderr}"
    );
}

#[test]
fn a_loopback_origin_passes_capture_validation() {
    let repo = TempDir::new().unwrap();
    let output = run_in(
        repo.path(),
        &[
            "--ui-command",
            "npm run dev",
            "--ui-route",
            "/pricing",
            "--ui-url",
            "http://[::1]:8080",
            // The check after the capture is configured: reaching it proves the
            // origin was accepted instead of rejected up front.
            "--team",
            "--max-concurrency",
            "9",
        ],
    );
    let stderr = stderr(&output);
    assert_eq!(output.status.code(), Some(1), "{stderr}");
    assert!(
        !stderr.contains("--ui-url"),
        "the origin should have been accepted, got: {stderr}"
    );
    assert!(
        stderr.contains("--max-concurrency must be between 1 and 4"),
        "expected the later run validation to fail, got: {stderr}"
    );
}

#[test]
fn a_non_loopback_origin_is_rejected_before_the_capture_runs() {
    let repo = TempDir::new().unwrap();
    let output = run_in(
        repo.path(),
        &[
            "--ui-command",
            "npm run dev",
            "--ui-url",
            "http://example.com:3000",
            "--team",
            "--max-concurrency",
            "9",
        ],
    );
    let stderr = stderr(&output);
    assert_eq!(output.status.code(), Some(1), "{stderr}");
    assert!(
        stderr.contains("must point at a loopback host"),
        "expected the origin to be rejected, got: {stderr}"
    );
    assert!(
        !stderr.contains("--max-concurrency"),
        "the origin should be rejected before the later checks, got: {stderr}"
    );
}
