//! Integrity tests for Git worktree leases.
//!
//! Each test drives a real repository through the public manager API and checks
//! what matters when agents, crashes and the user share one checkout: the
//! user's files survive, the target ref only moves forward when the merge is
//! clean, and a failure never leaves the repository half-merged.

use std::path::Path;
use std::time::Duration;

use ryoko_core::ValidationReport;
use ryoko_worktree::{GitWorktreeManager, WorktreeError, WorktreeSession, WorktreeState};
use tempfile::TempDir;
use tokio::process::Command;
use uuid::Uuid;

async fn git(cwd: &Path, args: &[&str]) -> std::process::Output {
    Command::new("git")
        .current_dir(cwd)
        .args(args)
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("LC_ALL", "C")
        .env("GIT_AUTHOR_NAME", "Test")
        .env("GIT_AUTHOR_EMAIL", "test@example.com")
        .env("GIT_COMMITTER_NAME", "Test")
        .env("GIT_COMMITTER_EMAIL", "test@example.com")
        .output()
        .await
        .expect("git must be installed to run the worktree integrity tests")
}

async fn git_ok(cwd: &Path, args: &[&str]) -> String {
    let output = git(cwd, args).await;
    assert!(
        output.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).trim().to_owned()
}

async fn branch_exists(root: &Path, branch: &str) -> bool {
    git(
        root,
        &[
            "show-ref",
            "--verify",
            "--quiet",
            &format!("refs/heads/{branch}"),
        ],
    )
    .await
    .status
    .success()
}

async fn tip(root: &Path) -> String {
    git_ok(root, &["rev-parse", "HEAD"]).await
}

async fn init_repo() -> (TempDir, GitWorktreeManager) {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    git_ok(root, &["init", "-b", "main"]).await;
    git_ok(root, &["config", "user.name", "Test"]).await;
    git_ok(root, &["config", "user.email", "test@example.com"]).await;
    git_ok(root, &["config", "commit.gpgsign", "false"]).await;
    // Byte-exact content assertions must not depend on Windows line endings.
    git_ok(root, &["config", "core.autocrlf", "false"]).await;
    // Rename detection is asserted below; a global config must not decide it.
    git_ok(root, &["config", "diff.renames", "true"]).await;
    tokio::fs::write(root.join("README.md"), "# project\n")
        .await
        .unwrap();
    tokio::fs::create_dir_all(root.join("docs")).await.unwrap();
    tokio::fs::write(root.join("docs/keep.md"), "keep\n")
        .await
        .unwrap();
    tokio::fs::write(root.join("docs/old.md"), "old\n")
        .await
        .unwrap();
    git_ok(root, &["add", "-A"]).await;
    git_ok(root, &["commit", "-m", "initial"]).await;
    let manager = GitWorktreeManager::new(root);
    (dir, manager)
}

fn approved() -> ValidationReport {
    ValidationReport::passed(
        "cargo test passed",
        vec!["12 passed".to_owned()],
        Duration::ZERO,
    )
}

async fn write(root: &Path, relative: &str, content: &str) {
    let target = root.join(relative);
    if let Some(parent) = target.parent() {
        tokio::fs::create_dir_all(parent).await.unwrap();
    }
    tokio::fs::write(&target, content).await.unwrap();
}

async fn published(
    manager: &GitWorktreeManager,
    agent: &str,
    path: &str,
    content: &str,
) -> WorktreeSession {
    let session = manager
        .create_worktree(agent, Uuid::new_v4())
        .await
        .unwrap();
    write(&session.path, path, content).await;
    manager
        .publish_worktree(&session, "agent change")
        .await
        .unwrap();
    session
}

#[tokio::test]
async fn uncommitted_main_changes_block_the_merge_and_survive() {
    let (dir, manager) = init_repo().await;
    let root = dir.path();
    let session = published(&manager, "dev-01", "src/agent.rs", "pub fn agent() {}\n").await;

    write(root, "README.md", "# user edit in progress\n").await;
    write(root, "scratch/notes.txt", "user notes\n").await;
    let before = tip(root).await;

    let error = manager
        .merge_worktree(&session, "main", &approved())
        .await
        .unwrap_err();
    assert!(matches!(error, WorktreeError::DirtyMainCheckout), "{error}");
    assert_eq!(
        tokio::fs::read_to_string(root.join("README.md"))
            .await
            .unwrap(),
        "# user edit in progress\n"
    );
    assert_eq!(
        tokio::fs::read_to_string(root.join("scratch/notes.txt"))
            .await
            .unwrap(),
        "user notes\n"
    );
    assert!(!root.join("src/agent.rs").exists());
    assert_eq!(tip(root).await, before);

    // Resolving the tracked change is the user's decision. Afterwards the merge
    // lands and the unrelated untracked file is still exactly as they left it.
    git_ok(root, &["checkout", "--", "README.md"]).await;
    manager
        .merge_worktree(&session, "main", &approved())
        .await
        .unwrap();
    assert_eq!(
        tokio::fs::read_to_string(root.join("src/agent.rs"))
            .await
            .unwrap(),
        "pub fn agent() {}\n"
    );
    assert_eq!(
        tokio::fs::read_to_string(root.join("scratch/notes.txt"))
            .await
            .unwrap(),
        "user notes\n"
    );
}

#[tokio::test]
async fn untracked_files_at_agent_paths_are_never_overwritten() {
    let (dir, manager) = init_repo().await;
    let root = dir.path();
    let same_path = published(&manager, "dev-01", "notes.md", "agent notes\n").await;
    let shadowed = published(&manager, "dev-02", "scratch/notes.txt", "agent notes\n").await;
    let before = tip(root).await;

    // `git reset --hard` silently replaces an untracked file that occupies a
    // path the merge writes, so the merge must refuse before moving the ref.
    write(root, "notes.md", "my own notes\n").await;
    match manager
        .merge_worktree(&same_path, "main", &approved())
        .await
        .unwrap_err()
    {
        WorktreeError::UntrackedOverwrite(paths) => {
            assert!(paths.contains("notes.md"), "{paths}")
        }
        other => panic!("expected an untracked overwrite refusal, got {other}"),
    }
    assert_eq!(
        tokio::fs::read_to_string(root.join("notes.md"))
            .await
            .unwrap(),
        "my own notes\n"
    );
    assert_eq!(tip(root).await, before);

    // Once the user moves their file away the merge is allowed, and the moved
    // copy is still untouched afterwards.
    tokio::fs::rename(root.join("notes.md"), root.join("notes.local.md"))
        .await
        .unwrap();
    manager
        .merge_worktree(&same_path, "main", &approved())
        .await
        .unwrap();
    assert_eq!(
        tokio::fs::read_to_string(root.join("notes.md"))
            .await
            .unwrap(),
        "agent notes\n"
    );
    assert_eq!(
        tokio::fs::read_to_string(root.join("notes.local.md"))
            .await
            .unwrap(),
        "my own notes\n"
    );

    // A file where the merge needs to create a directory is just as destructive.
    write(root, "scratch", "my file where a directory would go\n").await;
    match manager
        .merge_worktree(&shadowed, "main", &approved())
        .await
        .unwrap_err()
    {
        WorktreeError::UntrackedOverwrite(paths) => {
            assert!(paths.contains("scratch/notes.txt"), "{paths}")
        }
        other => panic!("expected an untracked shadow refusal, got {other}"),
    }
    assert_eq!(
        tokio::fs::read_to_string(root.join("scratch"))
            .await
            .unwrap(),
        "my file where a directory would go\n"
    );

    tokio::fs::remove_file(root.join("scratch")).await.unwrap();
    manager
        .merge_worktree(&shadowed, "main", &approved())
        .await
        .unwrap();
    assert_eq!(
        tokio::fs::read_to_string(root.join("scratch/notes.txt"))
            .await
            .unwrap(),
        "agent notes\n"
    );
    assert_eq!(
        tokio::fs::read_to_string(root.join("notes.local.md"))
            .await
            .unwrap(),
        "my own notes\n",
        "the user's copy must survive every later merge too"
    );
}

#[tokio::test]
async fn ignored_files_the_checkout_holds_are_not_overwritten_either() {
    let (dir, manager) = init_repo().await;
    let root = dir.path();
    write(root, ".gitignore", "*.env\n").await;
    git_ok(root, &["add", ".gitignore"]).await;
    git_ok(root, &["commit", "-m", "ignore env files"]).await;

    // `git add -A` never stages an ignored path, so the branch has to be crafted
    // the way a hand-made branch would be: the ignored file is forced in, with a
    // normal file so the publish has something to commit.
    let session = manager
        .create_worktree("dev-01", Uuid::new_v4())
        .await
        .unwrap();
    write(&session.path, "src/ignored_case.rs", "pub fn case() {}\n").await;
    write(&session.path, ".env", "AGENT_PLACEHOLDER=1\n").await;
    git_ok(&session.path, &["add", "-A"]).await;
    git_ok(&session.path, &["add", "-f", ".env"]).await;
    let pull_request = manager
        .publish_worktree(&session, "agent brings an env file")
        .await
        .unwrap();
    assert!(
        pull_request.files.iter().any(|path| path == ".env"),
        "{:?}",
        pull_request.files
    );

    write(root, ".env", "SECRET=user\n").await;
    let before = tip(root).await;
    match manager
        .merge_worktree(&session, "main", &approved())
        .await
        .unwrap_err()
    {
        WorktreeError::UntrackedOverwrite(paths) => assert!(paths.contains(".env"), "{paths}"),
        other => panic!("expected an ignored-file refusal, got {other}"),
    }
    assert_eq!(
        tokio::fs::read_to_string(root.join(".env")).await.unwrap(),
        "SECRET=user\n"
    );
    assert_eq!(tip(root).await, before);

    tokio::fs::rename(root.join(".env"), root.join("env.backup"))
        .await
        .unwrap();
    manager
        .merge_worktree(&session, "main", &approved())
        .await
        .unwrap();
    assert_eq!(
        tokio::fs::read_to_string(root.join(".env")).await.unwrap(),
        "AGENT_PLACEHOLDER=1\n"
    );
    assert_eq!(
        tokio::fs::read_to_string(root.join("env.backup"))
            .await
            .unwrap(),
        "SECRET=user\n"
    );
}

#[tokio::test]
async fn agent_deletions_merge_without_touching_neighbouring_files() {
    let (dir, manager) = init_repo().await;
    let root = dir.path();
    let session = manager
        .create_worktree("dev-01", Uuid::new_v4())
        .await
        .unwrap();
    tokio::fs::remove_file(session.path.join("docs/old.md"))
        .await
        .unwrap();
    let pull_request = manager
        .publish_worktree(&session, "drop old doc")
        .await
        .unwrap();
    assert!(
        pull_request.files.iter().any(|path| path == "docs/old.md"),
        "{:?}",
        pull_request.files
    );
    assert!(
        pull_request.patch.contains("deleted file mode"),
        "{}",
        pull_request.patch
    );
    assert!(
        root.join("docs/old.md").exists(),
        "a worktree deletion must not reach the checkout before the merge"
    );

    write(root, "docs/scratch.md", "mine\n").await;
    manager
        .merge_worktree(&session, "main", &approved())
        .await
        .unwrap();
    assert!(!root.join("docs/old.md").exists());
    assert_eq!(
        tokio::fs::read_to_string(root.join("docs/keep.md"))
            .await
            .unwrap(),
        "keep\n"
    );
    assert_eq!(
        tokio::fs::read_to_string(root.join("README.md"))
            .await
            .unwrap(),
        "# project\n"
    );
    assert_eq!(
        tokio::fs::read_to_string(root.join("docs/scratch.md"))
            .await
            .unwrap(),
        "mine\n"
    );
}

#[tokio::test]
async fn agent_renames_merge_with_the_old_path_removed() {
    let (dir, manager) = init_repo().await;
    let root = dir.path();
    let session = manager
        .create_worktree("dev-01", Uuid::new_v4())
        .await
        .unwrap();
    tokio::fs::rename(
        session.path.join("docs/old.md"),
        session.path.join("docs/renamed.md"),
    )
    .await
    .unwrap();
    let pull_request = manager
        .publish_worktree(&session, "rename old doc")
        .await
        .unwrap();
    assert!(
        pull_request
            .files
            .iter()
            .any(|path| path == "docs/renamed.md"),
        "{:?}",
        pull_request.files
    );
    assert!(
        pull_request.patch.contains("rename from docs/old.md"),
        "{}",
        pull_request.patch
    );
    assert!(
        root.join("docs/old.md").exists(),
        "the checkout keeps the old name until the merge"
    );

    manager
        .merge_worktree(&session, "main", &approved())
        .await
        .unwrap();
    assert!(!root.join("docs/old.md").exists());
    assert_eq!(
        tokio::fs::read_to_string(root.join("docs/renamed.md"))
            .await
            .unwrap(),
        "old\n"
    );
    assert_eq!(
        tokio::fs::read_to_string(root.join("docs/keep.md"))
            .await
            .unwrap(),
        "keep\n"
    );
}

#[tokio::test]
async fn binary_files_survive_publish_and_merge_byte_for_byte() {
    let (dir, manager) = init_repo().await;
    let root = dir.path();
    let original: &[u8] = b"\x00\x01\x02logo\xff\xfe";
    let updated: &[u8] = b"\x00\x01\x02logo-v2\xff\xfe\x00";
    tokio::fs::create_dir_all(root.join("assets"))
        .await
        .unwrap();
    tokio::fs::write(root.join("assets/logo.bin"), original)
        .await
        .unwrap();
    git_ok(root, &["add", "-A"]).await;
    git_ok(root, &["commit", "-m", "binary asset"]).await;

    let session = manager
        .create_worktree("dev-01", Uuid::new_v4())
        .await
        .unwrap();
    tokio::fs::write(session.path.join("assets/logo.bin"), updated)
        .await
        .unwrap();
    let pull_request = manager
        .publish_worktree(&session, "update logo")
        .await
        .unwrap();
    assert!(
        pull_request
            .files
            .iter()
            .any(|path| path == "assets/logo.bin"),
        "{:?}",
        pull_request.files
    );
    assert!(
        pull_request.patch.contains("GIT binary patch"),
        "{}",
        pull_request.patch
    );

    manager
        .merge_worktree(&session, "main", &approved())
        .await
        .unwrap();
    assert_eq!(
        tokio::fs::read(root.join("assets/logo.bin")).await.unwrap(),
        updated
    );
    assert_eq!(
        tokio::fs::read_to_string(root.join("README.md"))
            .await
            .unwrap(),
        "# project\n"
    );
}

#[tokio::test]
async fn interrupted_merge_is_reconciled_on_retry() {
    let (dir, manager) = init_repo().await;
    let root = dir.path();
    let session = published(&manager, "dev-01", "src/first.rs", "pub fn first() {}\n").await;

    // A crash after `update-ref` but before the lease state write: the branch is
    // already integrated while SQLite still says it is waiting for review.
    git_ok(
        root,
        &[
            "merge",
            "--no-ff",
            "-m",
            "external merge",
            &session.branch_name,
        ],
    )
    .await;
    let merged_tip = tip(root).await;
    assert!(root.join("src/first.rs").exists());
    let record = manager.load_session(session.id).unwrap().unwrap();
    assert_eq!(record.state, WorktreeState::Published);

    let error = manager
        .merge_worktree(&session, "main", &approved())
        .await
        .unwrap_err();
    assert!(matches!(error, WorktreeError::NoChanges(_)), "{error}");
    assert_eq!(
        tip(root).await,
        merged_tip,
        "the retry must not create a second merge commit"
    );
    let record = manager.load_session(session.id).unwrap().unwrap();
    assert_eq!(record.state, WorktreeState::Merged);
    assert_eq!(record.merge_commit.as_deref(), Some(merged_tip.as_str()));
}

#[tokio::test]
async fn merge_is_refused_while_the_users_own_merge_is_in_progress() {
    let (dir, manager) = init_repo().await;
    let root = dir.path();
    let session = published(&manager, "dev-01", "src/agent.rs", "pub fn agent() {}\n").await;

    // Leave a real conflicted merge in the checkout, exactly as a user would.
    git_ok(root, &["checkout", "-b", "user-side"]).await;
    write(root, "docs/keep.md", "side\n").await;
    git_ok(root, &["commit", "-am", "side"]).await;
    git_ok(root, &["checkout", "main"]).await;
    write(root, "docs/keep.md", "main\n").await;
    git_ok(root, &["commit", "-am", "main"]).await;
    let before = tip(root).await;
    let user_merge = git(root, &["merge", "user-side"]).await;
    assert!(
        !user_merge.status.success(),
        "the user's merge was expected to conflict"
    );
    assert!(root.join(".git/MERGE_HEAD").exists());

    let error = manager
        .merge_worktree(&session, "main", &approved())
        .await
        .unwrap_err();
    assert!(matches!(error, WorktreeError::DirtyMainCheckout), "{error}");
    let conflicted = tokio::fs::read_to_string(root.join("docs/keep.md"))
        .await
        .unwrap();
    assert!(conflicted.contains("<<<<<<<") && conflicted.contains(">>>>>>>"));
    assert!(
        root.join(".git/MERGE_HEAD").exists(),
        "Ryoko must not abandon the user's merge"
    );
    assert!(!root.join("src/agent.rs").exists());
    assert_eq!(tip(root).await, before);

    // The user resolves their own history; only then does the agent merge land.
    git_ok(root, &["merge", "--abort"]).await;
    manager
        .merge_worktree(&session, "main", &approved())
        .await
        .unwrap();
    assert!(root.join("src/agent.rs").exists());
    assert_eq!(
        tokio::fs::read_to_string(root.join("docs/keep.md"))
            .await
            .unwrap(),
        "main\n"
    );
}

#[tokio::test]
async fn stale_worktree_metadata_is_pruned_and_dead_leases_stay_dead() {
    let (dir, manager) = init_repo().await;
    let root = dir.path();
    let task = Uuid::new_v4();
    let session = manager.create_worktree("dev-01", task).await.unwrap();
    assert!(session.path.join("README.md").exists());

    // A crash (or a manual `rm -rf`) deletes the directory while Git keeps the
    // registration. `worktree add` refuses that path until it is pruned.
    tokio::fs::remove_dir_all(&session.path).await.unwrap();
    let stale = git_ok(root, &["worktree", "list", "--porcelain"]).await;
    assert!(stale.contains("prunable"), "{stale}");

    let resumed = manager.resume_worktree(task).await.unwrap();
    assert_eq!(resumed.path, session.path);
    assert!(resumed.path.join("README.md").exists());
    let listed = git_ok(root, &["worktree", "list", "--porcelain"]).await;
    assert!(!listed.contains("prunable"), "{listed}");

    // The repaired lease is still usable and still cleans up after itself.
    write(&resumed.path, "src/resumed.rs", "pub fn resumed() {}\n").await;
    manager
        .publish_worktree(&resumed, "resume after crash")
        .await
        .unwrap();
    manager.abandon_worktree(&resumed).await.unwrap();
    assert!(!resumed.path.exists());
    assert!(!branch_exists(root, &session.branch_name).await);
    assert!(matches!(
        manager.resume_worktree(task).await.unwrap_err(),
        WorktreeError::NotPublished(_)
    ));
    assert!(
        manager.open_sessions().unwrap().is_empty(),
        "an abandoned lease must not stay open forever"
    );
}

#[tokio::test]
async fn simultaneous_managers_keep_leases_and_publishes_separate() {
    let (dir, manager) = init_repo().await;
    let root = dir.path();
    let managers = [
        manager.clone(),
        GitWorktreeManager::new(root),
        GitWorktreeManager::new(root),
    ];
    write(root, "user.txt", "mine\n").await;

    // Three manager instances (a daemon and two CLI runs) create leases at the
    // same time against one repository.
    let mut tasks = Vec::new();
    for index in 0..6 {
        let manager = managers[index % managers.len()].clone();
        tasks.push(tokio::spawn(async move {
            let agent = format!("agent-{index}");
            manager
                .create_worktree(&agent, Uuid::new_v4())
                .await
                .map(|session| (manager, session))
        }));
    }
    let mut leases = Vec::new();
    for task in tasks {
        leases.push(task.await.unwrap().unwrap());
    }
    for (index, (_, session)) in leases.iter().enumerate() {
        assert!(session.path.join("README.md").exists());
        for (_, other) in &leases[index + 1..] {
            assert_ne!(session.path, other.path);
            assert_ne!(session.branch_name, other.branch_name);
        }
    }

    let mut tasks = Vec::new();
    for (index, (manager, session)) in leases.iter().cloned().enumerate() {
        tasks.push(tokio::spawn(async move {
            let path = format!("src/agent-{index}.rs");
            let content = format!("pub fn agent_{index}() {{}}\n");
            write(&session.path, &path, &content).await;
            let pull_request = manager
                .publish_worktree(&session, "parallel publish")
                .await
                .unwrap();
            (manager, session, path, pull_request)
        }));
    }
    let mut published_leases = Vec::new();
    for task in tasks {
        let (manager, session, path, pull_request) = task.await.unwrap();
        assert_eq!(pull_request.files, vec![path], "each publish owns its file");
        published_leases.push((manager, session));
    }
    assert_eq!(
        manager.open_sessions().unwrap().len(),
        6,
        "each publish must own exactly one lease"
    );
    for index in 0..6 {
        assert!(
            !root.join(format!("src/agent-{index}.rs")).exists(),
            "publishing must never write to the user's checkout"
        );
    }
    assert_eq!(
        tokio::fs::read_to_string(root.join("user.txt"))
            .await
            .unwrap(),
        "mine\n"
    );

    for (manager, session) in published_leases {
        manager.abandon_worktree(&session).await.unwrap();
    }
    assert!(manager.open_sessions().unwrap().is_empty());
    assert_eq!(
        git_ok(root, &["branch", "--list", "ryoko/*"]).await,
        "",
        "every managed branch must be gone after cleanup"
    );
    let worktrees = git_ok(root, &["worktree", "list", "--porcelain"]).await;
    assert_eq!(
        worktrees
            .lines()
            .filter(|line| line.starts_with("worktree "))
            .count(),
        1,
        "{worktrees}"
    );
}

#[tokio::test]
async fn concurrent_merges_update_the_target_branch_without_losing_commits() {
    let (dir, manager) = init_repo().await;
    let root = dir.path();
    let mut sessions = Vec::new();
    for index in 0..4 {
        sessions.push(
            published(
                &manager,
                &format!("dev-{index}"),
                &format!("src/parallel-{index}.rs"),
                &format!("pub fn parallel_{index}() {{}}\n"),
            )
            .await,
        );
    }
    write(root, "user/notes.txt", "mine\n").await;

    // All four validated branches ask to land at once through two manager
    // instances, as a daemon and a CLI run would. Git's compare-and-swap ref
    // update lets one win; the others must fail cleanly and can retry.
    let managers = [manager.clone(), GitWorktreeManager::new(root)];
    let mut pending = sessions.clone();
    let mut last_errors: Vec<String> = Vec::new();
    let mut attempts = 0;
    while !pending.is_empty() && attempts < 30 {
        attempts += 1;
        let mut tasks = Vec::new();
        for (index, session) in pending.into_iter().enumerate() {
            let manager = managers[index % managers.len()].clone();
            tasks.push(tokio::spawn(async move {
                let outcome = manager.merge_worktree(&session, "main", &approved()).await;
                (session, outcome)
            }));
        }
        let mut retry = Vec::new();
        for task in tasks {
            let (session, outcome) = task.await.unwrap();
            match outcome {
                Ok(_) | Err(WorktreeError::NoChanges(_)) => {}
                Err(error) => {
                    last_errors.push(format!("{}: {error}", session.branch_name));
                    retry.push(session);
                }
            }
        }
        pending = retry;
        if !pending.is_empty() {
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    }
    assert!(
        pending.is_empty(),
        "concurrent merges did not converge in {attempts} attempts: {last_errors:?}"
    );

    for (index, session) in sessions.iter().enumerate() {
        assert_eq!(
            tokio::fs::read_to_string(root.join(format!("src/parallel-{index}.rs")))
                .await
                .unwrap(),
            format!("pub fn parallel_{index}() {{}}\n")
        );
        let merged = git(
            root,
            &["merge-base", "--is-ancestor", &session.branch_name, "main"],
        )
        .await;
        assert!(
            merged.status.success(),
            "branch {} was reported merged but is not in main",
            session.branch_name
        );
    }
    assert_eq!(
        tokio::fs::read_to_string(root.join("user/notes.txt"))
            .await
            .unwrap(),
        "mine\n"
    );
    let resolved = git(root, &["status", "--porcelain", "--untracked-files=no"]).await;
    assert!(
        resolved.status.success() && resolved.stdout.is_empty(),
        "the checkout must be clean after the merges: {}",
        String::from_utf8_lossy(&resolved.stdout)
    );
    git_ok(root, &["fsck", "--no-dangling", "--no-progress"]).await;
}

#[tokio::test]
async fn delete_versus_modify_conflict_leaves_the_checkout_alone() {
    let (dir, manager) = init_repo().await;
    let root = dir.path();
    let deleter = manager
        .create_worktree("dev-01", Uuid::new_v4())
        .await
        .unwrap();
    tokio::fs::remove_file(deleter.path.join("docs/old.md"))
        .await
        .unwrap();
    manager
        .publish_worktree(&deleter, "delete old doc")
        .await
        .unwrap();

    let editor = manager
        .create_worktree("dev-02", Uuid::new_v4())
        .await
        .unwrap();
    write(&editor.path, "docs/old.md", "edited\n").await;
    manager
        .publish_worktree(&editor, "edit old doc")
        .await
        .unwrap();

    manager
        .merge_worktree(&deleter, "main", &approved())
        .await
        .unwrap();
    let before = tip(root).await;
    let error = manager
        .merge_worktree(&editor, "main", &approved())
        .await
        .unwrap_err();
    assert!(
        matches!(error, WorktreeError::MergeConflict { .. }),
        "{error}"
    );
    assert_eq!(tip(root).await, before);
    assert!(!root.join("docs/old.md").exists());
    assert!(
        !root.join(".git/MERGE_HEAD").exists(),
        "a failed merge must not leave the checkout mid-merge"
    );
    let status = git_ok(root, &["status", "--porcelain"]).await;
    assert!(
        !status.lines().any(|line| line.contains("docs/old.md")),
        "{status}"
    );
}
