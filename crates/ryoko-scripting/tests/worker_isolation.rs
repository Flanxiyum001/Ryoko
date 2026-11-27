use ryoko_scripting::{
    LintSeverity, PLUGIN_SCHEMA, PluginError, PluginHost, PluginKind, PluginLimits, PluginManifest,
    PluginPolicy,
};

fn wasi_output_module(output: &str) -> Vec<u8> {
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

fn write_plugin_fixture(
    dir: &tempfile::TempDir,
    id: &str,
    kind: PluginKind,
    wasm_bytes: &[u8],
) -> (std::path::PathBuf, std::path::PathBuf, PluginPolicy) {
    let manifest_path = dir.path().join("plugin.json");
    let wasm_path = dir.path().join("plugin.wasm");
    std::fs::write(
        &manifest_path,
        serde_json::to_vec(&PluginManifest {
            schema: PLUGIN_SCHEMA,
            id: id.into(),
            name: "Worker Test Plugin".into(),
            version: "1.0.0".into(),
            kind,
            description: "worker isolation test fixture".into(),
            wasm_sha256: None,
        })
        .unwrap(),
    )
    .unwrap();
    std::fs::write(&wasm_path, wasm_bytes).unwrap();
    let policy = PluginPolicy::new([id], [kind], PluginLimits::default());
    (manifest_path, wasm_path, policy)
}

#[tokio::test]
async fn worker_executes_valid_linter_plugin() {
    let dir = tempfile::tempdir().unwrap();
    let output = r#"{"schema":1,"result":{"type":"lint","diagnostics":[{"line":4,"column":2,"severity":"warning","message":"isolated worker lint"}]}}"#;
    let (manifest, wasm, policy) = write_plugin_fixture(
        &dir,
        "worker-lint",
        PluginKind::Linter,
        &wasi_output_module(output),
    );
    let worker_bin = env!("CARGO_BIN_EXE_wasm-plugin-worker");
    let host = PluginHost::new_with_worker(PluginLimits::default(), worker_bin).unwrap();
    let plugin = host.load(&manifest, &wasm, &policy).await.unwrap();
    let diagnostics = plugin.lint("src/lib.rs", "fn main() {}").await.unwrap();
    assert_eq!(diagnostics.len(), 1);
    assert_eq!(diagnostics[0].line, 4);
    assert_eq!(diagnostics[0].severity, LintSeverity::Warning);
    assert_eq!(diagnostics[0].message, "isolated worker lint");
}

#[tokio::test]
async fn worker_isolates_guest_trap_without_aborting_host() {
    let dir = tempfile::tempdir().unwrap();
    let trap_wasm = br#"(module (func (export "_start") (unreachable)))"#;
    let (manifest, wasm, policy) =
        write_plugin_fixture(&dir, "worker-trap", PluginKind::Linter, trap_wasm);
    let worker_bin = env!("CARGO_BIN_EXE_wasm-plugin-worker");
    let host = PluginHost::new_with_worker(PluginLimits::default(), worker_bin).unwrap();
    let plugin = host.load(&manifest, &wasm, &policy).await.unwrap();
    let error = plugin
        .lint("src/lib.rs", "fn main() {}")
        .await
        .expect_err("trapping guest must fail with PluginError::Execution");
    assert!(
        matches!(error, PluginError::Execution(_)),
        "expected Execution error, got {error:?}"
    );
}

#[test]
fn worker_host_fails_closed_when_binary_missing() {
    let dir = tempfile::tempdir().unwrap();
    let missing = dir.path().join("does-not-exist-wasm-plugin-worker");
    let error = PluginHost::new_with_worker(PluginLimits::default(), missing)
        .expect_err("missing worker binary must fail closed");
    assert!(matches!(error, PluginError::Compile(_)));
}
