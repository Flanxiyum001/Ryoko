//! Fetch, fast-forward pull and push for the checkout the IDE has open.
//!
//! Rules this module enforces:
//!
//! - Git runs with an explicit argv (no shell) and a bounded output read.
//! - A pull is `--ff-only`: Ryoko never invents a merge commit behind the
//!   operator's back, and refuses a dirty checkout instead of guessing.
//! - A push never forces, never rewrites history and never targets a different
//!   branch than the checked-out one.
//! - The GitHub token reaches Git through an ephemeral credential-helper
//!   definition and the process environment, never through a command-line
//!   argument, a remote URL or a file on disk. It is only supplied for an
//!   `https://github.com/...` remote.

use std::path::{Path, PathBuf};
use std::process::Stdio;

use serde::{Deserialize, Serialize};
use tokio::io::AsyncReadExt;
use tokio::process::Command;

use crate::{GitHubError, GitHubRepository, StoredCredential};

const MAX_OUTPUT_BYTES: usize = 64 * 1024;
const GIT_TIMEOUT_SECS: u64 = 180;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SyncAction {
    Fetch,
    Pull,
    Push,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SyncStatus {
    pub is_repository: bool,
    /// `None` when HEAD is detached.
    pub branch: Option<String>,
    pub upstream: Option<String>,
    pub ahead: u64,
    pub behind: u64,
    pub dirty: bool,
    pub remote_url: Option<String>,
}

impl SyncStatus {
    pub fn not_a_repository() -> Self {
        Self {
            is_repository: false,
            branch: None,
            upstream: None,
            ahead: 0,
            behind: 0,
            dirty: false,
            remote_url: None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SyncOutcome {
    pub action: SyncAction,
    pub branch: String,
    pub up_to_date: bool,
    /// Bounded, token-redacted Git summary for the status bar.
    pub summary: String,
}

/// The credential-helper definition Git is given. `!` makes Git run it with its
/// own shell; the secret comes from `RYOKO_GIT_TOKEN` in the environment, so it
/// is absent from argv, config files and the remote URL.
pub const CREDENTIAL_HELPER: &str = "!f() { test \"$1\" = get && printf 'username=%s\\npassword=%s\\n' \"${RYOKO_GIT_USER:-x-access-token}\" \"$RYOKO_GIT_TOKEN\"; }; f";

fn credential_args(credential: Option<&StoredCredential>) -> Vec<String> {
    match credential {
        Some(credential) if !credential.token.is_empty() => vec![
            // Clear any configured helper so a prompt or a cached token cannot
            // win over the signed-in account.
            "-c".to_owned(),
            "credential.helper=".to_owned(),
            "-c".to_owned(),
            format!("credential.helper={CREDENTIAL_HELPER}"),
            "-c".to_owned(),
            "core.askPass=".to_owned(),
            "-c".to_owned(),
            "credential.interactive=never".to_owned(),
            // The login is informational; GitHub accepts any username for a
            // token over HTTPS.
            "-c".to_owned(),
            format!(
                "ryoko.login={}",
                if credential.login.is_empty() {
                    "token"
                } else {
                    credential.login.as_str()
                }
            ),
        ],
        _ => vec!["-c".to_owned(), "credential.interactive=never".to_owned()],
    }
}

/// Strips a token that somehow reached Git's own output. Git echoes the remote
/// URL on errors, and an operator can configure a URL with a credential in it.
fn redact(text: &str, credential: Option<&StoredCredential>) -> String {
    let mut out = text.to_owned();
    if let Some(credential) = credential {
        let token = credential.token.expose();
        if token.len() >= 8 {
            out = out.replace(token, "<redacted>");
        }
    }
    let trimmed = out.trim();
    let bounded: String = trimmed.chars().take(600).collect();
    if bounded.is_empty() {
        "git produced no output".to_owned()
    } else {
        bounded
    }
}

async fn run_git(
    root: &Path,
    args: &[String],
    credential: Option<&StoredCredential>,
) -> Result<(bool, String, String), GitHubError> {
    let mut command = Command::new("git");
    command
        .current_dir(root)
        .args(credential_args(credential))
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        // A pinned terminal prompt: Git must never block an app window.
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GIT_ASKPASS", "")
        .env("GCM_INTERACTIVE", "never")
        .env("LC_ALL", "C");
    if let Some(credential) = credential {
        if !credential.token.is_empty() {
            command.env("RYOKO_GIT_TOKEN", credential.token.expose());
        }
    }
    let mut child = command
        .spawn()
        .map_err(|_| GitHubError::Git("Git is not installed or not on PATH".to_owned()))?;
    let stdout = child.stdout.take();
    let stderr = child.stderr.take();
    let read = async {
        let mut out = Vec::new();
        let mut err = Vec::new();
        if let Some(stream) = stdout {
            stream
                .take(MAX_OUTPUT_BYTES as u64)
                .read_to_end(&mut out)
                .await
                .map_err(|_| GitHubError::Git("could not read Git output".to_owned()))?;
        }
        if let Some(stream) = stderr {
            stream
                .take(MAX_OUTPUT_BYTES as u64)
                .read_to_end(&mut err)
                .await
                .map_err(|_| GitHubError::Git("could not read Git output".to_owned()))?;
        }
        Ok::<_, GitHubError>((out, err))
    };
    let (out, err) =
        match tokio::time::timeout(std::time::Duration::from_secs(GIT_TIMEOUT_SECS), read).await {
            Ok(result) => result?,
            Err(_) => {
                let _ = child.kill().await;
                return Err(GitHubError::Git(
                    "Git did not finish in time; the operation was stopped".to_owned(),
                ));
            }
        };
    let status = child
        .wait()
        .await
        .map_err(|_| GitHubError::Git("Git did not report a status".to_owned()))?;
    Ok((
        status.success(),
        String::from_utf8_lossy(&out).into_owned(),
        String::from_utf8_lossy(&err).into_owned(),
    ))
}

async fn run_checked(
    root: &Path,
    args: &[String],
    credential: Option<&StoredCredential>,
    context: &'static str,
) -> Result<String, GitHubError> {
    let (ok, stdout, stderr) = run_git(root, args, credential).await?;
    if ok {
        Ok(stdout)
    } else {
        Err(GitHubError::Git(format!(
            "{context}: {}",
            redact(&stderr, credential)
        )))
    }
}

/// Only a plain `https://github.com/...` remote may receive the token.
pub fn github_https_remote(url: &str) -> bool {
    reqwest::Url::parse(url).is_ok_and(|parsed| {
        parsed.scheme() == "https"
            && parsed.host_str() == Some("github.com")
            && parsed.username().is_empty()
            && parsed.password().is_none()
    })
}

pub async fn status(root: &Path) -> Result<SyncStatus, GitHubError> {
    if !root.is_dir() {
        return Ok(SyncStatus::not_a_repository());
    }
    let inside = run_git(
        root,
        &["rev-parse".into(), "--is-inside-work-tree".into()],
        None,
    )
    .await?;
    if !inside.0 || inside.1.trim() != "true" {
        return Ok(SyncStatus::not_a_repository());
    }
    let branch_out = run_git(
        root,
        &[
            "symbolic-ref".into(),
            "--short".into(),
            "-q".into(),
            "HEAD".into(),
        ],
        None,
    )
    .await?;
    let branch = branch_out
        .0
        .then(|| branch_out.1.trim().to_owned())
        .filter(|value| !value.is_empty() && value.len() <= 255);

    let upstream_out = run_git(
        root,
        &[
            "rev-parse".into(),
            "--abbrev-ref".into(),
            "--symbolic-full-name".into(),
            "@{u}".into(),
        ],
        None,
    )
    .await?;
    let upstream = upstream_out
        .0
        .then(|| upstream_out.1.trim().to_owned())
        .filter(|value| !value.is_empty() && value.len() <= 300);

    let (mut ahead, mut behind) = (0, 0);
    if upstream.is_some() {
        let counts = run_git(
            root,
            &[
                "rev-list".into(),
                "--left-right".into(),
                "--count".into(),
                "HEAD...@{u}".into(),
            ],
            None,
        )
        .await?;
        if counts.0 {
            let mut parts = counts.1.split_whitespace();
            ahead = parts
                .next()
                .and_then(|value| value.parse().ok())
                .unwrap_or(0);
            behind = parts
                .next()
                .and_then(|value| value.parse().ok())
                .unwrap_or(0);
        }
    }

    let dirty = run_git(
        root,
        &[
            "status".into(),
            "--porcelain=v1".into(),
            "--untracked-files=normal".into(),
        ],
        None,
    )
    .await?;
    let remote_url = if let Some(upstream) = &upstream {
        let remote = upstream.split('/').next().unwrap_or("origin");
        let output = run_git(
            root,
            &[
                "remote".into(),
                "get-url".into(),
                "--".into(),
                remote.to_owned(),
            ],
            None,
        )
        .await?;
        output
            .0
            .then(|| output.1.trim().to_owned())
            .filter(|value| value.len() <= 500)
    } else {
        None
    };

    Ok(SyncStatus {
        is_repository: true,
        branch,
        upstream,
        ahead,
        behind,
        dirty: dirty.0 && !dirty.1.trim().is_empty(),
        remote_url,
    })
}

fn remote_name(upstream: &Option<String>) -> String {
    upstream
        .as_deref()
        .and_then(|value| value.split('/').next())
        .filter(|value| !value.is_empty() && value.len() <= 100)
        .unwrap_or("origin")
        .to_owned()
}

pub async fn run(
    root: &Path,
    action: SyncAction,
    credential: Option<&StoredCredential>,
) -> Result<SyncOutcome, GitHubError> {
    let current = status(root).await?;
    if !current.is_repository {
        return Err(GitHubError::Git(
            "the open folder is not a Git repository".to_owned(),
        ));
    }
    let branch = current.branch.clone().ok_or_else(|| {
        GitHubError::Git("HEAD is detached; check out a branch before syncing".to_owned())
    })?;
    let remote = remote_name(&current.upstream);
    // The token is only handed to Git when the destination is GitHub over
    // HTTPS. A local, SSH or third-party remote keeps whatever credential its
    // own configuration already uses.
    let credential = match (&current.remote_url, credential) {
        (Some(url), Some(credential)) if github_https_remote(url) => Some(credential),
        _ => None,
    };

    let summary = match action {
        SyncAction::Fetch => {
            run_checked(
                root,
                &[
                    "fetch".into(),
                    "--prune".into(),
                    "--".into(),
                    remote.clone(),
                ],
                credential,
                "fetch failed",
            )
            .await?;
            "Fetched the latest commits.".to_owned()
        }
        SyncAction::Pull => {
            if current.dirty {
                return Err(GitHubError::Git(
                    "the checkout has uncommitted changes; commit or stash them before pulling"
                        .to_owned(),
                ));
            }
            run_checked(
                root,
                &[
                    "fetch".into(),
                    "--prune".into(),
                    "--".into(),
                    remote.clone(),
                ],
                credential,
                "fetch failed",
            )
            .await?;
            let (ok, stdout, stderr) = run_git(
                root,
                &[
                    "merge".into(),
                    "--ff-only".into(),
                    "--no-edit".into(),
                    "@{u}".into(),
                ],
                None,
            )
            .await?;
            if !ok {
                let message = redact(&format!("{stdout}{stderr}"), credential);
                if message.contains("Already up to date") || message.contains("Already up-to-date")
                {
                    return Ok(SyncOutcome {
                        action,
                        branch,
                        up_to_date: true,
                        summary: "Already up to date.".to_owned(),
                    });
                }
                return Err(GitHubError::Git(format!(
                    "pull is not a fast-forward; rebase or merge the branch yourself: {message}"
                )));
            }
            "Fast-forwarded to the upstream branch.".to_owned()
        }
        SyncAction::Push => {
            let (ok, stdout, stderr) = if current.upstream.is_some() {
                run_git(root, &["push".into()], credential).await?
            } else {
                run_git(
                    root,
                    &[
                        "push".into(),
                        "--set-upstream".into(),
                        "origin".into(),
                        branch.clone(),
                    ],
                    credential,
                )
                .await?
            };
            if !ok {
                return Err(GitHubError::Git(format!(
                    "push failed: {}",
                    redact(&format!("{stdout}{stderr}"), credential)
                )));
            }
            "Pushed the branch.".to_owned()
        }
    };

    let after = status(root).await?;
    Ok(SyncOutcome {
        action,
        branch,
        up_to_date: after.ahead == 0 && after.behind == 0,
        summary,
    })
}

/// Clones an authorized repository next to its sibling projects.
///
/// The destination is `parent/<repository name>`; nothing is overwritten and
/// the directory must not already exist.
pub async fn clone_repository(
    parent: &Path,
    repository: &GitHubRepository,
    credential: Option<&StoredCredential>,
) -> Result<PathBuf, GitHubError> {
    if !parent.is_dir() {
        return Err(GitHubError::Unsafe(
            "choose an existing folder for the clone",
        ));
    }
    let name = &repository.name;
    if name.is_empty()
        || name.len() > 100
        || !name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-' | b'_'))
        || name == "."
        || name == ".."
    {
        return Err(GitHubError::Unsafe(
            "repository name is not a safe folder name",
        ));
    }
    if !github_https_remote(&repository.clone_url) {
        return Err(GitHubError::Unsafe(
            "only an https://github.com remote can be cloned",
        ));
    }
    let destination = parent.join(name);
    if destination.exists() {
        return Err(GitHubError::Unsafe(
            "the clone destination already exists; open it instead",
        ));
    }

    let mut command = Command::new("git");
    command
        .current_dir(parent)
        .args(credential_args(credential))
        .args(["clone", "--", &repository.clone_url])
        .arg(&destination)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GIT_ASKPASS", "")
        .env("GCM_INTERACTIVE", "never")
        .env("LC_ALL", "C");
    if let Some(credential) = credential {
        if !credential.token.is_empty() {
            command.env("RYOKO_GIT_TOKEN", credential.token.expose());
        }
    }
    let output = command
        .output()
        .await
        .map_err(|_| GitHubError::Git("Git is not installed or not on PATH".to_owned()))?;
    if !output.status.success() {
        // A partial clone is left behind by Git on some failures; do not delete
        // a directory this function did not create.
        let message = redact(&String::from_utf8_lossy(&output.stderr), credential);
        return Err(GitHubError::Git(format!("clone failed: {message}")));
    }
    Ok(destination)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Secret;
    use std::process::Command as StdCommand;
    use tempfile::TempDir;

    /// An empty global config keeps a developer's own `user.name`, commit
    /// signing or hooks out of these tests on every platform.
    fn isolated_global_config() -> PathBuf {
        let path = std::env::temp_dir().join(format!(
            "ryoko-github-tests-{}-gitconfig",
            std::process::id()
        ));
        if !path.exists() {
            std::fs::write(&path, "").expect("empty global git config");
        }
        path
    }

    /// Reads a file out of a Git checkout. A host whose Git config converts line
    /// endings on checkout (`core.autocrlf`) hands back CRLF, so the comparison
    /// is made on the content the remote carried, not on the host's EOL policy.
    fn checkout_text(path: &Path) -> String {
        std::fs::read_to_string(path)
            .expect("checkout reads")
            .replace("\r\n", "\n")
    }

    fn git_in(dir: &Path, args: &[&str]) {
        let status = StdCommand::new("git")
            .current_dir(dir)
            .args(args)
            .env("GIT_TERMINAL_PROMPT", "0")
            .env("GIT_CONFIG_GLOBAL", isolated_global_config())
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .status()
            .expect("git runs");
        assert!(status.success(), "git {args:?} failed");
    }

    fn init_repo(dir: &Path) {
        std::fs::create_dir_all(dir).unwrap();
        git_in(dir, &["init", "-b", "main", "-q"]);
        git_in(dir, &["config", "user.email", "tests@example.invalid"]);
        git_in(dir, &["config", "user.name", "Ryoko Tests"]);
        git_in(dir, &["config", "commit.gpgsign", "false"]);
        std::fs::write(dir.join("README.md"), "one\n").unwrap();
        git_in(dir, &["add", "."]);
        git_in(dir, &["commit", "-q", "-m", "initial"]);
    }

    fn work_pair() -> (TempDir, PathBuf, PathBuf) {
        let root = TempDir::new().unwrap();
        let remote = root.path().join("remote.git");
        std::fs::create_dir_all(&remote).unwrap();
        git_in(&remote, &["init", "--bare", "-q", "-b", "main"]);
        let work = root.path().join("work");
        init_repo(&work);
        git_in(
            &work,
            &["remote", "add", "origin", remote.to_str().unwrap()],
        );
        git_in(&work, &["push", "-q", "-u", "origin", "main"]);
        (root, remote, work)
    }

    #[test]
    fn credential_helper_command_never_carries_the_token() {
        let credential = StoredCredential {
            token: Secret::new("gho_super_secret_value"),
            login: "octocat".to_owned(),
            scopes: vec![],
            source: crate::CredentialSource::Session,
        };
        let args = credential_args(Some(&credential));
        let joined = args.join(" ");
        assert!(!joined.contains("gho_super_secret_value"));
        assert!(joined.contains("credential.helper="));
        assert!(joined.contains("RYOKO_GIT_TOKEN"));
        assert_eq!(credential_args(None).len(), 2);
    }

    #[test]
    fn only_github_https_remotes_receive_the_token() {
        assert!(github_https_remote("https://github.com/acme/app.git"));
        assert!(!github_https_remote("http://github.com/acme/app.git"));
        assert!(!github_https_remote(
            "https://github.com.evil.example/acme/app"
        ));
        assert!(!github_https_remote(
            "https://user:token@github.com/acme/app"
        ));
        assert!(!github_https_remote("git@github.com:acme/app.git"));
        assert!(!github_https_remote("/tmp/remote.git"));
    }

    #[test]
    fn git_output_is_redacted_and_bounded() {
        let credential = StoredCredential {
            token: Secret::new("gho_super_secret_value"),
            login: "octocat".to_owned(),
            scopes: vec![],
            source: crate::CredentialSource::Session,
        };
        let text = format!(
            "fatal: https://gho_super_secret_value@github.com/x {}",
            "y".repeat(900)
        );
        let out = redact(&text, Some(&credential));
        assert!(!out.contains("gho_super_secret_value"));
        assert!(out.len() <= 600);
        assert_eq!(redact("   ", None), "git produced no output");
    }

    #[tokio::test]
    async fn a_plain_folder_is_reported_as_not_a_repository() {
        let folder = TempDir::new().unwrap();
        let status = status(folder.path()).await.unwrap();
        assert!(!status.is_repository);
    }

    #[tokio::test]
    async fn git_receives_global_options_before_its_subcommand() {
        let (_root, _remote, work) = work_pair();
        // Exercises the `-c` prefix and the `--` separator that every Git call
        // in this module uses, including with a credential configured.
        let credential = StoredCredential {
            token: Secret::new("gho_not_used_by_a_local_remote"),
            login: "octocat".to_owned(),
            scopes: vec![],
            source: crate::CredentialSource::Session,
        };
        run(&work, SyncAction::Fetch, Some(&credential))
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn status_push_and_fast_forward_pull_work_against_a_local_remote() {
        let (_root, remote, work) = work_pair();
        let initial = status(&work).await.unwrap();
        assert!(initial.is_repository);
        assert_eq!(initial.branch.as_deref(), Some("main"));
        assert_eq!(initial.upstream.as_deref(), Some("origin/main"));
        assert_eq!((initial.ahead, initial.behind), (0, 0));
        assert!(!initial.dirty);

        std::fs::write(work.join("README.md"), "two\n").unwrap();
        let dirty = status(&work).await.unwrap();
        assert!(dirty.dirty);
        git_in(&work, &["commit", "-q", "-am", "second"]);
        let ahead = status(&work).await.unwrap();
        assert_eq!(ahead.ahead, 1);

        let pushed = run(&work, SyncAction::Push, None).await.unwrap();
        assert!(pushed.up_to_date);
        assert_eq!(status(&work).await.unwrap().ahead, 0);

        // A second checkout adds a commit; the first fast-forwards onto it.
        let other = TempDir::new().unwrap();
        let other_path = other.path().join("other");
        git_in(
            other.path(),
            &[
                "clone",
                "-q",
                remote.to_str().unwrap(),
                other_path.to_str().unwrap(),
            ],
        );
        git_in(
            &other_path,
            &["config", "user.email", "tests@example.invalid"],
        );
        git_in(&other_path, &["config", "user.name", "Ryoko Tests"]);
        std::fs::write(other_path.join("README.md"), "three\n").unwrap();
        git_in(&other_path, &["commit", "-q", "-am", "third"]);
        git_in(&other_path, &["push", "-q", "origin", "main"]);

        run(&work, SyncAction::Fetch, None).await.unwrap();
        assert_eq!(status(&work).await.unwrap().behind, 1);
        let pulled = run(&work, SyncAction::Pull, None).await.unwrap();
        assert!(pulled.up_to_date);
        assert_eq!(checkout_text(&work.join("README.md")), "three\n");
    }

    #[tokio::test]
    async fn a_dirty_checkout_is_never_pulled() {
        let (_root, _remote, work) = work_pair();
        std::fs::write(work.join("README.md"), "local edit\n").unwrap();
        let error = run(&work, SyncAction::Pull, None).await.unwrap_err();
        assert!(error.to_string().contains("uncommitted changes"));
    }

    #[tokio::test]
    async fn a_diverged_branch_is_refused_instead_of_merged() {
        let (root, remote, work) = work_pair();
        let other = root.path().join("other");
        git_in(
            root.path(),
            &[
                "clone",
                "-q",
                remote.to_str().unwrap(),
                other.to_str().unwrap(),
            ],
        );
        git_in(&other, &["config", "user.email", "tests@example.invalid"]);
        git_in(&other, &["config", "user.name", "Ryoko Tests"]);
        std::fs::write(other.join("README.md"), "remote\n").unwrap();
        git_in(&other, &["commit", "-q", "-am", "remote change"]);
        git_in(&other, &["push", "-q", "origin", "main"]);

        std::fs::write(work.join("README.md"), "local\n").unwrap();
        git_in(&work, &["commit", "-q", "-am", "local change"]);
        run(&work, SyncAction::Fetch, None).await.unwrap();
        let error = run(&work, SyncAction::Pull, None).await.unwrap_err();
        assert!(error.to_string().contains("fast-forward"));
        // The local commit is intact: nothing was rewritten or merged.
        assert_eq!(checkout_text(&work.join("README.md")), "local\n");
    }

    #[tokio::test]
    async fn an_existing_destination_is_never_overwritten() {
        let parent = TempDir::new().unwrap();
        std::fs::create_dir_all(parent.path().join("app")).unwrap();
        let repository = GitHubRepository {
            id: 1,
            full_name: "acme/app".to_owned(),
            name: "app".to_owned(),
            owner: "acme".to_owned(),
            default_branch: "main".to_owned(),
            private: true,
            html_url: "https://github.com/acme/app".to_owned(),
            clone_url: "https://github.com/acme/app".to_owned(),
            description: String::new(),
            updated_at: String::new(),
        };
        let error = clone_repository(parent.path(), &repository, None)
            .await
            .unwrap_err();
        assert!(error.to_string().contains("already exists"));

        let mut unsafe_repo = repository.clone();
        unsafe_repo.name = "../escape".to_owned();
        assert!(
            clone_repository(parent.path(), &unsafe_repo, None)
                .await
                .is_err()
        );
    }
}
