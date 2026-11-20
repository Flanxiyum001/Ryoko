//! Restricted execution of project-provided build and test commands.
//!
//! Worktrees isolate Git state, NOT processes. This module runs untrusted
//! commands only in a container, unless an operator explicitly chooses the
//! unsafe host backend. No implicit fallback to the host is allowed.

use std::path::{Path, PathBuf};
use std::process::{Output, Stdio};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use tokio::io::{AsyncRead, AsyncReadExt};
use tokio::process::Command;

const MAX_STREAM_BYTES: u64 = 128 * 1024;
const MAX_BUILD_OUTPUT_BYTES: u64 = 4 * 1024 * 1024;
const WORKDIR: &str = "/workspace";

/// Where read-only output (screenshots, traces, reports) is mounted. It is a
/// bind mount of its own, so artefacts never appear in the Git worktree.
pub const ARTIFACT_MOUNT: &str = "/ryoko-artifacts";

/// Chromium writes outside its install directory even with a read-only root.
const TMPFS: &str = "/tmp:rw,noexec,nosuid,size=512m";
/// Chromium renderers communicate through /dev/shm; the default 64 MiB makes
/// them crash with SIGBUS on real pages.
const SHM_SIZE: &str = "512m";
/// A browser launch forks renderer, GPU and utility processes. 128 was enough
/// for a test runner and not for Chromium, so this is deliberately generous.
const PIDS_LIMIT: u32 = 256;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BuildBackend {
    Auto,
    Docker,
    Podman,
    /// Explicit opt-in for trusted projects/tests only. Never an automatic fallback.
    UnsafeHost,
}

#[derive(Debug, Clone)]
pub struct BuildSandbox {
    pub backend: BuildBackend,
    /// Operator-prepared OCI image containing the project's compiler and
    /// dependencies. Images are never downloaded during a validation run.
    pub image: Option<String>,
    /// Optional host directory exposed at [`ARTIFACT_MOUNT`]. It must sit
    /// outside the worktree that is being checked.
    artifacts: Option<PathBuf>,
}

#[derive(Debug, thiserror::Error)]
pub enum BuildSandboxError {
    #[error("an OCI sandbox image is required; supply --sandbox-image")]
    MissingImage,
    #[error("invalid sandbox image or worktree path")]
    InvalidConfiguration,
    #[error("no working Docker or Podman daemon; refusing to run the check on the host")]
    NoContainerRuntime,
    #[error("could not execute sandboxed check: {0}")]
    Process(#[from] std::io::Error),
    #[error("sandboxed check timed out after {0} seconds")]
    Timeout(u64),
    #[error("sandboxed command exceeded its output limit")]
    OutputLimit,
}

/// The complete `docker run` argument list. It is a pure function so the
/// hardening flags can be asserted without a container runtime.
fn run_arguments(
    name: &str,
    uid_gid: &str,
    root: &Path,
    image: &str,
    command: &str,
    artifacts: Option<&Path>,
) -> Vec<String> {
    let mut args: Vec<String> = vec![
        "run".to_owned(),
        "--rm".to_owned(),
        "--name".to_owned(),
        name.to_owned(),
        "--pull=never".to_owned(),
        "--network=none".to_owned(),
        "--read-only".to_owned(),
        "--cap-drop=ALL".to_owned(),
        "--security-opt=no-new-privileges".to_owned(),
        format!("--pids-limit={PIDS_LIMIT}"),
        "--memory=2g".to_owned(),
        "--cpus=2".to_owned(),
        // A private IPC namespace still provides /dev/shm. `--ipc=none` hides
        // it entirely, which is exactly what makes Chromium renderers die.
        "--ipc=private".to_owned(),
        format!("--shm-size={SHM_SIZE}"),
        "--user".to_owned(),
        uid_gid.to_owned(),
        "--tmpfs".to_owned(),
        TMPFS.to_owned(),
        "--mount".to_owned(),
        format!("type=bind,source={},target={WORKDIR}", root.display()),
    ];
    if let Some(artifacts) = artifacts {
        args.push("--mount".to_owned());
        args.push(format!(
            "type=bind,source={},target={ARTIFACT_MOUNT}",
            artifacts.display()
        ));
    }
    args.extend([
        "--workdir".to_owned(),
        WORKDIR.to_owned(),
        "--env".to_owned(),
        "HOME=/tmp".to_owned(),
        "--env".to_owned(),
        "CI=true".to_owned(),
        image.to_owned(),
        "/bin/sh".to_owned(),
        "-c".to_owned(),
        command.to_owned(),
    ]);
    args
}

impl BuildSandbox {
    pub fn new(backend: BuildBackend, image: Option<String>) -> Self {
        Self {
            backend,
            image,
            artifacts: None,
        }
    }

    /// Exposes a writable host directory at [`ARTIFACT_MOUNT`] for the duration
    /// of one run. Everything else in the container stays read-only.
    pub fn with_artifacts(mut self, dir: impl Into<PathBuf>) -> Self {
        self.artifacts = Some(dir.into());
        self
    }

    /// Creates and validates the artefact directory. It is refused when it sits
    /// inside the worktree: screenshots must never show up in `git status`.
    fn prepare_artifacts(&self, root: &Path) -> Result<Option<PathBuf>, BuildSandboxError> {
        let Some(dir) = &self.artifacts else {
            return Ok(None);
        };
        if dir
            .symlink_metadata()
            .is_ok_and(|metadata| metadata.file_type().is_symlink())
        {
            return Err(BuildSandboxError::InvalidConfiguration);
        }
        std::fs::create_dir_all(dir)?;
        let canonical = dir.canonicalize()?;
        if !canonical.is_dir()
            || canonical.starts_with(root)
            || canonical
                .to_string_lossy()
                .chars()
                .any(|ch| matches!(ch, ',' | '\n' | '\r'))
        {
            // Remove the directory only if this call created it empty.
            let _ = std::fs::remove_dir(&canonical);
            return Err(BuildSandboxError::InvalidConfiguration);
        }
        Ok(Some(canonical))
    }

    pub async fn run(
        &self,
        command: &str,
        root: &Path,
        timeout: Duration,
    ) -> Result<Output, BuildSandboxError> {
        let root = root.canonicalize()?;
        if !root.is_dir() {
            return Err(BuildSandboxError::InvalidConfiguration);
        }
        if self.backend == BuildBackend::UnsafeHost {
            return self.run_host(command, &root, timeout).await;
        }

        let image = self
            .image
            .as_deref()
            .filter(|image| !image.trim().is_empty() && !image.starts_with('-'))
            .ok_or(BuildSandboxError::MissingImage)?;
        // Docker --mount uses comma-separated key/value pairs. Forbid commas
        // instead of allowing a workspace path to inject extra mount options.
        if root
            .to_string_lossy()
            .chars()
            .any(|ch| matches!(ch, ',' | '\n' | '\r'))
        {
            return Err(BuildSandboxError::InvalidConfiguration);
        }
        let runtime = self.resolve_runtime().await?;
        let uid_gid = user_identity()?;
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|_| BuildSandboxError::InvalidConfiguration)?
            .as_nanos();
        let name = format!("ryoko-check-{}-{unique}", std::process::id());
        let artifacts = self.prepare_artifacts(&root)?;

        let mut process = Command::new(runtime);
        process.args(run_arguments(
            &name,
            &uid_gid,
            &root,
            image,
            command,
            artifacts.as_deref(),
        ));
        // Even when the CLI is interrupted, remove the named container.
        // Killing the client alone does not guarantee the container stops.
        let result = run_bounded(process, timeout).await;
        let _ = tokio::time::timeout(
            Duration::from_secs(8),
            Command::new(runtime)
                .args(["rm", "-f", &name])
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .output(),
        )
        .await;
        result
    }

    /// Builds a generated sandbox image with an explicit Dockerfile and tag.
    /// The context is always the canonical project root; no shell is involved.
    pub async fn build_image(
        &self,
        root: &Path,
        dockerfile: &Path,
        tag: &str,
        timeout: Duration,
    ) -> Result<Output, BuildSandboxError> {
        if self.backend == BuildBackend::UnsafeHost {
            return Err(BuildSandboxError::InvalidConfiguration);
        }
        let root = root.canonicalize()?;
        if !root.is_dir() {
            return Err(BuildSandboxError::InvalidConfiguration);
        }
        let dockerfile = dockerfile.canonicalize()?;
        if !dockerfile.is_file()
            || !dockerfile.starts_with(&root)
            || dockerfile
                .symlink_metadata()
                .is_ok_and(|metadata| metadata.file_type().is_symlink())
        {
            return Err(BuildSandboxError::InvalidConfiguration);
        }
        if tag.trim().is_empty()
            || tag.starts_with('-')
            || tag.chars().any(|ch| ch.is_whitespace() || ch.is_control())
        {
            return Err(BuildSandboxError::InvalidConfiguration);
        }
        let runtime = self.resolve_runtime().await?;
        let mut process = Command::new(runtime);
        process.args([
            "build",
            "--file",
            &dockerfile.to_string_lossy(),
            "--tag",
            tag,
            &root.to_string_lossy(),
        ]);
        run_bounded_with_limit(process, timeout, MAX_BUILD_OUTPUT_BYTES).await
    }

    async fn run_host(
        &self,
        command: &str,
        root: &Path,
        timeout: Duration,
    ) -> Result<Output, BuildSandboxError> {
        #[cfg(windows)]
        let mut process = {
            let mut process = Command::new("cmd");
            process.args(["/C", command]);
            process
        };
        #[cfg(not(windows))]
        let mut process = {
            let mut process = Command::new("sh");
            process.args(["-c", command]);
            process
        };
        process.current_dir(root);
        run_bounded(process, timeout).await
    }

    async fn resolve_runtime(&self) -> Result<&'static str, BuildSandboxError> {
        match self.backend {
            BuildBackend::Docker => Ok("docker"),
            BuildBackend::Podman => Ok("podman"),
            BuildBackend::Auto => {
                for runtime in ["docker", "podman"] {
                    let available = tokio::time::timeout(
                        Duration::from_secs(4),
                        Command::new(runtime)
                            .args(["info", "--format", "{{.ServerVersion}}"])
                            .stdout(Stdio::null())
                            .stderr(Stdio::null())
                            .output(),
                    )
                    .await
                    .ok()
                    .and_then(Result::ok)
                    .is_some_and(|output| output.status.success());
                    if available {
                        return Ok(runtime);
                    }
                }
                Err(BuildSandboxError::NoContainerRuntime)
            }
            BuildBackend::UnsafeHost => unreachable!("host checked above"),
        }
    }
}

#[cfg(unix)]
fn user_identity() -> Result<String, BuildSandboxError> {
    let read = |arg: &str| -> Result<String, BuildSandboxError> {
        let output = std::process::Command::new("id").arg(arg).output()?;
        let text = String::from_utf8_lossy(&output.stdout).trim().to_owned();
        if !output.status.success() || text.is_empty() || !text.bytes().all(|b| b.is_ascii_digit())
        {
            return Err(BuildSandboxError::InvalidConfiguration);
        }
        Ok(text)
    };
    Ok(format!("{}:{}", read("-u")?, read("-g")?))
}

#[cfg(not(unix))]
fn user_identity() -> Result<String, BuildSandboxError> {
    // On non-Unix hosts, prefer an unprivileged UID inside the Linux container.
    // Writable worktree access depends on Docker Desktop's bind mount mapping.
    Ok("65534:65534".to_owned())
}

async fn read_bounded<R: AsyncRead + Unpin>(reader: R, limit: u64) -> std::io::Result<Vec<u8>> {
    let mut buf = Vec::new();
    reader.take(limit + 1).read_to_end(&mut buf).await?;
    Ok(buf)
}

async fn run_bounded(command: Command, timeout: Duration) -> Result<Output, BuildSandboxError> {
    run_bounded_with_limit(command, timeout, MAX_STREAM_BYTES).await
}

async fn run_bounded_with_limit(
    mut command: Command,
    timeout: Duration,
    limit: u64,
) -> Result<Output, BuildSandboxError> {
    command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    let mut child = command.spawn()?;
    let stdout = child
        .stdout
        .take()
        .ok_or(BuildSandboxError::InvalidConfiguration)?;
    let stderr = child
        .stderr
        .take()
        .ok_or(BuildSandboxError::InvalidConfiguration)?;
    let result = tokio::time::timeout(timeout, async {
        tokio::try_join!(
            child.wait(),
            read_bounded(stdout, limit),
            read_bounded(stderr, limit)
        )
    })
    .await
    .map_err(|_| BuildSandboxError::Timeout(timeout.as_secs()))??;
    let (status, stdout, stderr) = result;
    if stdout.len() as u64 > limit || stderr.len() as u64 > limit {
        return Err(BuildSandboxError::OutputLimit);
    }
    Ok(Output {
        status,
        stdout,
        stderr,
    })
}

#[cfg(test)]
mod runtime_flags {
    use super::*;

    fn run_args(artifacts: Option<&Path>) -> Vec<String> {
        run_arguments(
            "ryoko-check-1",
            "1000:1000",
            Path::new("/repo/worktree"),
            "ryoko-sandbox:latest",
            "cargo test",
            artifacts,
        )
    }

    #[test]
    fn chromium_gets_writable_scratch_space_and_enough_processes() {
        let args = run_args(None);
        let has = |value: &str| args.iter().any(|arg| arg == value);
        assert!(has("--shm-size=512m"), "{args:?}");
        assert!(has("--pids-limit=256"), "{args:?}");
        assert!(has("/tmp:rw,noexec,nosuid,size=512m"), "{args:?}");
        assert!(
            has("--ipc=private"),
            "a private IPC namespace keeps /dev/shm"
        );
        assert!(!has("--ipc=none"), "--ipc=none removes /dev/shm entirely");
        // The hardening that must not regress while adding scratch space.
        assert!(has("--network=none"));
        assert!(has("--read-only"));
        assert!(has("--cap-drop=ALL"));
        assert!(has("--security-opt=no-new-privileges"));
        assert!(args.contains(&"/bin/sh".to_owned()));
    }

    #[test]
    fn artifacts_are_a_separate_optional_mount() {
        let args = run_args(None);
        assert!(!args.iter().any(|arg| arg.contains(ARTIFACT_MOUNT)));

        let args = run_args(Some(Path::new("/repo/.ryoko/artifacts/task-1")));
        assert!(args.contains(&format!(
            "type=bind,source=/repo/.ryoko/artifacts/task-1,target={ARTIFACT_MOUNT}"
        )));
        assert!(args.contains(&"type=bind,source=/repo/worktree,target=/workspace".to_owned()));
        // The worktree stays the only source mount; artefacts are never mixed in.
        assert_eq!(
            args.iter()
                .filter(|arg| arg.starts_with("type=bind"))
                .count(),
            2
        );
    }

    #[test]
    fn an_artifacts_directory_inside_the_worktree_is_refused() {
        let dir = tempfile::TempDir::new().unwrap();
        let worktree = dir.path().join("worktree");
        std::fs::create_dir(&worktree).unwrap();
        let sandbox = BuildSandbox::new(BuildBackend::Docker, Some("image:1".to_owned()))
            .with_artifacts(worktree.join("screenshots"));
        assert!(matches!(
            sandbox.prepare_artifacts(&worktree),
            Err(BuildSandboxError::InvalidConfiguration)
        ));
    }

    #[test]
    fn an_artifacts_directory_outside_the_worktree_is_created_and_returned() {
        let dir = tempfile::TempDir::new().unwrap();
        let worktree = dir.path().join("worktree");
        std::fs::create_dir(&worktree).unwrap();
        let artifacts = dir.path().join(".ryoko/artifacts/task-1");
        let sandbox = BuildSandbox::new(BuildBackend::Docker, Some("image:1".to_owned()))
            .with_artifacts(&artifacts);
        let prepared = sandbox.prepare_artifacts(&worktree).unwrap().unwrap();
        assert!(prepared.ends_with(".ryoko/artifacts/task-1"));
        assert!(prepared.is_dir());
        assert!(
            BuildSandbox::new(BuildBackend::Docker, None)
                .prepare_artifacts(&worktree)
                .unwrap()
                .is_none()
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[tokio::test]
    async fn auto_backend_never_falls_back_to_host() {
        let dir = TempDir::new().unwrap();
        let sentinel = dir.path().join("untrusted-executed");
        let command = format!("touch {}", sentinel.display());
        let result = BuildSandbox::new(BuildBackend::Auto, None)
            .run(&command, dir.path(), Duration::from_secs(2))
            .await;
        assert!(matches!(result, Err(BuildSandboxError::MissingImage)));
        assert!(!sentinel.exists());
    }

    #[tokio::test]
    async fn host_requires_an_explicit_backend_and_caps_output() {
        let dir = TempDir::new().unwrap();
        #[cfg(unix)]
        let sandbox = BuildSandbox::new(BuildBackend::UnsafeHost, None);
        #[cfg(unix)]
        {
            let output = sandbox
                .run("printf safe", dir.path(), Duration::from_secs(2))
                .await
                .unwrap();
            assert_eq!(output.stdout, b"safe");
            let excess = sandbox
                .run("yes a | head -c 200000", dir.path(), Duration::from_secs(2))
                .await;
            assert!(matches!(excess, Err(BuildSandboxError::OutputLimit)));
        }
    }

    #[test]
    fn container_runtime_variants_are_not_host() {
        for backend in [
            BuildBackend::Auto,
            BuildBackend::Docker,
            BuildBackend::Podman,
        ] {
            assert_ne!(backend, BuildBackend::UnsafeHost);
        }
    }
}
