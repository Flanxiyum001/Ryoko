//! Zero-friction onboarding for offline project sandboxes.
//!
//! Generates a small Dockerfile from the repository's manifest, writes a
//! matching Dockerfile-specific ignore file, and optionally builds the image.
//! The generated file is deterministic: no model output and no repository
//! content is interpolated into the Dockerfile.

use std::path::{Component, Path, PathBuf};

use serde::Serialize;

use crate::{BuildBackend, BuildSandbox};

const DEFAULT_IMAGE: &str = "ryoko-sandbox:latest";
const BUILD_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30 * 60);
const IGNORE: &str = "\
.git
target
node_modules
dist
.ryoko/worktrees
.ryoko/events
.ryoko/runs
.ryoko/state
.env
.env.*
*.pem
*.key
";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Ecosystem {
    Rust,
    Node,
    Python,
}

impl Ecosystem {
    pub fn label(self) -> &'static str {
        match self {
            Self::Rust => "rust",
            Self::Node => "node",
            Self::Python => "python",
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct SandboxPlan {
    pub ecosystem: Ecosystem,
    pub image: String,
    pub dockerfile: PathBuf,
    pub context: PathBuf,
    pub dockerfile_contents: String,
    pub ignore_contents: String,
}

#[derive(Debug, thiserror::Error)]
pub enum InitError {
    #[error("no supported project manifest found (Cargo.toml, package.json or pyproject.toml)")]
    NoManifest,
    #[error("invalid sandbox image tag: {0}")]
    InvalidImage(String),
    #[error("sandbox files must stay inside the repository")]
    UnsafeOutput,
    #[error("sandbox file I/O failed: {0}")]
    Io(#[from] std::io::Error),
    #[error("sandbox image build failed: {0}")]
    Build(String),
}

impl From<crate::BuildSandboxError> for InitError {
    fn from(value: crate::BuildSandboxError) -> Self {
        Self::Build(value.to_string())
    }
}

impl SandboxPlan {
    /// Inspects one manifest, preferring Rust, then Node, then Python. A mixed
    /// repository can pass `--tag` and generated files explicitly.
    pub fn inspect(
        root: &Path,
        image: Option<String>,
        dockerfile: Option<PathBuf>,
    ) -> Result<Self, InitError> {
        let root = root.canonicalize()?;
        if !root.is_dir() {
            return Err(InitError::UnsafeOutput);
        }
        let ecosystem = if root.join("Cargo.toml").is_file() {
            Ecosystem::Rust
        } else if root.join("package.json").is_file() {
            Ecosystem::Node
        } else if root.join("pyproject.toml").is_file() {
            Ecosystem::Python
        } else {
            return Err(InitError::NoManifest);
        };
        let image = image.unwrap_or_else(|| DEFAULT_IMAGE.to_owned());
        validate_image(&image)?;
        let dockerfile = match dockerfile {
            Some(path) => safe_output(&root, &path)?,
            None => root.join(".ryoko/sandbox/Dockerfile"),
        };
        let dockerfile_contents = match ecosystem {
            Ecosystem::Rust => rust_dockerfile(&root),
            Ecosystem::Node => node_dockerfile(),
            Ecosystem::Python => python_dockerfile(),
        };
        Ok(Self {
            ecosystem,
            image,
            dockerfile,
            context: root,
            dockerfile_contents,
            ignore_contents: IGNORE.to_owned(),
        })
    }

    pub fn write(&self) -> Result<(), InitError> {
        let parent = self.dockerfile.parent().ok_or(InitError::UnsafeOutput)?;
        std::fs::create_dir_all(parent)?;
        let canonical_parent = parent.canonicalize()?;
        if !canonical_parent.starts_with(&self.context)
            || self
                .dockerfile
                .symlink_metadata()
                .is_ok_and(|metadata| metadata.file_type().is_symlink())
        {
            return Err(InitError::UnsafeOutput);
        }
        std::fs::write(&self.dockerfile, &self.dockerfile_contents)?;
        let ignore = self.dockerfile.with_file_name(format!(
            "{}.dockerignore",
            self.dockerfile
                .file_name()
                .and_then(|name| name.to_str())
                .ok_or(InitError::UnsafeOutput)?
        ));
        std::fs::write(ignore, &self.ignore_contents)?;
        Ok(())
    }

    pub async fn build(&self, backend: BuildBackend) -> Result<(), InitError> {
        let sandbox = BuildSandbox::new(backend, None);
        sandbox
            .build_image(&self.context, &self.dockerfile, &self.image, BUILD_TIMEOUT)
            .await?;
        Ok(())
    }
}

fn safe_output(root: &Path, path: &Path) -> Result<PathBuf, InitError> {
    if path.is_absolute()
        || path
            .components()
            .any(|component| matches!(component, Component::ParentDir | Component::Prefix(_)))
    {
        return Err(InitError::UnsafeOutput);
    }
    Ok(root.join(path))
}

fn validate_image(image: &str) -> Result<(), InitError> {
    let last_slash = image.rfind('/').unwrap_or(0);
    let last_colon = image.rfind(':');
    let (name, tag) = match last_colon.filter(|colon| *colon > last_slash) {
        Some(colon) => (&image[..colon], &image[colon + 1..]),
        None => (image, "latest"),
    };
    let valid_name = !name.is_empty()
        && name.len() <= 200
        && name
            .chars()
            .all(|ch| ch.is_ascii_lowercase() || ch.is_ascii_digit() || "./-_".contains(ch))
        && !name.starts_with(['-', '.', '/'])
        && !name.contains("..");
    let valid_tag = !tag.is_empty()
        && tag.len() <= 128
        && tag
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || "._-".contains(ch))
        && !tag.starts_with(['-', '.']);
    if valid_name && valid_tag {
        Ok(())
    } else {
        Err(InitError::InvalidImage(image.to_owned()))
    }
}

fn rust_dockerfile(root: &Path) -> String {
    let version = std::fs::read_to_string(root.join("Cargo.toml"))
        .ok()
        .and_then(|manifest| {
            manifest.lines().find_map(|line| {
                let (key, value) = line.split_once('=')?;
                if key.trim() != "rust-version" {
                    return None;
                }
                let version = value.trim().trim_matches('"');
                let valid = !version.is_empty()
                    && version.len() <= 16
                    && version.chars().all(|ch| ch.is_ascii_digit() || ch == '.');
                valid.then(|| version.to_owned())
            })
        })
        .unwrap_or_else(|| "1.85".to_owned());
    format!(
        "\
# Generated by `ryoko sandbox init`. Review before building.
FROM rust:{version}-bookworm
WORKDIR /workspace
COPY . .
RUN if [ -f Cargo.lock ]; then cargo fetch --locked; else cargo fetch; fi
ENV CARGO_NET_OFFLINE=true
"
    )
}

fn node_dockerfile() -> String {
    "\
# Generated by `ryoko sandbox init`. Review before building.
FROM node:22-bookworm-slim
WORKDIR /opt/ryoko
COPY package*.json ./
RUN if [ -f package-lock.json ]; then npm ci; else npm install; fi
ENV NODE_PATH=/opt/ryoko/node_modules
ENV PATH=/opt/ryoko/node_modules/.bin:$PATH
ENV npm_config_offline=true
WORKDIR /workspace
"
    .to_owned()
}

fn python_dockerfile() -> String {
    "\
# Generated by `ryoko sandbox init`. Review before building.
FROM python:3.12-slim
WORKDIR /opt/ryoko
COPY . .
RUN python -m pip install --no-cache-dir .
ENV PIP_NO_INDEX=1
WORKDIR /workspace
"
    .to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn detects_rust_and_generates_an_offline_image() {
        let dir = TempDir::new().unwrap();
        std::fs::write(
            dir.path().join("Cargo.toml"),
            "[package]\nname = \"demo\"\nrust-version = \"1.85\"\n",
        )
        .unwrap();
        let plan = SandboxPlan::inspect(dir.path(), None, None).unwrap();
        assert_eq!(plan.ecosystem, Ecosystem::Rust);
        assert_eq!(plan.image, DEFAULT_IMAGE);
        assert!(plan.dockerfile_contents.contains("FROM rust:1.85-bookworm"));
        assert!(plan.dockerfile_contents.contains("cargo fetch --locked"));
        assert!(plan.dockerfile_contents.contains("CARGO_NET_OFFLINE=true"));
    }

    #[test]
    fn rejects_missing_manifests_unsafe_outputs_and_bad_tags() {
        let dir = TempDir::new().unwrap();
        assert!(matches!(
            SandboxPlan::inspect(dir.path(), None, None),
            Err(InitError::NoManifest)
        ));
        std::fs::write(dir.path().join("package.json"), "{}").unwrap();
        assert!(matches!(
            SandboxPlan::inspect(
                dir.path(),
                None,
                Some(PathBuf::from("../outside/Dockerfile"))
            ),
            Err(InitError::UnsafeOutput)
        ));
        assert!(matches!(
            SandboxPlan::inspect(dir.path(), Some("bad tag".to_owned()), None),
            Err(InitError::InvalidImage(_))
        ));
        assert!(
            SandboxPlan::inspect(dir.path(), Some("ghcr.io/org/image:1.2".to_owned()), None)
                .is_ok()
        );
    }

    #[test]
    fn writes_the_dockerfile_and_ignore_file_inside_the_repository() {
        let dir = TempDir::new().unwrap();
        std::fs::write(
            dir.path().join("pyproject.toml"),
            "[project]\nname=\"demo\"\n",
        )
        .unwrap();
        let plan = SandboxPlan::inspect(dir.path(), None, None).unwrap();
        plan.write().unwrap();
        assert!(plan.dockerfile.is_file());
        assert!(
            plan.dockerfile
                .with_file_name("Dockerfile.dockerignore")
                .is_file()
        );
        assert!(plan.dockerfile.starts_with(dir.path()));
    }
}
