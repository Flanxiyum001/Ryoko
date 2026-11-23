//! Zero-friction onboarding for offline project sandboxes.
//!
//! Generates a small Dockerfile from the repository's manifest, writes a
//! matching Dockerfile-specific ignore file, and optionally builds the image.
//! The generated file is deterministic: no model output and no repository
//! content is interpolated into the Dockerfile.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Component, Path, PathBuf};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

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
.ryoko/artifacts
.env
.env.*
*.pem
*.key
";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
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

const MAX_MANIFEST_BYTES: u64 = 512 * 1024;

/// Fixed path of the bundled UI verification harness inside the image.
pub const VERIFY_UI_CONTAINER_PATH: &str = "/opt/ryoko/verify-ui.mjs";
/// Where the harness is written in the project, relative to the repository.
const VERIFY_UI_PROJECT_PATH: &str = ".ryoko/sandbox/verify-ui.mjs";

/// The harness itself. It is shipped as a file so it can be syntax-checked and
/// reviewed, then embedded into the crate at compile time.
const VERIFY_UI_SCRIPT: &str = include_str!("verify_ui.mjs");

/// The bundled Playwright harness agents call instead of writing their own.
pub fn verify_ui_script() -> &'static str {
    VERIFY_UI_SCRIPT
}

/// Frontend tooling found in `package.json`. Only the manifest is read, so
/// detection never executes project code.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct FrontendProfile {
    /// Detected frameworks, sorted and de-duplicated: `next`, `react`, `svelte`,
    /// `tailwind`, `vite`, `vue`.
    pub frameworks: Vec<String>,
    /// Playwright is a direct dependency.
    pub playwright: bool,
    /// Browsers must be baked into the image for this project.
    pub browser_stack: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct SandboxPlan {
    pub ecosystem: Ecosystem,
    pub image: String,
    pub dockerfile: PathBuf,
    pub context: PathBuf,
    pub dockerfile_contents: String,
    pub ignore_contents: String,
    pub frontend: FrontendProfile,
    /// Where the verification harness is written, when the project needs one.
    pub verify_script: Option<PathBuf>,
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
        let frontend = detect_frontend(&root);
        let dockerfile_contents = match ecosystem {
            Ecosystem::Rust => rust_dockerfile(&root),
            Ecosystem::Node => node_dockerfile(&frontend),
            Ecosystem::Python => python_dockerfile(),
        };
        let verify_script = frontend
            .browser_stack
            .then(|| root.join(VERIFY_UI_PROJECT_PATH));
        Ok(Self {
            ecosystem,
            image,
            dockerfile,
            context: root,
            dockerfile_contents,
            ignore_contents: IGNORE.to_owned(),
            frontend,
            verify_script,
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
        if let Some(script) = &self.verify_script {
            let canonical = script
                .parent()
                .ok_or(InitError::UnsafeOutput)?
                .canonicalize()?;
            if !canonical.starts_with(&self.context)
                || script
                    .symlink_metadata()
                    .is_ok_and(|metadata| metadata.file_type().is_symlink())
            {
                return Err(InitError::UnsafeOutput);
            }
            std::fs::write(script, VERIFY_UI_SCRIPT)?;
        }
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
        let output = sandbox
            .build_image(&self.context, &self.dockerfile, &self.image, BUILD_TIMEOUT)
            .await?;
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            let tail: String = stderr
                .lines()
                .rev()
                .take(20)
                .collect::<Vec<_>>()
                .into_iter()
                .rev()
                .collect::<Vec<_>>()
                .join("\n");
            return Err(InitError::Build(tail));
        }
        self.record_build()?;
        Ok(())
    }

    /// Where the fingerprint of the last successful build is kept, next to the
    /// generated Dockerfile.
    pub fn state_path(&self) -> Result<PathBuf, InitError> {
        let name = self
            .dockerfile
            .file_name()
            .and_then(|name| name.to_str())
            .ok_or(InitError::UnsafeOutput)?;
        Ok(self.dockerfile.with_file_name(format!("{name}.state.json")))
    }

    /// Hashes every input that decides what the image contains: the generated
    /// Dockerfile, the image tag and the dependency manifests and lockfiles of
    /// the detected ecosystem. Only file bytes are read; nothing is executed.
    pub fn fingerprint(&self) -> Result<ImageState, InitError> {
        let mut inputs = BTreeMap::new();
        for relative in dependency_inputs(&self.context, self.ecosystem)? {
            let path = self.context.join(&relative);
            // Stream large manifests instead of hashing only their length:
            // two different oversized lockfiles can have the same size.
            let mut file = std::fs::File::open(&path)?;
            let mut hasher = Sha256::new();
            let mut buffer = [0u8; 8192];
            loop {
                let read = std::io::Read::read(&mut file, &mut buffer)?;
                if read == 0 {
                    break;
                }
                hasher.update(&buffer[..read]);
            }
            let digest = hex(&hasher.finalize());
            inputs.insert(relative, digest);
        }
        let mut hasher = Sha256::new();
        hasher.update(b"ryoko-sandbox-fingerprint-v1\0");
        hasher.update(self.ecosystem.label().as_bytes());
        hasher.update(b"\0");
        hasher.update(self.image.as_bytes());
        hasher.update(b"\0");
        hasher.update(Sha256::digest(self.dockerfile_contents.as_bytes()));
        for (path, digest) in &inputs {
            hasher.update(path.as_bytes());
            hasher.update(b"\0");
            hasher.update(digest.as_bytes());
            hasher.update(b"\0");
        }
        Ok(ImageState {
            version: STATE_VERSION,
            ecosystem: self.ecosystem,
            image: self.image.clone(),
            dockerfile_sha256: hex(&Sha256::digest(self.dockerfile_contents.as_bytes())),
            fingerprint: hex(&hasher.finalize()),
            inputs,
        })
    }

    /// Compares the current inputs with the last successful build. A missing,
    /// unreadable or foreign state file means the image has never been built
    /// from this plan, so it is reported as such rather than trusted.
    pub fn freshness(&self) -> Result<Freshness, InitError> {
        let current = self.fingerprint()?;
        let path = self.state_path()?;
        let recorded = match std::fs::symlink_metadata(&path) {
            Ok(metadata) if metadata.is_file() && metadata.len() <= MAX_STATE_BYTES => {
                std::fs::read(&path)
                    .ok()
                    .and_then(|bytes| serde_json::from_slice::<ImageState>(&bytes).ok())
            }
            _ => None,
        };
        let Some(recorded) = recorded.filter(|state| state.version == STATE_VERSION) else {
            return Ok(Freshness::NeverBuilt);
        };
        if recorded.fingerprint == current.fingerprint {
            return Ok(Freshness::Fresh);
        }
        let mut changed: BTreeSet<String> = BTreeSet::new();
        for (path, digest) in &current.inputs {
            if recorded.inputs.get(path) != Some(digest) {
                changed.insert(path.clone());
            }
        }
        for path in recorded.inputs.keys() {
            if !current.inputs.contains_key(path) {
                changed.insert(path.clone());
            }
        }
        if recorded.dockerfile_sha256 != current.dockerfile_sha256 {
            changed.insert(self.relative_dockerfile());
        }
        if recorded.image != current.image {
            changed.insert("image tag".to_owned());
        }
        Ok(Freshness::Stale {
            changed: changed.into_iter().collect(),
        })
    }

    /// Records the current fingerprint as the last successful build. Called by
    /// [`Self::build`]; exposed so an image built by other means can be adopted
    /// explicitly.
    pub fn record_build(&self) -> Result<(), InitError> {
        let path = self.state_path()?;
        let parent = path.parent().ok_or(InitError::UnsafeOutput)?;
        if !parent.canonicalize()?.starts_with(&self.context)
            || path
                .symlink_metadata()
                .is_ok_and(|metadata| metadata.file_type().is_symlink())
        {
            return Err(InitError::UnsafeOutput);
        }
        let state = self.fingerprint()?;
        let mut bytes = serde_json::to_vec_pretty(&state)
            .map_err(|error| InitError::Io(std::io::Error::other(error)))?;
        bytes.push(b'\n');
        std::fs::write(path, bytes)?;
        Ok(())
    }

    fn relative_dockerfile(&self) -> String {
        self.dockerfile
            .strip_prefix(&self.context)
            .unwrap_or(&self.dockerfile)
            .to_string_lossy()
            .replace('\\', "/")
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

/// Reads `package.json` and reports the frontend tooling it declares. A
/// missing, oversized or malformed manifest yields an empty profile instead of
/// failing, because detection is an optimisation, not a gate.
pub fn detect_frontend(root: &Path) -> FrontendProfile {
    let Ok(metadata) = std::fs::metadata(root.join("package.json")) else {
        return FrontendProfile::default();
    };
    if !metadata.is_file() || metadata.len() > MAX_MANIFEST_BYTES {
        return FrontendProfile::default();
    }
    let Ok(bytes) = std::fs::read(root.join("package.json")) else {
        return FrontendProfile::default();
    };
    let Ok(manifest) = serde_json::from_slice::<serde_json::Value>(&bytes) else {
        return FrontendProfile::default();
    };
    let mut declared = BTreeSet::new();
    for section in ["dependencies", "devDependencies", "peerDependencies"] {
        if let Some(map) = manifest.get(section).and_then(serde_json::Value::as_object) {
            for name in map.keys() {
                declared.insert(name.trim().to_ascii_lowercase());
            }
        }
    }
    let mut frameworks: Vec<String> = Vec::new();
    for (dependency, label) in [
        ("react", "react"),
        ("react-dom", "react"),
        ("next", "next"),
        ("vite", "vite"),
        ("svelte", "svelte"),
        ("@sveltejs/kit", "svelte"),
        ("vue", "vue"),
        ("tailwindcss", "tailwind"),
    ] {
        if declared.contains(dependency) && !frameworks.iter().any(|found| found == label) {
            frameworks.push(label.to_owned());
        }
    }
    frameworks.sort();
    let playwright = declared.contains("playwright") || declared.contains("@playwright/test");
    FrontendProfile {
        browser_stack: playwright || !frameworks.is_empty(),
        frameworks,
        playwright,
    }
}

fn browser_layers(profile: &FrontendProfile) -> String {
    let mut layers = String::from(
        "\
# Headless Chromium is baked in: containers run with --network=none and a
# read-only root filesystem, so nothing can be downloaded or installed later.
ENV PLAYWRIGHT_BROWSERS_PATH=/ms-playwright
# Minimal runtime libraries plus fonts, so text metrics match between runs.
RUN apt-get update \\
    && apt-get install -y --no-install-recommends \\
        libnss3 libatk1.0-0 libatk-bridge2.0-0 libgbm1 libasound2 fonts-liberation \\
    && rm -rf /var/lib/apt/lists/*
",
    );
    if profile.playwright {
        layers.push_str("RUN npx playwright install --with-deps chromium \\\n    && chmod -R o+rx /ms-playwright\n");
    } else {
        // No Playwright dependency is declared. Install the runner in its own
        // directory so the project's dependency tree stays untouched.
        layers.push_str(
            "\
WORKDIR /opt/playwright
RUN npm init -y > /dev/null && npm install --no-fund --no-audit playwright
RUN npx playwright install --with-deps chromium \\
    && chmod -R o+rx /ms-playwright
",
        );
    }
    layers.push_str(&format!(
        "\
# Deterministic verification harness: the same screenshots, ARIA tree and
# console-error capture on every run, with no agent-written browser script.
COPY {VERIFY_UI_PROJECT_PATH} {VERIFY_UI_CONTAINER_PATH}
ENV RYOKO_VERIFY_UI={VERIFY_UI_CONTAINER_PATH}
"
    ));
    layers
}

fn node_dockerfile(frontend: &FrontendProfile) -> String {
    let mut contents = String::from(
        "\
# Generated by `ryoko sandbox init`. Review before building.
FROM node:22-bookworm-slim
WORKDIR /opt/ryoko
COPY package*.json ./
RUN if [ -f package-lock.json ]; then npm ci; else npm install; fi
ENV NODE_PATH=/opt/ryoko/node_modules
ENV PATH=/opt/ryoko/node_modules/.bin:$PATH
ENV npm_config_offline=true
",
    );
    if frontend.browser_stack {
        contents.push_str(&browser_layers(frontend));
    }
    contents.push_str("WORKDIR /workspace\n");
    contents
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

const STATE_VERSION: u32 = 1;
const MAX_STATE_BYTES: u64 = 1024 * 1024;
/// Bounds the manifest walk for Rust workspaces so a huge tree cannot stall it.
const MAX_WALK_DEPTH: usize = 32;
const MAX_WALK_ENTRIES: usize = 20_000;
const SKIPPED_DIRS: &[&str] = &[".git", "target", "node_modules", "dist", ".ryoko"];

/// Fingerprint of the inputs an image was built from.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ImageState {
    pub version: u32,
    pub ecosystem: Ecosystem,
    pub image: String,
    pub dockerfile_sha256: String,
    pub fingerprint: String,
    /// Repository-relative path (always `/`-separated) to its SHA-256.
    pub inputs: BTreeMap<String, String>,
}

/// Whether the sandbox image still matches the repository's dependencies.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum Freshness {
    /// No successful build has been recorded for this plan.
    NeverBuilt,
    Fresh,
    /// Inputs changed since the last build; `changed` names them.
    Stale {
        changed: Vec<String>,
    },
}

impl Freshness {
    pub fn needs_build(&self) -> bool {
        !matches!(self, Self::Fresh)
    }
}

/// The manifests and lockfiles that decide an ecosystem's dependency set.
/// Symlinks are skipped so the fingerprint never follows a link out of the
/// repository.
fn dependency_inputs(root: &Path, ecosystem: Ecosystem) -> Result<Vec<String>, InitError> {
    let fixed: &[&str] = match ecosystem {
        Ecosystem::Rust => &[],
        Ecosystem::Node => &["package.json", "package-lock.json", "npm-shrinkwrap.json"],
        Ecosystem::Python => &[
            "pyproject.toml",
            "poetry.lock",
            "uv.lock",
            "requirements.txt",
            "setup.cfg",
            "setup.py",
        ],
    };
    let mut found: Vec<String> = fixed
        .iter()
        .filter(|name| {
            std::fs::symlink_metadata(root.join(name)).is_ok_and(|metadata| metadata.is_file())
        })
        .map(|name| (*name).to_owned())
        .collect();
    if ecosystem == Ecosystem::Rust {
        let mut stack = vec![(root.to_path_buf(), 0usize)];
        let mut visited = 0usize;
        while let Some((dir, depth)) = stack.pop() {
            for entry in std::fs::read_dir(&dir)? {
                let entry = entry?;
                visited += 1;
                if visited > MAX_WALK_ENTRIES {
                    return Err(InitError::Io(std::io::Error::other(
                        "dependency scan exceeded the directory entry limit",
                    )));
                }
                let file_type = entry.file_type()?;
                let name = entry.file_name();
                let name = name.to_string_lossy();
                if file_type.is_dir() && !SKIPPED_DIRS.contains(&name.as_ref()) {
                    if depth >= MAX_WALK_DEPTH {
                        return Err(InitError::Io(std::io::Error::other(
                            "dependency scan exceeded the directory depth limit",
                        )));
                    }
                    stack.push((entry.path(), depth + 1));
                } else if file_type.is_file()
                    && matches!(
                        name.as_ref(),
                        "Cargo.toml" | "Cargo.lock" | "rust-toolchain" | "rust-toolchain.toml"
                    )
                    && let Ok(relative) = entry.path().strip_prefix(root)
                {
                    found.push(relative.to_string_lossy().replace('\\', "/"));
                }
            }
        }
    }
    found.sort();
    found.dedup();
    Ok(found)
}

fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write;
    bytes
        .iter()
        .fold(String::with_capacity(bytes.len() * 2), |mut out, byte| {
            let _ = write!(out, "{byte:02x}");
            out
        })
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
    fn detects_frontend_frameworks_and_playwright_from_the_manifest() {
        let dir = TempDir::new().unwrap();
        std::fs::write(
            dir.path().join("package.json"),
            r#"{
                "dependencies": { "react": "^19.0.0", "react-dom": "^19.0.0" },
                "devDependencies": { "vite": "^5.4.0", "tailwindcss": "^3.4.0",
                                     "@playwright/test": "^1.49.0" }
            }"#,
        )
        .unwrap();
        let profile = detect_frontend(dir.path());
        assert_eq!(profile.frameworks, vec!["react", "tailwind", "vite"]);
        assert!(profile.playwright);
        assert!(profile.browser_stack);
    }

    #[test]
    fn a_frontend_image_bakes_chromium_at_a_fixed_global_path() {
        let dir = TempDir::new().unwrap();
        std::fs::write(
            dir.path().join("package.json"),
            r#"{ "dependencies": { "vue": "^3.4.0" }, "devDependencies": { "vite": "^5" } }"#,
        )
        .unwrap();
        let plan = SandboxPlan::inspect(dir.path(), None, None).unwrap();
        assert_eq!(plan.ecosystem, Ecosystem::Node);
        assert!(plan.frontend.browser_stack);
        let dockerfile = &plan.dockerfile_contents;
        assert!(dockerfile.contains("FROM node:22-bookworm-slim"));
        assert!(dockerfile.contains("ENV PLAYWRIGHT_BROWSERS_PATH=/ms-playwright"));
        for library in [
            "libnss3",
            "libatk-bridge2.0-0",
            "libgbm1",
            "fonts-liberation",
        ] {
            assert!(dockerfile.contains(library), "missing {library}");
        }
        assert!(dockerfile.contains("npx playwright install --with-deps chromium"));
        assert!(dockerfile.contains("chmod -R o+rx /ms-playwright"));
        // Vue declares no Playwright, so the runner is installed out of the way.
        assert!(dockerfile.contains("/opt/playwright"));
        assert!(dockerfile.contains("npm install --no-fund --no-audit playwright"));
        assert!(dockerfile.trim_end().ends_with("WORKDIR /workspace"));
    }

    #[test]
    fn a_declared_playwright_uses_the_project_version() {
        let dir = TempDir::new().unwrap();
        std::fs::write(
            dir.path().join("package.json"),
            r#"{ "devDependencies": { "@playwright/test": "^1.49.0" } }"#,
        )
        .unwrap();
        let plan = SandboxPlan::inspect(dir.path(), None, None).unwrap();
        assert!(
            plan.dockerfile_contents
                .contains("npx playwright install --with-deps chromium")
        );
        assert!(!plan.dockerfile_contents.contains("/opt/playwright"));
    }

    #[test]
    fn a_non_frontend_node_project_gets_no_browser_layer() {
        let dir = TempDir::new().unwrap();
        std::fs::write(
            dir.path().join("package.json"),
            r#"{ "dependencies": { "express": "^4" } }"#,
        )
        .unwrap();
        let plan = SandboxPlan::inspect(dir.path(), None, None).unwrap();
        assert!(!plan.frontend.browser_stack);
        assert!(!plan.dockerfile_contents.contains("ms-playwright"));
        assert!(!plan.dockerfile_contents.contains("apt-get"));
    }

    #[test]
    fn a_malformed_or_oversized_manifest_is_ignored() {
        let dir = TempDir::new().unwrap();
        std::fs::write(dir.path().join("package.json"), "not json").unwrap();
        assert_eq!(detect_frontend(dir.path()), FrontendProfile::default());
        let plan = SandboxPlan::inspect(dir.path(), None, None).unwrap();
        assert!(!plan.dockerfile_contents.contains("ms-playwright"));

        let huge = format!(
            "{{\"dependencies\":{{\"react\":\"{}\"}}}}",
            "x".repeat(600_000)
        );
        std::fs::write(dir.path().join("package.json"), huge).unwrap();
        assert_eq!(detect_frontend(dir.path()), FrontendProfile::default());
    }

    fn node_project(bytes: &str) -> TempDir {
        let dir = TempDir::new().unwrap();
        std::fs::write(dir.path().join("package.json"), bytes).unwrap();
        dir
    }

    fn run_node(dir: &Path, args: &[&str]) -> std::process::Output {
        std::process::Command::new("node")
            .current_dir(dir)
            .args(args)
            .output()
            .expect("node must be installed to verify the bundled harness")
    }

    #[test]
    fn the_bundled_harness_is_valid_javascript() {
        let dir = TempDir::new().unwrap();
        let script = dir.path().join("verify-ui.mjs");
        std::fs::write(&script, verify_ui_script()).unwrap();
        let output = run_node(dir.path(), &["--check", script.to_str().unwrap()]);
        assert!(
            output.status.success(),
            "node --check failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    #[test]
    fn the_harness_reports_its_artifact_plan_without_a_browser() {
        let dir = TempDir::new().unwrap();
        let script = dir.path().join("verify-ui.mjs");
        std::fs::write(&script, verify_ui_script()).unwrap();
        let output = run_node(
            dir.path(),
            &[
                script.to_str().unwrap(),
                "--check-config",
                "--dev-command",
                "npm run preview -- --port 4173",
                "--routes",
                "/,/about",
                "--out",
                "/ryoko-artifacts",
            ],
        );
        assert!(
            output.status.success(),
            "check-config failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        let plan: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(plan["routes"][0]["directory"], "/ryoko-artifacts/root");
        assert_eq!(plan["routes"][1]["directory"], "/ryoko-artifacts/about");
        let artifacts = plan["routes"][0]["artifacts"].as_array().unwrap();
        for name in [
            "desktop.png",
            "mobile.png",
            "aria-tree.json",
            "console-errors.json",
        ] {
            assert!(
                artifacts.iter().any(|value| value == name),
                "missing {name}"
            );
        }
        assert_eq!(plan["viewports"]["desktop"]["width"], 1440);
        assert_eq!(plan["viewports"]["mobile"]["width"], 390);
    }

    #[test]
    fn the_harness_refuses_to_leave_the_loopback_interface() {
        let dir = TempDir::new().unwrap();
        let script = dir.path().join("verify-ui.mjs");
        std::fs::write(&script, verify_ui_script()).unwrap();
        let output = run_node(
            dir.path(),
            &[
                script.to_str().unwrap(),
                "--check-config",
                "--dev-command",
                "npm run preview",
                "--url",
                "https://example.com",
            ],
        );
        assert_eq!(output.status.code(), Some(2));
        assert!(
            String::from_utf8_lossy(&output.stderr).contains("local server"),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    /// Exercises the two lifecycle guarantees that need no browser: readiness
    /// polling against a real server, and killing the whole process group.
    #[test]
    fn the_harness_waits_for_the_server_and_kills_its_process_group() {
        const DRIVER: &str = r#"
import { spawn } from "node:child_process";
import { createServer } from "node:http";
import { readFileSync } from "node:fs";
import { waitForServer, stopServer } from "./verify-ui.mjs";

const server = createServer((_, res) => res.end("ok"));
await new Promise((resolve) => server.listen(0, "127.0.0.1", resolve));
const { port } = server.address();
const ready = await waitForServer(`http://127.0.0.1:${port}/`, 5000);
server.close();
if (!ready.ready) {
  console.error("server was never reported ready");
  process.exit(1);
}
const closed = await waitForServer(`http://127.0.0.1:${port}/`, 1500);
if (closed.ready) {
  console.error("a closed port was reported ready");
  process.exit(1);
}

const child = spawn("/bin/sh", ["-c", "sleep 30 & echo $! > grandchild.pid; wait"], {
  detached: true,
  stdio: "ignore",
});
// Poll instead of sleeping: under a loaded test runner the shell may take
// longer than any fixed delay to reach the write.
let grandchild = 0;
for (let attempt = 0; attempt < 100 && !grandchild; attempt += 1) {
  try {
    grandchild = Number(readFileSync("grandchild.pid", "utf8").trim());
  } catch {
    grandchild = 0;
  }
  if (!grandchild) await new Promise((resolve) => setTimeout(resolve, 50));
}
if (!grandchild) {
  console.error("the fake server never wrote its pid");
  process.exit(1);
}
await stopServer(child);
// The kill is a promise the kernel keeps on its own schedule, so a single probe
// right after stopServer races signal delivery and reaping. Bounded polling
// still fails the test if the grandchild really survived.
let alive = true;
for (let attempt = 0; attempt < 80 && alive; attempt += 1) {
  try {
    process.kill(grandchild, 0);
    await new Promise((resolve) => setTimeout(resolve, 25));
  } catch {
    alive = false;
  }
}
if (alive) {
  console.error("the grandchild process survived the process-group kill");
  process.exit(1);
}
console.log("ok");
"#;
        let dir = TempDir::new().unwrap();
        std::fs::write(dir.path().join("verify-ui.mjs"), verify_ui_script()).unwrap();
        std::fs::write(dir.path().join("driver.mjs"), DRIVER).unwrap();
        let output = run_node(dir.path(), &["driver.mjs"]);
        assert!(
            output.status.success(),
            "harness lifecycle failed: stdout={} stderr={}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(String::from_utf8_lossy(&output.stdout).trim(), "ok");
    }

    #[test]
    fn a_frontend_image_ships_the_harness_at_a_fixed_path() {
        let dir = node_project(r#"{ "devDependencies": { "vite": "^5" } }"#);
        let plan = SandboxPlan::inspect(dir.path(), None, None).unwrap();
        assert!(
            plan.dockerfile_contents
                .contains("COPY .ryoko/sandbox/verify-ui.mjs /opt/ryoko/verify-ui.mjs")
        );
        assert!(
            plan.dockerfile_contents
                .contains("ENV RYOKO_VERIFY_UI=/opt/ryoko/verify-ui.mjs")
        );
        plan.write().unwrap();
        let script = plan.verify_script.as_ref().unwrap();
        assert!(script.is_file());
        assert!(
            std::fs::read_to_string(script)
                .unwrap()
                .contains("console-errors.json")
        );
    }

    #[test]
    fn a_non_frontend_project_gets_no_harness_or_copy() {
        let dir = node_project(r#"{ "dependencies": { "express": "^4" } }"#);
        let plan = SandboxPlan::inspect(dir.path(), None, None).unwrap();
        assert!(plan.verify_script.is_none());
        assert!(!plan.dockerfile_contents.contains("verify-ui.mjs"));
        plan.write().unwrap();
        assert!(!dir.path().join(".ryoko/sandbox/verify-ui.mjs").exists());
    }

    #[test]
    fn the_build_context_ignores_artifacts_and_worktrees() {
        let dir = TempDir::new().unwrap();
        std::fs::write(dir.path().join("Cargo.toml"), "[package]\nname=\"x\"\n").unwrap();
        let ignore = SandboxPlan::inspect(dir.path(), None, None)
            .unwrap()
            .ignore_contents;
        assert!(ignore.lines().any(|line| line == ".ryoko/artifacts"));
        assert!(ignore.lines().any(|line| line == ".ryoko/worktrees"));
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
        // inspect() canonicalizes the repository root: /var and /private/var
        // refer to the same location on macOS but are not lexical prefixes.
        assert!(plan.dockerfile.starts_with(dir.path().canonicalize().unwrap()));
    }

    fn rust_project() -> TempDir {
        let dir = TempDir::new().unwrap();
        std::fs::write(
            dir.path().join("Cargo.toml"),
            "[workspace]\nmembers = [\"crates/a\"]\n",
        )
        .unwrap();
        std::fs::write(dir.path().join("Cargo.lock"), "version = 4\n").unwrap();
        std::fs::create_dir_all(dir.path().join("crates/a/src")).unwrap();
        std::fs::write(
            dir.path().join("crates/a/Cargo.toml"),
            "[package]\nname=\"a\"\n",
        )
        .unwrap();
        std::fs::write(dir.path().join("crates/a/src/lib.rs"), "").unwrap();
        dir
    }

    #[test]
    fn an_unbuilt_plan_needs_a_build_and_a_recorded_one_is_fresh() {
        let dir = rust_project();
        let plan = SandboxPlan::inspect(dir.path(), None, None).unwrap();
        plan.write().unwrap();
        assert_eq!(plan.freshness().unwrap(), Freshness::NeverBuilt);
        plan.record_build().unwrap();
        assert_eq!(plan.freshness().unwrap(), Freshness::Fresh);
        let state = plan.fingerprint().unwrap();
        let inputs: Vec<&str> = state.inputs.keys().map(String::as_str).collect();
        assert_eq!(
            inputs,
            vec!["Cargo.lock", "Cargo.toml", "crates/a/Cargo.toml"]
        );
    }

    #[test]
    fn a_lockfile_or_member_manifest_change_marks_the_image_stale() {
        let dir = rust_project();
        let plan = SandboxPlan::inspect(dir.path(), None, None).unwrap();
        plan.write().unwrap();
        plan.record_build().unwrap();
        std::fs::write(dir.path().join("Cargo.lock"), "version = 4\n# bumped\n").unwrap();
        std::fs::write(
            dir.path().join("crates/a/Cargo.toml"),
            "[package]\nname=\"a\"\n[dependencies]\nserde = \"1\"\n",
        )
        .unwrap();
        assert_eq!(
            plan.freshness().unwrap(),
            Freshness::Stale {
                changed: vec!["Cargo.lock".to_owned(), "crates/a/Cargo.toml".to_owned()]
            }
        );
        // Source edits are not dependency inputs.
        plan.record_build().unwrap();
        std::fs::write(dir.path().join("crates/a/src/lib.rs"), "pub fn x() {}").unwrap();
        assert_eq!(plan.freshness().unwrap(), Freshness::Fresh);
    }

    #[test]
    fn a_new_tag_a_removed_lockfile_or_a_tampered_state_is_not_fresh() {
        let dir = node_project(r#"{ "dependencies": { "express": "^4" } }"#);
        std::fs::write(dir.path().join("package-lock.json"), "{}").unwrap();
        let plan = SandboxPlan::inspect(dir.path(), None, None).unwrap();
        plan.write().unwrap();
        plan.record_build().unwrap();

        let retagged = SandboxPlan::inspect(dir.path(), Some("other:1".to_owned()), None).unwrap();
        assert_eq!(
            retagged.freshness().unwrap(),
            Freshness::Stale {
                changed: vec!["image tag".to_owned()]
            }
        );

        std::fs::remove_file(dir.path().join("package-lock.json")).unwrap();
        assert_eq!(
            plan.freshness().unwrap(),
            Freshness::Stale {
                changed: vec!["package-lock.json".to_owned()]
            }
        );

        std::fs::write(plan.state_path().unwrap(), "not json").unwrap();
        assert_eq!(plan.freshness().unwrap(), Freshness::NeverBuilt);
    }

    #[test]
    fn nested_manifests_are_tracked_and_scan_limits_fail_closed() {
        let dir = rust_project();
        let nested = dir.path().join("crates/a/src/deep/one/two/three/four");
        std::fs::create_dir_all(&nested).unwrap();
        let manifest = nested.join("Cargo.toml");
        std::fs::write(&manifest, "[package]\nname=\"nested\"\n").unwrap();
        let plan = SandboxPlan::inspect(dir.path(), None, None).unwrap();
        plan.write().unwrap();
        plan.record_build().unwrap();
        std::fs::write(&manifest, "[package]\nname=\"changed\"\n").unwrap();
        assert!(matches!(plan.freshness(), Ok(Freshness::Stale { .. })));

        // A deeper tree must error rather than silently become "fresh".
        let mut deep = nested;
        for _ in 0..MAX_WALK_DEPTH {
            deep.push("child");
        }
        std::fs::create_dir_all(deep).unwrap();
        assert!(plan.fingerprint().is_err());
    }

    #[test]
    fn oversized_lockfile_changes_of_identical_length_are_detected() {
        let dir = rust_project();
        let path = dir.path().join("Cargo.lock");
        let mut contents = vec![b'a'; 16 * 1024 * 1024 + 1];
        std::fs::write(&path, &contents).unwrap();
        let plan = SandboxPlan::inspect(dir.path(), None, None).unwrap();
        plan.write().unwrap();
        plan.record_build().unwrap();
        assert_eq!(plan.freshness().unwrap(), Freshness::Fresh);

        contents[0] = b'b';
        std::fs::write(path, &contents).unwrap();
        assert_eq!(
            plan.freshness().unwrap(),
            Freshness::Stale {
                changed: vec!["Cargo.lock".to_owned()]
            }
        );
    }

    #[cfg(unix)]
    #[test]
    fn the_fingerprint_never_follows_symlinks_out_of_the_repository() {
        let outside = TempDir::new().unwrap();
        std::fs::write(
            outside.path().join("Cargo.toml"),
            "[package]\nname=\"secret\"\n",
        )
        .unwrap();
        let dir = rust_project();
        std::os::unix::fs::symlink(outside.path(), dir.path().join("linked")).unwrap();
        let plan = SandboxPlan::inspect(dir.path(), None, None).unwrap();
        let state = plan.fingerprint().unwrap();
        assert!(state.inputs.keys().all(|path| !path.starts_with("linked")));

        plan.write().unwrap();
        let state_path = plan.state_path().unwrap();
        std::os::unix::fs::symlink(outside.path().join("stolen.json"), &state_path).unwrap();
        assert!(matches!(plan.record_build(), Err(InitError::UnsafeOutput)));
        assert!(!outside.path().join("stolen.json").exists());
    }
}
