//! Restricted execution of project-provided build and test commands.
//!
//! Worktrees isolate Git state, NOT processes. This module runs untrusted
//! commands only in a container, unless an operator explicitly chooses the
//! unsafe host backend. No implicit fallback to the host is allowed.

use std::path::Path;
use std::process::{Output, Stdio};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use tokio::io::{AsyncRead, AsyncReadExt};
use tokio::process::Command;

const MAX_STREAM_BYTES: u64 = 128 * 1024;
const WORKDIR: &str = "/workspace";

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
    #[error("sandboxed check exceeded its 128 KiB output limit")]
    OutputLimit,
}

impl BuildSandbox {
    pub fn new(backend: BuildBackend, image: Option<String>) -> Self {
        Self { backend, image }
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
        if root.to_string_lossy().chars().any(|ch| matches!(ch, ',' | '\n' | '\r')) {
            return Err(BuildSandboxError::InvalidConfiguration);
        }
        let runtime = self.resolve_runtime().await?;
        let uid_gid = user_identity()?;
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|_| BuildSandboxError::InvalidConfiguration)?
            .as_nanos();
        let name = format!("ryoko-check-{}-{unique}", std::process::id());
        let mount = format!("type=bind,source={},target={WORKDIR}", root.display());

        let mut process = Command::new(runtime);
        process.args([
            "run",
            "--rm",
            "--name",
            &name,
            "--pull=never",
            "--network=none",
            "--read-only",
            "--cap-drop=ALL",
            "--security-opt=no-new-privileges",
            "--pids-limit=128",
            "--memory=2g",
            "--cpus=2",
            "--ipc=none",
            "--user",
            &uid_gid,
            "--tmpfs",
            "/tmp:rw,nosuid,nodev,size=256m",
            "--mount",
            &mount,
            "--workdir",
            WORKDIR,
            "--env",
            "HOME=/tmp",
            "--env",
            "CI=true",
            image,
            "/bin/sh",
            "-c",
            command,
        ]);
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
        if !output.status.success() || text.is_empty() || !text.bytes().all(|b| b.is_ascii_digit()) {
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

async fn read_bounded<R: AsyncRead + Unpin>(reader: R) -> std::io::Result<Vec<u8>> {
    let mut buf = Vec::new();
    reader.take(MAX_STREAM_BYTES + 1).read_to_end(&mut buf).await?;
    Ok(buf)
}

async fn run_bounded(
    mut command: Command,
    timeout: Duration,
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
        tokio::try_join!(child.wait(), read_bounded(stdout), read_bounded(stderr))
    })
    .await
    .map_err(|_| BuildSandboxError::Timeout(timeout.as_secs()))??;
    let (status, stdout, stderr) = result;
    if stdout.len() as u64 > MAX_STREAM_BYTES || stderr.len() as u64 > MAX_STREAM_BYTES {
        return Err(BuildSandboxError::OutputLimit);
    }
    Ok(Output {
        status,
        stdout,
        stderr,
    })
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
        for backend in [BuildBackend::Auto, BuildBackend::Docker, BuildBackend::Podman] {
            assert_ne!(backend, BuildBackend::UnsafeHost);
        }
    }
}
