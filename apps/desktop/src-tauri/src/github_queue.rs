//! Read-only GitHub queue for the currently opened workspace.
//!
//! Uses the user's existing GitHub CLI authentication. No app token, PAT,
//! dynamic shell command or repository-selected executable reaches the UI.
use std::io::Read;
use std::path::Path;
use std::process::{Command, Stdio};

use serde::Serialize;
use serde_json::Value;
use tauri::State;

use crate::workspace::{WorkspaceState, current_root};

const MAX_GITHUB_BYTES: u64 = 2 * 1024 * 1024;
const MAX_REMOTE_BYTES: u64 = 4096;

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct QueueItem {
    number: u64,
    title: String,
    url: String,
    kind: &'static str,
    draft: bool,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GitHubQueue {
    repository: String,
    pulls: Vec<QueueItem>,
    issues: Vec<QueueItem>,
}

/// Only github.com URLs with plain owner/repository names are accepted.
/// Git remote URL rewrites are not allowed to select another API host.
fn parse_github_repo(remote: &str) -> Result<(String, String), String> {
    let remote = remote.trim();
    let path = remote
        .strip_prefix("https://github.com/")
        .or_else(|| remote.strip_prefix("git@github.com:"))
        .or_else(|| remote.strip_prefix("ssh://git@github.com/"))
        .ok_or_else(|| "The workspace origin must point to github.com".to_owned())?;
    let path = path.strip_suffix(".git").unwrap_or(path);
    let (owner, name) = path
        .split_once('/')
        .ok_or_else(|| "GitHub origin must be owner/repository".to_owned())?;
    let valid = |part: &str| {
        !part.is_empty()
            && part != "."
            && part != ".."
            && part.len() <= 100
            && part.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.'))
    };
    if !valid(owner) || !valid(name) {
        return Err("Invalid GitHub owner or repository name".to_owned());
    }
    Ok((owner.to_owned(), name.to_owned()))
}

/// Processes execute only fixed, read-only argument vectors. Limit stdout
/// before decoding, do not inherit stdin, and never expose CLI stderr.
fn limited_output(mut command: Command, limit: u64) -> Result<String, String> {
    let mut child = command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|_| "GitHub CLI (gh) or Git is unavailable".to_owned())?;
    let stdout = child.stdout.take().ok_or_else(|| "Missing process output".to_owned())?;
    let mut bytes = Vec::new();
    let read = stdout.take(limit + 1).read_to_end(&mut bytes);
    if read.is_err() || bytes.len() as u64 > limit {
        let _ = child.kill();
        let _ = child.wait();
        return Err("GitHub queue response exceeded its output limit".to_owned());
    }
    let exit = child.wait().map_err(|_| "GitHub queue subprocess failed".to_owned())?;
    if !exit.success() {
        return Err("GitHub CLI request failed. Run 'gh auth login' and verify repository access.".to_owned());
    }
    String::from_utf8(bytes).map_err(|_| "GitHub queue response was not UTF-8".to_owned())
}

fn safe_title(raw: &Value) -> String {
    raw.as_str().unwrap_or("(untitled)").chars().take(240).collect()
}

fn parse_items(
    body: &str,
    owner: &str,
    repo: &str,
    kind: &'static str,
) -> Result<Vec<QueueItem>, String> {
    let parsed: Value = serde_json::from_str(body)
        .map_err(|_| "GitHub returned an invalid queue response".to_owned())?;
    let entries = parsed.as_array().ok_or_else(|| "GitHub queue is not an array".to_owned())?;
    if entries.len() > 30 {
        return Err("GitHub queue contains too many items".to_owned());
    }
    let mut items = Vec::new();
    for entry in entries {
        // The issues API also includes pull requests; only real issues appear here.
        if kind == "issues" && entry.get("pull_request").is_some() {
            continue;
        }
        let number = entry.get("number").and_then(Value::as_u64)
            .filter(|n| *n > 0)
            .ok_or_else(|| "Invalid GitHub queue entry number".to_owned())?;
        items.push(QueueItem {
            number,
            title: safe_title(&entry["title"]),
            url: format!("https://github.com/{owner}/{repo}/{kind}/{number}"),
            kind,
            draft: kind == "pull" && entry["draft"].as_bool() == Some(true),
        });
    }
    Ok(items)
}

fn read_queue(root: &Path) -> Result<GitHubQueue, String> {
    let mut origin = Command::new("git");
    origin.args(["remote", "get-url", "origin"]).current_dir(root);
    let remote = limited_output(origin, MAX_REMOTE_BYTES)?;
    let (owner, repo) = parse_github_repo(&remote)?;
    let target = format!("repos/{owner}/{repo}");
    let read = |resource: &str| -> Result<String, String> {
        let mut cmd = Command::new("gh");
        cmd.args([
            "api", "--method", "GET",
            &format!("{target}/{resource}?state=open&per_page=30"),
        ])
        .env("GH_PROMPT_DISABLED", "1")
        .env("GH_HOST", "github.com")
        .env("GH_HTTP_TIMEOUT", "10");
        limited_output(cmd, MAX_GITHUB_BYTES)
    };
    let pulls = parse_items(&read("pulls")?, &owner, &repo, "pull")?;
    let issues = parse_items(&read("issues")?, &owner, &repo, "issues")?;
    Ok(GitHubQueue {
        repository: format!("{owner}/{repo}"),
        pulls,
        issues,
    })
}

/// Explicit refresh only. The assistant/agent does not invoke gh or receive
/// credentials; this command uses the user's existing local gh login.
#[tauri::command]
pub async fn github_review_queue(state: State<'_, WorkspaceState>) -> Result<GitHubQueue, String> {
    let root = current_root(&state)?;
    tauri::async_runtime::spawn_blocking(move || read_queue(&root))
        .await
        .map_err(|_| "GitHub queue worker failed".to_owned())?
}

#[cfg(test)]
mod tests {
    use super::{parse_github_repo, parse_items};

    #[test]
    fn only_restricted_github_origin_is_accepted() {
        assert_eq!(
            parse_github_repo("git@github.com:acme/project.git").unwrap(),
            ("acme".into(), "project".into()),
        );
        assert_eq!(
            parse_github_repo("https://github.com/acme/project").unwrap(),
            ("acme".into(), "project".into()),
        );
        for invalid in [
            "https://evil.example/acme/project",
            "https://github.com/acme/project/extra",
            "https://github.com/acme/../secrets",
            "https://github.com/acme/project?token=bad",
            "git@github.com:acme/project;evil",
        ] {
            assert!(parse_github_repo(invalid).is_err(), "{invalid}");
        }
    }

    #[test]
    fn issue_queue_excludes_pull_requests_and_constructs_safe_links() {
        let json = r#"[{"number":3,"title":"Fix a bug"},
            {"number":4,"title":"Actually a PR","pull_request":{}}]"#;
        let items = parse_items(json, "acme", "project", "issues").unwrap();
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].url, "https://github.com/acme/project/issues/3");
        assert!(parse_items(r#"[{"number":0}]"#, "acme", "project", "pull").is_err());
    }
}
