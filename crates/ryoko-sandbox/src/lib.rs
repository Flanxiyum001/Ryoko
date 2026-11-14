//! WASM execution guardrail for agent-proposed changes.
//!
//! Test artifacts run in a fresh wasmtime store with no filesystem, network or
//! environment preopens. WASI imports are available, but the empty context
//! denies host resources by default. Fuel bounds CPU, an epoch deadline bounds
//! wall time, and a `StoreLimits` cap bounds linear memory. Only a clean
//! `_start` return counts as a pass. [`SandboxSpecValidator`] additionally
//! refuses proposals that do not carry tests, so source-only changes never
//! reach review.

mod spec;

pub use spec::{SandboxSpecValidator, SpecError, SpecEvidence, SpecPolicy};

use std::sync::Arc;
use std::time::{Duration, Instant};

use async_trait::async_trait;
use ryoko_core::validation::{ChangeValidator, StagedChanges, ValidationReport};
use ryoko_core::{CoreError, CoreResult};
use wasmtime::{
    Config, Engine, Linker, Module, ResourceLimiter, Store, StoreLimits, StoreLimitsBuilder,
};
use wasmtime_wasi::WasiCtxBuilder;
use wasmtime_wasi::preview1::{self, WasiP1Ctx};

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
}

impl From<SandboxError> for CoreError {
    fn from(value: SandboxError) -> Self {
        CoreError::Sandbox(value.to_string())
    }
}

struct SandboxState {
    wasi: WasiP1Ctx,
    limits: StoreLimits,
}

impl ResourceLimiter for SandboxState {
    fn memory_growing(
        &mut self,
        current: usize,
        desired: usize,
        maximum: Option<usize>,
    ) -> wasmtime::Result<bool> {
        self.limits.memory_growing(current, desired, maximum)
    }

    fn table_growing(
        &mut self,
        current: usize,
        desired: usize,
        maximum: Option<usize>,
    ) -> wasmtime::Result<bool> {
        self.limits.table_growing(current, desired, maximum)
    }

    fn instances(&self) -> usize {
        self.limits.instances()
    }

    fn tables(&self) -> usize {
        self.limits.tables()
    }

    fn memories(&self) -> usize {
        self.limits.memories()
    }
}

/// A reusable wasmtime engine with empty WASI state and no host preopens.
#[derive(Clone)]
pub struct WasmSandbox {
    engine: Engine,
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
    pub fn new(limits: SandboxLimits) -> Result<Self, SandboxError> {
        let mut config = Config::new();
        config.consume_fuel(true);
        config.epoch_interruption(true);
        config.wasm_backtrace_details(wasmtime::WasmBacktraceDetails::Disable);
        let engine = Engine::new(&config).map_err(|err| SandboxError::Engine(err.to_string()))?;
        Ok(Self { engine, limits })
    }

    pub fn limits(&self) -> &SandboxLimits {
        &self.limits
    }

    /// Executes a core wasm module's `_start` export in an isolated store.
    pub async fn execute(&self, artifact: &[u8]) -> Result<SandboxOutcome, SandboxError> {
        let started = Instant::now();
        let engine = self.engine.clone();
        let module =
            Module::new(&engine, artifact).map_err(|err| SandboxError::Compile(err.to_string()))?;
        let limits = self.limits.clone();
        let initial_fuel = limits.fuel;

        // The epoch ticker is the only way to interrupt wasm that ignores or
        // outlives its fuel budget.
        let ticker = {
            let engine = engine.clone();
            let timeout = limits.timeout.max(Duration::from_millis(1));
            tokio::spawn(async move {
                tokio::time::sleep(timeout).await;
                engine.increment_epoch();
            })
        };

        let execution = tokio::task::spawn_blocking(move || {
            let store_limits = StoreLimitsBuilder::new()
                .memory_size(limits.max_memory_bytes)
                .instances(16)
                .tables(16)
                .build();
            let mut store = Store::new(
                &engine,
                SandboxState {
                    wasi: WasiCtxBuilder::new().build_p1(),
                    limits: store_limits,
                },
            );
            store.limiter(|state| state as &mut dyn ResourceLimiter);
            store
                .set_fuel(limits.fuel)
                .map_err(|err| SandboxError::Engine(err.to_string()))?;
            store.set_epoch_deadline(1);

            let mut linker = Linker::new(&engine);
            preview1::add_to_linker_sync(&mut linker, |state: &mut SandboxState| &mut state.wasi)
                .map_err(|err| SandboxError::Instantiate(err.to_string()))?;
            let instance = linker
                .instantiate(&mut store, &module)
                .map_err(|err| SandboxError::Instantiate(err.to_string()))?;
            let start = instance
                .get_typed_func::<(), ()>(&mut store, "_start")
                .map_err(|err| SandboxError::Instantiate(format!("missing `_start`: {err}")))?;
            let call = start.call(&mut store, ());
            let fuel_left = store.get_fuel().unwrap_or(0);
            call.map_err(|err| SandboxError::Execution(err.to_string()))?;
            Ok::<u64, SandboxError>(initial_fuel.saturating_sub(fuel_left))
        })
        .await;

        ticker.abort();
        let fuel_consumed = match execution {
            Ok(result) => result?,
            Err(err) => return Err(SandboxError::Panic(err.to_string())),
        };

        Ok(SandboxOutcome {
            passed: true,
            fuel_consumed,
            duration_ms: started.elapsed().as_millis() as u64,
            message: format!("sandboxed test artifact exited cleanly ({fuel_consumed} fuel units)"),
        })
    }
}

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

    #[tokio::test]
    async fn traps_fail_the_guardrail() {
        let validator =
            SandboxValidator::static_module(br#"(module (func (export "_start") unreachable))"#)
                .unwrap();
        let report = validator.validate(&changes()).await.unwrap();
        assert!(!report.passed);
        assert!(report.logs.join(" ").contains("execution failed"));
    }

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

    #[tokio::test]
    async fn sandbox_validator_passes_clean_test_artifact() {
        let validator =
            SandboxValidator::static_module(br#"(module (func (export "_start")))"#).unwrap();
        let report = validator.validate(&changes()).await.unwrap();
        assert!(report.passed);
        assert!(report.duration_ms < 5_000);
    }

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
