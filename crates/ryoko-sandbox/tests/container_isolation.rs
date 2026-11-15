//! Real Docker isolation smoke test. CI provides the pre-pulled image;
//! ordinary offline test runs skip without silently claiming proof.
#![cfg(target_os = "linux")]

use std::fs;
use std::time::Duration;

use ryoko_sandbox::{BuildBackend, BuildSandbox};
use tempfile::TempDir;

#[tokio::test]
async fn container_blocks_host_escape_and_network_but_writes_worktree() {
    let Ok(image) = std::env::var("RYOKO_CONTAINER_TEST_IMAGE") else {
        eprintln!("container isolation integration skipped: image not provisioned");
        return;
    };
    assert!(!image.is_empty());
    let base = TempDir::new().unwrap();
    let worktree = base.path().join("worktree");
    fs::create_dir(&worktree).unwrap();
    let outside = base.path().join("outside-secret");
    fs::write(&outside, "outside-worktree-secret").unwrap();
    std::os::unix::fs::symlink(&outside, worktree.join("escape")).unwrap();

    let sandbox = BuildSandbox::new(BuildBackend::Docker, Some(image));
    // Any of these operations succeeding must fail the validation.
    let command = "test ! -e /workspace/escape \
        && ! cat /workspace/escape >/dev/null 2>&1 \
        && ! touch /etc/ryoko-escape 2>/dev/null \
        && ! wget -q -T 2 -O /tmp/network https://example.com 2>/dev/null \
        && echo allowed > /workspace/generated \
        && test -f /workspace/generated";
    let result = sandbox
        .run(command, &worktree, Duration::from_secs(35))
        .await
        .expect("Docker must be available in the explicit CI smoke test");
    assert!(
        result.status.success(),
        "restricted runner failed: stdout={}; stderr={}",
        String::from_utf8_lossy(&result.stdout),
        String::from_utf8_lossy(&result.stderr),
    );
    assert_eq!(
        fs::read_to_string(worktree.join("generated")).unwrap(),
        "allowed\n"
    );
    assert_eq!(
        fs::read_to_string(outside).unwrap(),
        "outside-worktree-secret"
    );
}
