//! Real Docker isolation smoke test. CI provides the pre-pulled image;
//! ordinary offline test runs skip without silently claiming proof.
#![cfg(target_os = "linux")]

use std::fs;
use std::time::Duration;

use ryoko_sandbox::{BuildBackend, BuildSandbox};
use tempfile::TempDir;

#[tokio::test]
async fn container_blocks_host_escape_network_and_source_writes() {
    let Ok(image) = std::env::var("RYOKO_CONTAINER_TEST_IMAGE") else {
        eprintln!("container isolation integration skipped: image not provisioned");
        return;
    };
    assert!(!image.is_empty());
    let base = TempDir::new().unwrap();
    let worktree = base.path().join("worktree");
    fs::create_dir(&worktree).unwrap();
    fs::write(worktree.join("source.txt"), "sealed source\n").unwrap();
    let outside = base.path().join("outside-secret");
    fs::write(&outside, "outside-worktree-secret").unwrap();
    std::os::unix::fs::symlink(&outside, worktree.join("escape")).unwrap();

    let pinned = BuildSandbox::new(BuildBackend::Docker, None)
        .image_id(&image)
        .await
        .expect("CI image must resolve to a local digest");
    let sandbox = BuildSandbox::new(BuildBackend::Docker, Some(pinned));
    // Any of these operations succeeding must fail the validation.
    let command = "test ! -e /workspace/escape \
        && ! cat /workspace/escape >/dev/null 2>&1 \
        && ! touch /etc/ryoko-escape 2>/dev/null \
        && ! wget -q -T 2 -O /tmp/network https://example.com 2>/dev/null \
        && ! touch /workspace/generated 2>/dev/null \
        && ! (echo changed > /workspace/source.txt) 2>/dev/null \
        && ! rm /workspace/source.txt 2>/dev/null \
        && echo allowed > /ryoko-build/generated \
        && test -f /ryoko-build/generated";
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
    assert!(!worktree.join("generated").exists());
    assert_eq!(
        fs::read_to_string(worktree.join("source.txt")).unwrap(),
        "sealed source\n"
    );
    assert_eq!(
        fs::read_to_string(outside).unwrap(),
        "outside-worktree-secret"
    );
}

/// Chromium needs writable /tmp and a real /dev/shm, and its screenshots must
/// land on the artefact mount rather than in the worktree.
///
/// The source stays read-only; outputs are written to a separate mount.
#[tokio::test]
async fn browser_scratch_space_and_artifact_mount_are_available() {
    let Ok(image) = std::env::var("RYOKO_CONTAINER_TEST_IMAGE") else {
        eprintln!("container browser integration skipped: image not provisioned");
        return;
    };
    assert!(!image.is_empty());
    let base = TempDir::new().unwrap();
    let worktree = base.path().join("worktree");
    fs::create_dir(&worktree).unwrap();
    let artifacts = base.path().join(".ryoko/artifacts/task-1");

    let pinned = BuildSandbox::new(BuildBackend::Docker, None)
        .image_id(&image)
        .await
        .expect("CI image must resolve to a local digest");
    let sandbox = BuildSandbox::new(BuildBackend::Docker, Some(pinned)).with_artifacts(&artifacts);
    let command = "test -w /tmp \
        && test -w /ryoko-artifacts \
        && test \"$(df -k /dev/shm | tail -1 | awk '{print $2}')\" -gt 200000 \
        && echo screenshot > /ryoko-artifacts/shot.txt \
        && test -f /ryoko-artifacts/shot.txt";
    let result = sandbox
        .run(command, &worktree, Duration::from_secs(35))
        .await
        .expect("Docker must be available in the explicit CI smoke test");
    assert!(
        result.status.success(),
        "scratch space or artefact mount missing: stdout={}; stderr={}",
        String::from_utf8_lossy(&result.stdout),
        String::from_utf8_lossy(&result.stderr),
    );
    assert_eq!(
        fs::read_to_string(artifacts.join("shot.txt")).unwrap(),
        "screenshot\n"
    );
    assert!(!worktree.join("shot.txt").exists());
}

/// A dependency-free Cargo test must compile and execute without source writes.
#[tokio::test]
async fn cargo_test_uses_disposable_build_output() {
    let Ok(image) = std::env::var("RYOKO_CONTAINER_TEST_RUST_IMAGE") else {
        eprintln!("Cargo isolation integration skipped: Rust image not provisioned");
        return;
    };
    let base = TempDir::new().unwrap();
    let worktree = base.path().join("worktree");
    fs::create_dir_all(worktree.join("src")).unwrap();
    let manifest = concat!(
        "[package]\n",
        "name = \"sealed-fixture\"\n",
        "version = \"0.1.0\"\n",
        "edition = \"2024\"\n"
    );
    let lock = "version = 4\n\n[[package]]\nname = \"sealed-fixture\"\nversion = \"0.1.0\"\n";
    let source = "#[test]\nfn works() { assert_eq!(2 + 2, 4); }\n";
    fs::write(worktree.join("Cargo.toml"), manifest).unwrap();
    fs::write(worktree.join("Cargo.lock"), lock).unwrap();
    fs::write(worktree.join("src/lib.rs"), source).unwrap();

    let pinned = BuildSandbox::new(BuildBackend::Docker, None)
        .image_id(&image)
        .await
        .expect("CI Rust image must resolve to a local digest");
    let sandbox = BuildSandbox::new(BuildBackend::Docker, Some(pinned));
    let result = sandbox
        .run(
            "cargo test --locked --offline && test -d /ryoko-build/target/debug",
            &worktree,
            Duration::from_secs(120),
        )
        .await
        .expect("Docker must be available in the explicit CI smoke test");
    assert!(
        result.status.success(),
        "Cargo isolation failed: stdout={}; stderr={}",
        String::from_utf8_lossy(&result.stdout),
        String::from_utf8_lossy(&result.stderr),
    );
    assert_eq!(
        fs::read_to_string(worktree.join("Cargo.toml")).unwrap(),
        manifest
    );
    assert_eq!(
        fs::read_to_string(worktree.join("Cargo.lock")).unwrap(),
        lock
    );
    assert_eq!(
        fs::read_to_string(worktree.join("src/lib.rs")).unwrap(),
        source
    );
    assert!(!worktree.join("target").exists());
}
