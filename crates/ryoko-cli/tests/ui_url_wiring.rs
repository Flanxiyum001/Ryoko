//! CLI-level coverage for `--ui-url` on `run` and `review`.
//!
//! The in-process tests in `main.rs` stop at the parsed arguments, so a flag that
//! parses but never reaches the capture configuration would still pass. These
//! spawn the real binary against a scratch checkout and assert on its exit status
//! and message instead.
//!
//! `run` configures the capture before it allocates a worktree or calls a model,
//! so an accepted loopback origin stops at the next operator check
//! (`--max-concurrency`) rather than touching Git or the network. `review` only
//! reaches the same configuration once a published worktree session exists, so
//! its cases build one through `GitWorktreeManager` — the API `ryoko run` itself
//! uses — and then run the binary against that checkout.

use std::path::Path;
use std::process::{Command, Output};

use ryoko_worktree::GitWorktreeManager;
use tempfile::TempDir;
use uuid::Uuid;

const BINARY: &str = env!("CARGO_BIN_EXE_ryoko");

fn git(cwd: &Path, arguments: &[&str]) {
    let output = Command::new("git")
        .current_dir(cwd)
        .args(arguments)
        .env("GIT_AUTHOR_NAME", "Ryoko Test")
        .env("GIT_AUTHOR_EMAIL", "test@example.com")
        .env("GIT_COMMITTER_NAME", "Ryoko Test")
        .env("GIT_COMMITTER_EMAIL", "test@example.com")
        .output()
        .expect("git is required to publish a review fixture");
    assert!(
        output.status.success(),
        "git {arguments:?} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

/// A checkout with one published worktree session, which is what `review`
/// selects before it configures the capture. The session is created and
/// published through the manager rather than by writing state directly, so the
/// fixture cannot drift from what the binary reads back.
async fn published_checkout() -> (TempDir, Uuid) {
    let repo = TempDir::new().unwrap();
    git(repo.path(), &["init", "-b", "main"]);
    git(repo.path(), &["config", "user.name", "Ryoko Test"]);
    git(repo.path(), &["config", "user.email", "test@example.com"]);
    std::fs::write(repo.path().join("README.md"), "# project\n").unwrap();
    git(repo.path(), &["add", "-A"]);
    git(repo.path(), &["commit", "-m", "initial"]);

    let manager = GitWorktreeManager::new(repo.path());
    let session = manager
        .create_worktree("review-test", Uuid::new_v4())
        .await
        .unwrap();
    std::fs::write(session.path.join("feature.rs"), "pub fn feature() {}\n").unwrap();
    manager
        .publish_worktree(&session, "add the feature")
        .await
        .unwrap();
    (repo, session.id)
}

fn review_in(repo: &Path, arguments: &[&str]) -> Output {
    Command::new(BINARY)
        .arg("review")
        .arg("--repo")
        .arg(repo)
        .arg("--check-command")
        .arg("true")
        // The fixture's check command is the host's own `true`. Nothing else
        // in these cases runs untrusted code.
        .arg("--unsafe-host-check")
        .args(arguments)
        // Closed stdin makes the review prompt answer `q`, so an accepted run
        // keeps the branch and exits instead of waiting for an operator.
        .stdin(std::process::Stdio::null())
        .output()
        .expect("spawn the ryoko binary")
}

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

#[tokio::test]
async fn review_ui_url_without_a_dev_command_is_rejected_by_the_binary() {
    let (repo, _task) = published_checkout().await;
    let output = review_in(repo.path(), &["--ui-url", "http://localhost:3000"]);
    let stderr = stderr(&output);
    assert_eq!(output.status.code(), Some(1), "{stderr}");
    assert!(
        stderr.contains("--ui-url requires --ui-command"),
        "expected the capture error, got: {stderr}"
    );
}

#[tokio::test]
async fn review_accepts_a_loopback_origin() {
    let (repo, task) = published_checkout().await;
    let task = task.to_string();
    let output = review_in(
        repo.path(),
        &[
            "--task",
            &task,
            "--ui-command",
            "npm run dev",
            "--ui-route",
            "/pricing",
            "--ui-url",
            "http://localhost:3000",
        ],
    );
    let stderr = stderr(&output);
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    assert_eq!(output.status.code(), Some(0), "{stderr}");
    assert!(
        !stderr.contains("--ui-url") && !stderr.contains("loopback host"),
        "the origin should have been accepted, got: {stderr}"
    );
    assert!(
        stdout.contains("Reviewing ryoko/review-test/"),
        "expected the review to start, got: {stdout}"
    );
}

#[tokio::test]
async fn review_rejects_a_non_loopback_origin() {
    let (repo, task) = published_checkout().await;
    let task = task.to_string();
    let output = review_in(
        repo.path(),
        &[
            "--task",
            &task,
            "--ui-command",
            "npm run dev",
            "--ui-url",
            "http://example.com:3000",
        ],
    );
    let stderr = stderr(&output);
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    assert_eq!(output.status.code(), Some(1), "{stderr}");
    assert!(
        stderr.contains("must point at a loopback host"),
        "expected the origin to be rejected, got: {stderr}"
    );
    assert!(
        !stdout.contains("Reviewing"),
        "the origin should be rejected before the diff is rendered, got: {stdout}"
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
