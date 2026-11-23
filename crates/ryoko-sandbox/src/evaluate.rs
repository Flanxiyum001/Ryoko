//! In-process WASM evaluation core shared by the Unix host path and the
//! isolated guardrail worker binary.
//!
//! The store is fresh per artifact with no filesystem, network or environment
//! preopens. WASI imports are available, but the empty context denies host
//! resources by default. Fuel bounds CPU, an epoch deadline bounds wall time,
//! and a `StoreLimits` cap bounds linear memory.

use std::sync::mpsc;
use std::time::{Duration, Instant};

use wasmtime::{
    Config, Engine, Linker, Module, ResourceLimiter, Store, StoreLimits, StoreLimitsBuilder,
};
use wasmtime_wasi::WasiCtxBuilder;
use wasmtime_wasi::preview1::{self, WasiP1Ctx};

use crate::{SandboxError, SandboxLimits, SandboxOutcome};

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
pub(crate) fn build_engine() -> Result<Engine, SandboxError> {
    let mut config = Config::new();
    config.consume_fuel(true);
    config.epoch_interruption(true);
    config.wasm_backtrace_details(wasmtime::WasmBacktraceDetails::Disable);
    Engine::new(&config).map_err(|err| SandboxError::Engine(err.to_string()))
}

/// Executes a core wasm module's `_start` export in an isolated store. The
/// epoch ticker is the only way to interrupt wasm that ignores or outlives its
/// fuel budget; it is cancelled as soon as evaluation finishes.
pub(crate) fn evaluate_module(
    engine: Engine,
    artifact: &[u8],
    limits: &SandboxLimits,
) -> Result<SandboxOutcome, SandboxError> {
    let started = Instant::now();
    let module =
        Module::new(&engine, artifact).map_err(|err| SandboxError::Compile(err.to_string()))?;
    let initial_fuel = limits.fuel;

    let (finished_tx, finished_rx) = mpsc::channel::<()>();
    {
        let ticker_engine = engine.clone();
        let timeout = limits.timeout.max(Duration::from_millis(1));
        std::thread::spawn(move || {
            if finished_rx.recv_timeout(timeout).is_err() {
                ticker_engine.increment_epoch();
            }
        });
    }

    let outcome = (|| -> Result<u64, SandboxError> {
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
        Ok(initial_fuel.saturating_sub(fuel_left))
    })();

    let _ = finished_tx.send(());
    outcome.map(|fuel_consumed| SandboxOutcome {
        passed: true,
        fuel_consumed,
        duration_ms: started.elapsed().as_millis() as u64,
        message: format!("sandboxed test artifact exited cleanly ({fuel_consumed} fuel units)"),
    })
}
