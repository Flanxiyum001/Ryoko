//! Manifest, policy, host and WASI invocation.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
#[cfg(not(windows))]
use tokio::task::JoinError;
use wasmtime::{
    Config, Engine, Linker, Module, ResourceLimiter, Store, StoreLimits, StoreLimitsBuilder,
};
use wasmtime_wasi::WasiCtxBuilder;
use wasmtime_wasi::pipe::{MemoryInputPipe, MemoryOutputPipe};
use wasmtime_wasi::preview1::{self, WasiP1Ctx};

use crate::types::{
    MAX_REQUEST_BYTES, PLUGIN_SCHEMA, PluginContext, PluginOutput, PluginRequest, PluginResponse,
};

const MAX_MANIFEST_BYTES: u64 = 64 * 1024;
const MAX_WASM_BYTES: u64 = 64 * 1024 * 1024;
const MAX_STDERR_BYTES: usize = 16 * 1024;

#[derive(Debug, thiserror::Error)]
pub enum PluginError {
    #[error("plugin file could not be read: {0}")]
    Io(String),
    #[error("invalid plugin manifest: {0}")]
    Manifest(String),
    #[error("plugin `{0}` is not permitted by the host policy")]
    PolicyDenied(String),
    #[error("plugin wasm could not be compiled: {0}")]
    Compile(String),
    #[error("plugin instantiation failed: {0}")]
    Instantiate(String),
    #[error("plugin execution failed: {0}")]
    Execution(String),
    #[error("plugin exceeded its {0} ms wall-clock limit")]
    Timeout(u64),
    #[error("plugin output exceeded the {0} byte limit")]
    OutputLimit(usize),
    #[error("invalid plugin protocol payload: {0}")]
    Protocol(String),
}

impl From<PluginError> for ryoko_core::CoreError {
    fn from(value: PluginError) -> Self {
        ryoko_core::CoreError::Sandbox(value.to_string())
    }
}

/// The capability class a plugin declares. The host policy must allow this
/// exact kind before the module can be loaded.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PluginKind {
    Agent,
    Linter,
    Context,
}

impl PluginKind {
    pub fn label(self) -> &'static str {
        match self {
            Self::Agent => "agent",
            Self::Linter => "linter",
            Self::Context => "context",
        }
    }

    fn matches_request(self, request: &PluginRequest) -> bool {
        matches!(
            (self, &request.operation),
            (Self::Agent, crate::types::PluginOperation::Agent { .. })
                | (Self::Linter, crate::types::PluginOperation::Lint { .. })
                | (Self::Context, crate::types::PluginOperation::Context { .. })
        )
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct PluginLimits {
    /// Fuel units available to the module.
    pub fuel: u64,
    /// Wall-clock limit, enforced through wasmtime epoch interruption.
    pub timeout_ms: u64,
    pub max_memory_bytes: usize,
    /// Maximum bytes accepted from plugin stdout. Stderr is separately capped
    /// at 16 KiB for diagnostics and is never interpreted as a result.
    pub max_output_bytes: usize,
}

impl Default for PluginLimits {
    fn default() -> Self {
        Self {
            fuel: 100_000_000,
            timeout_ms: 10_000,
            max_memory_bytes: 128 * 1024 * 1024,
            max_output_bytes: 2 * 1024 * 1024,
        }
    }
}

impl PluginLimits {
    fn capped_by(self, max: Self) -> Self {
        Self {
            fuel: self.fuel.min(max.fuel).max(1),
            timeout_ms: self.timeout_ms.min(max.timeout_ms).max(1),
            max_memory_bytes: self.max_memory_bytes.min(max.max_memory_bytes).max(1),
            max_output_bytes: self.max_output_bytes.min(max.max_output_bytes).max(1),
        }
    }

    pub(crate) fn timeout(self) -> Duration {
        Duration::from_millis(self.timeout_ms.max(1))
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PluginManifest {
    pub schema: u32,
    pub id: String,
    pub name: String,
    pub version: String,
    pub kind: PluginKind,
    #[serde(default)]
    pub description: String,
    /// SHA-256 of the `.wasm` module. Required for registry installs; the host
    /// also verifies it when present.
    #[serde(default)]
    pub wasm_sha256: Option<String>,
}

impl PluginManifest {
    pub fn load(path: &Path) -> Result<Self, PluginError> {
        let bytes = read_regular_file(path, MAX_MANIFEST_BYTES)?;
        let manifest: Self = serde_json::from_slice(&bytes)
            .map_err(|error| PluginError::Manifest(error.to_string()))?;
        manifest.validate()?;
        Ok(manifest)
    }

    pub fn validate(&self) -> Result<(), PluginError> {
        if self.schema != PLUGIN_SCHEMA {
            return Err(PluginError::Manifest(format!(
                "unsupported manifest schema {}",
                self.schema
            )));
        }
        if self.id.is_empty()
            || self.id.len() > 64
            || !self
                .id
                .chars()
                .all(|ch| ch.is_ascii_lowercase() || ch.is_ascii_digit() || "._-".contains(ch))
        {
            return Err(PluginError::Manifest(
                "id must be 1-64 lowercase ASCII letters, digits, '.', '_' or '-'".into(),
            ));
        }
        if self.name.trim().is_empty()
            || self.name.len() > 80
            || self.name.chars().any(char::is_control)
        {
            return Err(PluginError::Manifest(
                "name must be 1-80 printable characters".into(),
            ));
        }
        if self.version.trim().is_empty()
            || self.version.len() > 32
            || self.version.chars().any(char::is_control)
        {
            return Err(PluginError::Manifest(
                "version must be 1-32 printable characters".into(),
            ));
        }
        if self.description.len() > 1024 || self.description.chars().any(char::is_control) {
            return Err(PluginError::Manifest(
                "description must be at most 1024 printable characters".into(),
            ));
        }
        if let Some(hash) = &self.wasm_sha256
            && (hash.len() != 64
                || !hash.chars().all(|ch| ch.is_ascii_hexdigit())
                || hash.chars().any(|ch| ch.is_ascii_uppercase()))
        {
            return Err(PluginError::Manifest(
                "wasm_sha256 must be 64 lowercase hexadecimal characters".into(),
            ));
        }
        Ok(())
    }
}

/// Host-owned allowlist. Empty allowlists deny every plugin; there is no
/// implicit "allow all" mode.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PluginPolicy {
    pub allowed_ids: BTreeSet<String>,
    pub allowed_kinds: BTreeSet<PluginKind>,
    pub limits: PluginLimits,
}

impl PluginPolicy {
    pub fn new(
        allowed_ids: impl IntoIterator<Item = impl Into<String>>,
        allowed_kinds: impl IntoIterator<Item = PluginKind>,
        limits: PluginLimits,
    ) -> Self {
        Self {
            allowed_ids: allowed_ids.into_iter().map(Into::into).collect(),
            allowed_kinds: allowed_kinds.into_iter().collect(),
            limits,
        }
    }

    pub fn from_json(input: &str) -> Result<Self, PluginError> {
        if input.len() > MAX_MANIFEST_BYTES as usize {
            return Err(PluginError::PolicyDenied("policy file is too large".into()));
        }
        let policy: Self = serde_json::from_str(input)
            .map_err(|error| PluginError::Manifest(format!("invalid policy: {error}")))?;
        if policy.allowed_ids.len() > 128 || policy.allowed_kinds.len() > 8 {
            return Err(PluginError::PolicyDenied(
                "policy allowlist is too large".into(),
            ));
        }
        Ok(policy)
    }

    pub fn permits(&self, manifest: &PluginManifest) -> bool {
        self.allowed_ids.contains(&manifest.id) && self.allowed_kinds.contains(&manifest.kind)
    }
}

enum HostBackend {
    #[cfg(not(windows))]
    InProcess(Engine),
    Worker(PathBuf),
}

#[derive(Clone)]
enum PluginBackend {
    #[cfg(not(windows))]
    InProcess { engine: Engine, module: Module },
    Worker {
        worker_path: PathBuf,
        wasm_bytes: Arc<[u8]>,
    },
}

/// Compiles and executes plugins either in-process (Unix) or inside an isolated
/// `wasm-plugin-worker` subprocess (Windows, or explicitly via
/// [`PluginHost::new_with_worker`]). Fuel and epoch interruption are always
/// enabled and no host functions beyond WASI preview 1 are linked.
pub struct PluginHost {
    backend: HostBackend,
    max_limits: PluginLimits,
}

impl std::fmt::Debug for PluginHost {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PluginHost")
            .field("max_limits", &self.max_limits)
            .finish_non_exhaustive()
    }
}

fn build_engine() -> Result<Engine, PluginError> {
    let mut config = Config::new();
    config.consume_fuel(true);
    config.epoch_interruption(true);
    config.wasm_backtrace_details(wasmtime::WasmBacktraceDetails::Disable);
    Engine::new(&config).map_err(|error| PluginError::Compile(error.to_string()))
}

#[cfg(windows)]
fn resolve_windows_worker() -> Option<PathBuf> {
    if let Some(explicit) = std::env::var_os("RYOKO_WASM_PLUGIN_WORKER_BIN") {
        let candidate = PathBuf::from(explicit);
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    let current = std::env::current_exe().ok()?;
    let sibling = current.parent()?.join("wasm-plugin-worker.exe");
    sibling.is_file().then_some(sibling)
}

impl PluginHost {
    /// On Windows, Wasmtime 26 guest traps can abort the host process
    /// (`0xc0000409`). Delegate execution to the dedicated
    /// `wasm-plugin-worker.exe` subprocess next to the app binary (or via
    /// `RYOKO_WASM_PLUGIN_WORKER_BIN`), failing closed if the worker binary
    /// is missing.
    #[cfg(windows)]
    pub fn new(max_limits: PluginLimits) -> Result<Self, PluginError> {
        let worker_path = resolve_windows_worker().ok_or_else(|| {
            PluginError::Compile(
                "native Windows WASI plugin execution unavailable: wasm-plugin-worker.exe not found next to host binary"
                    .into(),
            )
        })?;
        Ok(Self {
            backend: HostBackend::Worker(worker_path),
            max_limits,
        })
    }

    #[cfg(not(windows))]
    pub fn new(max_limits: PluginLimits) -> Result<Self, PluginError> {
        let engine = build_engine()?;
        Ok(Self {
            backend: HostBackend::InProcess(engine),
            max_limits,
        })
    }

    /// Creates a plugin host that delegates every invocation to an explicit
    /// `wasm-plugin-worker` binary. Available on all platforms for isolated
    /// execution and cross-platform testing.
    pub fn new_with_worker(
        max_limits: PluginLimits,
        worker_path: impl Into<PathBuf>,
    ) -> Result<Self, PluginError> {
        let worker_path = worker_path.into();
        if !worker_path.is_file() {
            return Err(PluginError::Compile(format!(
                "isolated WASI plugin worker not found at {}",
                worker_path.display()
            )));
        }
        Ok(Self {
            backend: HostBackend::Worker(worker_path),
            max_limits,
        })
    }

    /// Loads one explicitly chosen manifest/module pair. The policy is checked
    /// before compilation, so a denied module is never even validated.
    pub async fn load(
        &self,
        manifest_path: &Path,
        wasm_path: &Path,
        policy: &PluginPolicy,
    ) -> Result<WasiPlugin, PluginError> {
        let manifest = PluginManifest::load(manifest_path)?;
        if !policy.permits(&manifest) {
            return Err(PluginError::PolicyDenied(manifest.id));
        }
        let limits = policy.limits.capped_by(self.max_limits);
        let bytes = read_regular_file(wasm_path, MAX_WASM_BYTES)?;
        if let Some(expected) = &manifest.wasm_sha256 {
            let actual = sha256_hex(&bytes);
            if !actual.eq_ignore_ascii_case(expected) {
                return Err(PluginError::Manifest(format!(
                    "wasm hash mismatch for `{}`",
                    manifest.id
                )));
            }
        }
        let backend = match &self.backend {
            #[cfg(not(windows))]
            HostBackend::InProcess(engine) => {
                let engine_clone = engine.clone();
                let module =
                    tokio::task::spawn_blocking(move || Module::new(&engine_clone, &bytes))
                        .await
                        .map_err(join_error)?
                        .map_err(|error| PluginError::Compile(error.to_string()))?;
                PluginBackend::InProcess {
                    engine: engine.clone(),
                    module,
                }
            }
            HostBackend::Worker(worker_path) => PluginBackend::Worker {
                worker_path: worker_path.clone(),
                wasm_bytes: Arc::from(bytes),
            },
        };
        Ok(WasiPlugin {
            manifest,
            backend,
            limits,
        })
    }
}

fn read_regular_file(path: &Path, max_bytes: u64) -> Result<Vec<u8>, PluginError> {
    let metadata = std::fs::symlink_metadata(path)
        .map_err(|error| PluginError::Io(format!("{}: {error}", path.display())))?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err(PluginError::Io(format!(
            "{} is not a regular file",
            path.display()
        )));
    }
    if metadata.len() > max_bytes {
        return Err(PluginError::Io(format!(
            "{} exceeds the {} byte limit",
            path.display(),
            max_bytes
        )));
    }
    std::fs::read(path).map_err(|error| PluginError::Io(format!("{}: {error}", path.display())))
}

/// Lowercase hex SHA-256 used by registry installs and host verification.
pub fn sha256_hex(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    let digest = Sha256::digest(bytes);
    let mut output = String::with_capacity(64);
    for byte in digest {
        use std::fmt::Write as _;
        let _ = write!(output, "{byte:02x}");
    }
    output
}

#[cfg(not(windows))]
fn join_error(error: JoinError) -> PluginError {
    PluginError::Execution(format!("plugin task failed: {error}"))
}

/// A loaded, policy-approved plugin module.
#[derive(Clone)]
pub struct WasiPlugin {
    manifest: PluginManifest,
    backend: PluginBackend,
    limits: PluginLimits,
}

impl std::fmt::Debug for WasiPlugin {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WasiPlugin")
            .field("manifest", &self.manifest)
            .field("limits", &self.limits)
            .finish_non_exhaustive()
    }
}

impl WasiPlugin {
    pub fn manifest(&self) -> &PluginManifest {
        &self.manifest
    }

    pub fn limits(&self) -> PluginLimits {
        self.limits
    }

    /// Runs one request. The module's declared kind must match the operation;
    /// a linter cannot be used as an agent even if both are loaded.
    pub async fn invoke(&self, request: &PluginRequest) -> Result<PluginResponse, PluginError> {
        request.validate()?;
        if !self.manifest.kind.matches_request(request) {
            return Err(PluginError::Protocol(format!(
                "plugin `{}` is a {} but received a different operation",
                self.manifest.id,
                self.manifest.kind.label()
            )));
        }
        let input = serde_json::to_vec(request)
            .map_err(|error| PluginError::Protocol(error.to_string()))?;
        if input.len() > MAX_REQUEST_BYTES {
            return Err(PluginError::Protocol(
                "plugin request exceeds 256 KiB".into(),
            ));
        }
        let output = match &self.backend {
            #[cfg(not(windows))]
            PluginBackend::InProcess { engine, module } => {
                execute(engine.clone(), module.clone(), self.limits, input).await?
            }
            PluginBackend::Worker {
                worker_path,
                wasm_bytes,
            } => {
                crate::worker::execute_in_worker(worker_path, wasm_bytes, self.limits, input)
                    .await?
            }
        };
        let response: PluginResponse = serde_json::from_slice(&output)
            .map_err(|error| PluginError::Protocol(format!("response is not JSON: {error}")))?;
        response.validate()?;
        Ok(response)
    }

    pub async fn lint(
        &self,
        path: &str,
        source: &str,
    ) -> Result<Vec<crate::types::LintDiagnostic>, PluginError> {
        let response = self.invoke(&PluginRequest::lint(path, source)).await?;
        match response.result {
            PluginOutput::Lint { diagnostics } => Ok(diagnostics),
            _ => Err(PluginError::Protocol(
                "linter plugin returned a non-lint result".into(),
            )),
        }
    }

    pub async fn context(&self, path: &str, source: &str) -> Result<PluginContext, PluginError> {
        let response = self.invoke(&PluginRequest::context(path, source)).await?;
        match response.result {
            PluginOutput::Context { outline, symbols } => Ok(PluginContext { outline, symbols }),
            _ => Err(PluginError::Protocol(
                "context plugin returned a non-context result".into(),
            )),
        }
    }
}

struct PluginState {
    wasi: WasiP1Ctx,
    limits: StoreLimits,
}

impl ResourceLimiter for PluginState {
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

fn run_module_sync(
    engine: &Engine,
    module: &Module,
    limits: PluginLimits,
    input: Vec<u8>,
    started: Instant,
) -> Result<Vec<u8>, PluginError> {
    let timeout = limits.timeout();
    let stdin = MemoryInputPipe::new(input);
    // One extra byte distinguishes "exactly at the limit" from truncation.
    let stdout = MemoryOutputPipe::new(limits.max_output_bytes.saturating_add(1));
    let stderr = MemoryOutputPipe::new(MAX_STDERR_BYTES);
    let mut builder = WasiCtxBuilder::new();
    builder
        .stdin(stdin)
        .stdout(stdout.clone())
        .stderr(stderr.clone());
    let store_limits = StoreLimitsBuilder::new()
        .memory_size(limits.max_memory_bytes)
        .instances(8)
        .tables(8)
        .build();
    let mut store = Store::new(
        engine,
        PluginState {
            wasi: builder.build_p1(),
            limits: store_limits,
        },
    );
    store.limiter(|state| state as &mut dyn ResourceLimiter);
    store
        .set_fuel(limits.fuel)
        .map_err(|error| PluginError::Instantiate(error.to_string()))?;
    store.set_epoch_deadline(1);
    let mut linker = Linker::new(engine);
    preview1::add_to_linker_sync(&mut linker, |state: &mut PluginState| &mut state.wasi)
        .map_err(|error| PluginError::Instantiate(error.to_string()))?;
    let instance = linker
        .instantiate(&mut store, module)
        .map_err(|error| PluginError::Instantiate(error.to_string()))?;
    let start = instance
        .get_typed_func::<(), ()>(&mut store, "_start")
        .map_err(|error| {
            PluginError::Instantiate(format!("plugin must export `_start`: {error}"))
        })?;
    let result = start.call(&mut store, ());
    let bytes = stdout.contents();
    let logs = stderr.contents();
    if bytes.len() > limits.max_output_bytes {
        return Err(PluginError::OutputLimit(limits.max_output_bytes));
    }
    if let Err(error) = result {
        let detail = String::from_utf8_lossy(&logs);
        let detail = detail.trim();
        let message = if detail.is_empty() {
            error.to_string()
        } else {
            format!("{error}: {}", detail.chars().take(1024).collect::<String>())
        };
        return Err(if started.elapsed() >= timeout {
            PluginError::Timeout(limits.timeout_ms)
        } else {
            PluginError::Execution(message)
        });
    }
    Ok(bytes.to_vec())
}

/// Synchronous entry point executed inside the isolated `wasm-plugin-worker`
/// subprocess.
pub(crate) fn evaluate_sync(
    wasm_bytes: &[u8],
    limits: PluginLimits,
    input: Vec<u8>,
) -> Result<Vec<u8>, PluginError> {
    let engine = build_engine()?;
    let module = Module::new(&engine, wasm_bytes)
        .map_err(|error| PluginError::Compile(error.to_string()))?;
    let started = Instant::now();
    let timeout = limits.timeout();
    let done = Arc::new(AtomicBool::new(false));
    let done_flag = Arc::clone(&done);
    let engine_clone = engine.clone();
    let ticker = std::thread::spawn(move || {
        std::thread::park_timeout(timeout);
        if !done_flag.load(Ordering::Acquire) {
            engine_clone.increment_epoch();
        }
    });
    let result = run_module_sync(&engine, &module, limits, input, started);
    done.store(true, Ordering::Release);
    ticker.thread().unpark();
    let _ = ticker.join();
    result
}

#[cfg(not(windows))]
async fn execute(
    engine: Engine,
    module: Module,
    limits: PluginLimits,
    input: Vec<u8>,
) -> Result<Vec<u8>, PluginError> {
    let started = Instant::now();
    let timeout = limits.timeout();
    let ticker = {
        let engine = engine.clone();
        tokio::spawn(async move {
            tokio::time::sleep(timeout).await;
            engine.increment_epoch();
        })
    };

    let execution = tokio::task::spawn_blocking(move || {
        run_module_sync(&engine, &module, limits, input, started)
    })
    .await;

    ticker.abort();
    match execution {
        Ok(result) => result,
        Err(error) => Err(join_error(error)),
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use std::path::PathBuf;
    use tempfile::TempDir;

    pub(crate) fn wasi_module(output: &str) -> Vec<u8> {
        format!(
            r#"(module
                (import "wasi_snapshot_preview1" "fd_write"
                    (func $fd_write (param i32 i32 i32 i32) (result i32)))
                (memory (export "memory") 1)
                (data (i32.const 32) {output:?})
                (func (export "_start")
                    (i32.store (i32.const 0) (i32.const 32))
                    (i32.store (i32.const 4) (i32.const {length}))
                    (drop (call $fd_write
                        (i32.const 1) (i32.const 0) (i32.const 1) (i32.const 8))
                    )
                )
            )"#,
            length = output.len()
        )
        .into_bytes()
    }

    pub(crate) struct Fixture {
        pub dir: TempDir,
        pub manifest: PathBuf,
        pub wasm: PathBuf,
        pub policy: PluginPolicy,
    }

    pub(crate) fn fixture(kind: PluginKind, output: &str, limits: PluginLimits) -> Fixture {
        let dir = tempfile::tempdir().unwrap();
        let manifest = dir.path().join("plugin.json");
        let wasm = dir.path().join("plugin.wasm");
        std::fs::write(
            &manifest,
            serde_json::to_vec(&PluginManifest {
                schema: PLUGIN_SCHEMA,
                id: "test-plugin".into(),
                name: "Test plugin".into(),
                version: "1.0.0".into(),
                kind,
                description: "test fixture".into(),
                wasm_sha256: None,
            })
            .unwrap(),
        )
        .unwrap();
        std::fs::write(&wasm, wasi_module(output)).unwrap();
        let policy = PluginPolicy::new(["test-plugin"], [kind], limits);
        Fixture {
            dir,
            manifest,
            wasm,
            policy,
        }
    }

    #[cfg(windows)]
    #[test]
    fn native_windows_wasi_plugin_execution_fails_closed() {
        assert!(matches!(
            PluginHost::new(PluginLimits::default()),
            Err(PluginError::Compile(_))
        ));
    }

    #[cfg(not(windows))]
    #[tokio::test]
    async fn runs_a_context_plugin_over_wasi_stdio() {
        let output = r#"{"schema":1,"result":{"type":"context","outline":"fn main() {}","symbols":["main"]}}"#;
        let fixture = fixture(PluginKind::Context, output, PluginLimits::default());
        let host = PluginHost::new(PluginLimits::default()).unwrap();
        let plugin = host
            .load(&fixture.manifest, &fixture.wasm, &fixture.policy)
            .await
            .unwrap();
        let context = plugin.context("src/main.rs", "fn main() {}").await.unwrap();
        assert_eq!(context.symbols, vec!["main"]);
    }

    #[cfg(not(windows))]
    #[tokio::test]
    async fn runs_a_linter_plugin() {
        let output = r#"{"schema":1,"result":{"type":"lint","diagnostics":[{"line":2,"column":1,"severity":"warning","message":"avoid this"}]}}"#;
        let fixture = fixture(PluginKind::Linter, output, PluginLimits::default());
        let host = PluginHost::new(PluginLimits::default()).unwrap();
        let plugin = host
            .load(&fixture.manifest, &fixture.wasm, &fixture.policy)
            .await
            .unwrap();
        let diagnostics = plugin.lint("src/main.rs", "fn main() {}").await.unwrap();
        assert_eq!(diagnostics.len(), 1);
        assert_eq!(diagnostics[0].line, 2);
        assert_eq!(diagnostics[0].severity, crate::types::LintSeverity::Warning);
    }

    #[cfg(not(windows))]
    #[tokio::test]
    async fn policy_denies_unknown_ids_before_execution() {
        let output = r#"{"schema":1,"result":{"type":"context","outline":"","symbols":[]}}"#;
        let fixture = fixture(PluginKind::Context, output, PluginLimits::default());
        let host = PluginHost::new(PluginLimits::default()).unwrap();
        let mut policy = fixture.policy.clone();
        policy.allowed_ids.clear();
        assert!(matches!(
            host.load(&fixture.manifest, &fixture.wasm, &policy).await,
            Err(PluginError::PolicyDenied(_))
        ));
    }

    #[cfg(not(windows))]
    #[tokio::test]
    async fn plugin_kind_cannot_be_used_for_another_operation() {
        let output = r#"{"schema":1,"result":{"type":"context","outline":"","symbols":[]}}"#;
        let fixture = fixture(PluginKind::Context, output, PluginLimits::default());
        let host = PluginHost::new(PluginLimits::default()).unwrap();
        let plugin = host
            .load(&fixture.manifest, &fixture.wasm, &fixture.policy)
            .await
            .unwrap();
        assert!(matches!(
            plugin.lint("src/main.rs", "fn main() {}").await,
            Err(PluginError::Protocol(_))
        ));
    }

    #[cfg(not(windows))]
    #[tokio::test]
    async fn malformed_or_truncated_output_is_rejected() {
        let fixture = fixture(PluginKind::Context, "not json", PluginLimits::default());
        let host = PluginHost::new(PluginLimits::default()).unwrap();
        let plugin = host
            .load(&fixture.manifest, &fixture.wasm, &fixture.policy)
            .await
            .unwrap();
        assert!(matches!(
            plugin.context("src/main.rs", "fn main() {}").await,
            Err(PluginError::Protocol(_))
        ));
    }

    #[cfg(not(windows))]
    #[tokio::test]
    async fn output_cap_is_enforced() {
        let output = format!(
            r#"{{"schema":1,"result":{{"type":"context","outline":"{}","symbols":[]}}}}"#,
            "x".repeat(2048)
        );
        let fixture = fixture(
            PluginKind::Context,
            &output,
            PluginLimits {
                max_output_bytes: 128,
                ..PluginLimits::default()
            },
        );
        let host = PluginHost::new(PluginLimits::default()).unwrap();
        let plugin = host
            .load(&fixture.manifest, &fixture.wasm, &fixture.policy)
            .await
            .unwrap();
        assert!(matches!(
            plugin.context("src/main.rs", "fn main() {}").await,
            Err(PluginError::OutputLimit(_)) | Err(PluginError::Execution(_))
        ));
    }

    #[cfg(not(windows))]
    #[tokio::test]
    async fn fuel_and_memory_limits_stop_hostile_modules() {
        let dir = tempfile::tempdir().unwrap();
        let manifest = dir.path().join("plugin.json");
        let wasm = dir.path().join("plugin.wasm");
        std::fs::write(
            &manifest,
            serde_json::to_vec(&PluginManifest {
                schema: PLUGIN_SCHEMA,
                id: "test-plugin".into(),
                name: "Test plugin".into(),
                version: "1.0.0".into(),
                kind: PluginKind::Context,
                description: String::new(),
                wasm_sha256: None,
            })
            .unwrap(),
        )
        .unwrap();
        std::fs::write(
            &wasm,
            br#"(module (memory 10) (func (export "_start") (loop br 0)))"#,
        )
        .unwrap();
        let policy = PluginPolicy::new(
            ["test-plugin"],
            [PluginKind::Context],
            PluginLimits {
                fuel: 100_000,
                timeout_ms: 5_000,
                max_memory_bytes: 64 * 1024,
                max_output_bytes: 1024,
            },
        );
        let host = PluginHost::new(PluginLimits::default()).unwrap();
        let memory_plugin = host.load(&manifest, &wasm, &policy).await.unwrap();
        assert!(memory_plugin.context("src/main.rs", "").await.is_err());
        let wasm = dir.path().join("loop.wasm");
        std::fs::write(&wasm, br#"(module (func (export "_start") (loop br 0)))"#).unwrap();
        let plugin = host.load(&manifest, &wasm, &policy).await.unwrap();
        assert!(matches!(
            plugin.context("src/main.rs", "").await,
            Err(PluginError::Execution(_)) | Err(PluginError::Timeout(_))
        ));
    }

    #[test]
    fn manifest_and_policy_validation_reject_unsafe_values() {
        let mut manifest = PluginManifest {
            schema: PLUGIN_SCHEMA,
            id: "bad id".into(),
            name: "x".into(),
            version: "1".into(),
            kind: PluginKind::Agent,
            description: String::new(),
            wasm_sha256: None,
        };
        assert!(manifest.validate().is_err());
        manifest.id = "ok".into();
        manifest.schema = 2;
        assert!(manifest.validate().is_err());
        assert!(PluginPolicy::from_json("{\"allowed_ids\":[]}").is_err());
    }
}
