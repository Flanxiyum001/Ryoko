//! WASM execution guardrail for agent-proposed changes.
//!
//! Test artifacts run in a fresh wasmtime store with no filesystem, network or
//! environment preopens. WASI imports are available, but the empty context
//! denies host resources by default. Fuel bounds CPU, an epoch deadline bounds
//! wall time, and a `StoreLimits` cap bounds linear memory. Only a clean
//! `_start` return counts as a pass. [`SandboxSpecValidator`] additionally
//! refuses proposals that do not carry tests, so source-only changes never
//! reach review.
//!
//! Unix hosts evaluate in-process; Windows delegates every evaluation to the
//! isolated `wasm-guardrail-worker` process because Wasmtime 26 guest traps
//! there can abort the host. The worker receives one typed request and replies
//! with one typed response and is spawned with a cleared environment; see
//! the [`worker`] module.

mod evaluate;
mod init;
mod process;
mod protocol;
mod spec;
pub mod worker;

pub use init::{
    Ecosystem, Freshness, FrontendProfile, ImageState, InitError, SandboxPlan,
    VERIFY_UI_CONTAINER_PATH, verify_ui_script,
};
pub use process::{ARTIFACT_MOUNT, BuildBackend, BuildSandbox, BuildSandboxError};

pub use spec::{SandboxSpecValidator, SpecError, SpecEvidence, SpecPolicy};

use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use async_trait::async_trait;
use ryoko_core::validation::{ChangeValidator, StagedChanges, ValidationReport};
use ryoko_core::{CoreError, CoreResult};
use wasmtime::Engine;

#[derive(Debug, Clone)]
pub struct SandboxLimits {
    pub fuel: u64,
    pub timeout: Duration,
    pub max_memory_bytes: usize,
}

impl Default for SandboxLimits {
    fn default() -> Self {
        Self {
            fuel: 50_000_000,
            timeout: Duration::from_secs(30),
            max_memory_bytes: 256 * 1024 * 1024,
        }
    }
}

#[derive(Debug, Clone)]
pub struct SandboxOutcome {
    pub passed: bool,
    pub fuel_consumed: u64,
    pub duration_ms: u64,
    pub message: String,
}

#[derive(Debug, Clone, thiserror::Error)]
pub enum SandboxError {
    #[error("could not configure the WASM engine: {0}")]
    Engine(String),
    #[error("invalid or unsupported WASM artifact: {0}")]
    Compile(String),
    #[error("sandbox instantiation failed: {0}")]
    Instantiate(String),
    #[error("sandbox execution failed: {0}")]
    Execution(String),
    #[error("sandbox task panicked: {0}")]
    Panic(String),
    #[error("guardrail worker process failed: {0}")]
    Worker(String),
}

impl From<SandboxError> for CoreError {
    fn from(value: SandboxError) -> Self {
        CoreError::Sandbox(value.to_string())
    }
}

/// Where guardrail evaluation runs. Unix hosts default to in-process
/// execution; Windows must delegate to the isolated worker binary and fails
/// closed when it cannot be resolved.
#[derive(Clone)]
enum SandboxStrategy {
    InProcess(Engine),
    Worker(PathBuf),
}

/// A wasmtime execution boundary with empty WASI state and no host preopens.
#[derive(Clone)]
pub struct WasmSandbox {
    strategy: SandboxStrategy,
    limits: SandboxLimits,
}

impl std::fmt::Debug for WasmSandbox {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WasmSandbox")
            .field("limits", &self.limits)
            .finish_non_exhaustive()
    }
}

impl WasmSandbox {
    /// Native Windows Wasmtime 26 guest traps can abort the process that hosts
    /// them (0xc0000409), including when signals-based traps are disabled, so
    /// guardrail evaluation never runs in-process on Windows. It is delegated
    /// to a dedicated `wasm-guardrail-worker` process spawned with a cleared
    /// environment and no inherited host capabilities; a guest trap, deadline
    /// overrun or worker crash becomes a bounded failed validation instead of
    /// a host abort. When the worker binary cannot be resolved this path keeps
    /// failing closed. Docker/Podman project checks use a separate
    /// container-backed runner.
    #[cfg(windows)]
    pub fn new(limits: SandboxLimits) -> Result<Self, SandboxError> {
        if let Ok(override_path) = std::env::var("RYOKO_WASM_WORKER_BIN") {
            let candidate = PathBuf::from(override_path);
            if candidate.is_file() {
                return Ok(Self {
                    strategy: SandboxStrategy::Worker(candidate),
                    limits,
                });
            }
        }
        if let Ok(current) = std::env::current_exe() {
            if let Some(directory) = current.parent() {
                let candidate = directory.join(format!(
                    "{WORKER_BINARY_NAME}{}",
                    std::env::consts::EXE_SUFFIX
                ));
                if candidate.is_file() {
                    return Ok(Self {
                        strategy: SandboxStrategy::Worker(candidate),
                        limits,
                    });
                }
            }
        }
        Err(SandboxError::Engine(
            "no isolated WASM guardrail worker found (install wasm-guardrail-worker next to the application or set RYOKO_WASM_WORKER_BIN); refusing in-process WASM execution"
                .into(),
        ))
    }

    #[cfg(not(windows))]
    pub fn new(limits: SandboxLimits) -> Result<Self, SandboxError> {
        let engine = evaluate::build_engine()?;
        Ok(Self {
            strategy: SandboxStrategy::InProcess(engine),
            limits,
        })
    }

    /// Evaluates through the worker binary at `worker_path` on any platform.
    /// Windows derives the path automatically in [`WasmSandbox::new`]; other
    /// platforms default to in-process evaluation and can opt in explicitly.
    pub fn new_with_worker(
        limits: SandboxLimits,
        worker_path: impl Into<PathBuf>,
    ) -> Result<Self, SandboxError> {
        Ok(Self {
            strategy: SandboxStrategy::Worker(worker_path.into()),
            limits,
        })
    }

    pub fn limits(&self) -> &SandboxLimits {
        &self.limits
    }

    /// Executes a core wasm module's `_start` export under the configured
    /// isolation boundary.
    pub async fn execute(&self, artifact: &[u8]) -> Result<SandboxOutcome, SandboxError> {
        match &self.strategy {
            SandboxStrategy::Worker(worker_path) => {
                worker::execute_in_worker(worker_path, &self.limits, artifact).await
            }
            SandboxStrategy::InProcess(engine) => {
                let engine = engine.clone();
                let limits = self.limits.clone();
                let artifact = artifact.to_vec();
                tokio::task::spawn_blocking(move || {
                    evaluate::evaluate_module(engine, &artifact, &limits)
                })
                .await
                .map_err(|err| SandboxError::Panic(err.to_string()))?
            }
        }
    }
}

/// File name (without platform suffix) of the worker binary spawned next to
/// the application executable on Windows.
#[cfg(windows)]
const WORKER_BINARY_NAME: &str = "wasm-guardrail-worker";

/// Supplies the wasm artifact that exercises a task's staged changes.
/// Production implementations compile the isolated worktree to a WASM test
/// harness; tests can supply a literal module.
#[async_trait]
pub trait TestArtifact: Send + Sync {
    async fn build(&self, changes: &StagedChanges) -> CoreResult<Vec<u8>>;
}

#[derive(Debug, Clone)]
pub struct StaticArtifact {
    module: Arc<Vec<u8>>,
}

impl StaticArtifact {
    pub fn new(module: impl Into<Vec<u8>>) -> Self {
        Self {
            module: Arc::new(module.into()),
        }
    }
}

#[async_trait]
impl TestArtifact for StaticArtifact {
    async fn build(&self, _changes: &StagedChanges) -> CoreResult<Vec<u8>> {
        Ok(self.module.as_ref().clone())
    }
}

/// [`ChangeValidator`] that refuses to publish anything unless its WASM test
/// artifact exits cleanly. Every compile, trap, timeout and fuel exhaustion is
/// converted into a failed validation report, never a silent pass.
pub struct SandboxValidator {
    sandbox: WasmSandbox,
    artifact: Arc<dyn TestArtifact>,
}

impl SandboxValidator {
    pub fn new(sandbox: WasmSandbox, artifact: Arc<dyn TestArtifact>) -> Self {
        Self { sandbox, artifact }
    }

    pub fn static_module(module: impl Into<Vec<u8>>) -> Result<Self, SandboxError> {
        Ok(Self::new(
            WasmSandbox::new(SandboxLimits::default())?,
            Arc::new(StaticArtifact::new(module)),
        ))
    }
}

#[async_trait]
impl ChangeValidator for SandboxValidator {
    async fn validate(&self, changes: &StagedChanges) -> CoreResult<ValidationReport> {
        let started = Instant::now();
        let artifact = match self.artifact.build(changes).await {
            Ok(artifact) => artifact,
            Err(err) => {
                return Ok(ValidationReport::failed(
                    "could not build sandbox test artifact",
                    vec![err.to_string()],
                    started.elapsed(),
                ));
            }
        };

        match self.sandbox.execute(&artifact).await {
            Ok(outcome) => Ok(ValidationReport::passed(
                outcome.message.clone(),
                vec![outcome.message],
                Duration::from_millis(outcome.duration_ms),
            )),
            Err(err) => Ok(ValidationReport::failed(
                "WASM guardrail rejected the proposed changes",
                vec![err.to_string()],
                started.elapsed(),
            )),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ryoko_core::ProposedChange;
    use uuid::Uuid;

    fn changes() -> StagedChanges {
        StagedChanges::new(
            Uuid::new_v4(),
            vec![ProposedChange::new(
                "src/lib.rs",
                "pub fn answer() -> u32 { 42 }",
            )],
        )
    }

    #[cfg(windows)]
    #[test]
    fn windows_fails_closed_without_a_resolvable_guardrail_worker() {
        let error = WasmSandbox::new(SandboxLimits::default()).unwrap_err();
        assert!(
            error
                .to_string()
                .contains("refusing in-process WASM execution")
        );
        assert!(SandboxValidator::static_module(b"(module)").is_err());
    }

    #[cfg(not(windows))]
    #[tokio::test]
    async fn executes_clean_module() {
        let sandbox = WasmSandbox::new(SandboxLimits::default()).unwrap();
        let outcome = sandbox
            .execute(br#"(module (func (export "_start")))"#)
            .await
            .unwrap();
        assert!(outcome.passed);
        assert!(outcome.fuel_consumed > 0);
    }

    #[cfg(not(windows))]
    #[tokio::test]
    async fn traps_fail_the_guardrail() {
        let validator =
            SandboxValidator::static_module(br#"(module (func (export "_start") unreachable))"#)
                .unwrap();
        let report = validator.validate(&changes()).await.unwrap();
        assert!(!report.passed);
        assert!(report.logs.join(" ").contains("execution failed"));
    }

    #[cfg(not(windows))]
    #[tokio::test]
    async fn infinite_loops_are_stopped_by_fuel() {
        let sandbox = WasmSandbox::new(SandboxLimits {
            fuel: 100_000,
            timeout: Duration::from_secs(5),
            max_memory_bytes: 16 * 1024 * 1024,
        })
        .unwrap();
        let outcome = sandbox
            .execute(br#"(module (func (export "_start") (loop br 0)))"#)
            .await;
        assert!(outcome.is_err());
    }

    #[cfg(not(windows))]
    #[tokio::test]
    async fn memory_limit_is_enforced() {
        let sandbox = WasmSandbox::new(SandboxLimits {
            fuel: 10_000,
            timeout: Duration::from_secs(2),
            max_memory_bytes: 64 * 1024,
        })
        .unwrap();
        let outcome = sandbox
            .execute(br#"(module (memory 10) (func (export "_start")))"#)
            .await;
        assert!(outcome.is_err());
    }

    #[cfg(not(windows))]
    #[tokio::test]
    async fn sandbox_validator_passes_clean_test_artifact() {
        let validator =
            SandboxValidator::static_module(br#"(module (func (export "_start")))"#).unwrap();
        let report = validator.validate(&changes()).await.unwrap();
        assert!(report.passed);
        assert!(report.duration_ms < 5_000);
    }

    #[cfg(not(windows))]
    #[tokio::test]
    async fn wasi_imports_are_available_without_host_preopens() {
        let sandbox = WasmSandbox::new(SandboxLimits {
            fuel: 1_000_000,
            timeout: Duration::from_secs(2),
            max_memory_bytes: 1024 * 1024,
        })
        .unwrap();
        let outcome = sandbox
            .execute(
                br#"(module
                    (import "wasi_snapshot_preview1" "random_get"
                        (func $random_get (param i32 i32) (result i32)))
                    (memory (export "memory") 1)
                    (func (export "_start")
                        (drop (call $random_get (i32.const 0) (i32.const 8))))
                )"#,
            )
            .await
            .unwrap();
        assert!(outcome.passed);
    }
}
