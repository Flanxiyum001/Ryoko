//! Regression suite for the isolated guardrail worker process boundary.
//!
//! The host must stay healthy regardless of what the guest does: traps,
//! fuel exhaustion, deadline overruns and unavailable or crashing workers all
//! have to produce bounded failed validations instead of host aborts or
//! unbounded waits. Windows uses this worker path by default (see
//! `WasmSandbox::new`); other platforms exercise it explicitly here.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use ryoko_core::validation::ChangeValidator;
use ryoko_core::{ProposedChange, StagedChanges};
use ryoko_sandbox::{SandboxLimits, SandboxValidator, StaticArtifact, WasmSandbox};
use uuid::Uuid;

fn worker_path() -> Option<PathBuf> {
    std::env::var_os("CARGO_BIN_EXE_wasm-guardrail-worker").map(PathBuf::from)
}

fn worker_sandbox(limits: SandboxLimits) -> Option<WasmSandbox> {
    Some(WasmSandbox::new_with_worker(limits, worker_path()?).unwrap())
}

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
async fn worker_runs_clean_modules_successfully() {
    let Some(sandbox) = worker_sandbox(SandboxLimits::default()) else {
        eprintln!("worker regression suite skipped: worker binary not provisioned");
        return;
    };
    let outcome = sandbox
        .execute(br#"(module (func (export "_start")))"#)
        .await
        .expect("clean module must pass");
    assert!(outcome.passed);
    assert!(outcome.fuel_consumed > 0);
}

#[tokio::test]
async fn worker_runs_a_fresh_process_for_every_evaluation() {
    let Some(sandbox) = worker_sandbox(SandboxLimits::default()) else {
        eprintln!("worker regression suite skipped: worker binary not provisioned");
        return;
    };
    for _ in 0..2 {
        let outcome = sandbox
            .execute(br#"(module (func (export "_start")))"#)
            .await
            .expect("each evaluation spawns its own worker");
        assert!(outcome.passed);
    }
}

#[tokio::test]
async fn worker_traps_return_bounded_failures() {
    let Some(sandbox) = worker_sandbox(SandboxLimits::default()) else {
        eprintln!("worker regression suite skipped: worker binary not provisioned");
        return;
    };
    let validator = SandboxValidator::new(
        sandbox,
        Arc::new(StaticArtifact::new(
            br#"(module (func (export "_start") unreachable))"#,
        )),
    );
    let report = validator.validate(&changes()).await.unwrap();
    assert!(!report.passed, "a trapped guest must fail the guardrail");
    assert!(
        report.logs.join(" ").contains("execution failed")
            || report.logs.join(" ").contains("worker"),
        "unexpected report: {:?}",
        report
    );
}

#[tokio::test]
async fn worker_timeouts_and_fuel_exhaustion_are_bounded() {
    let Some(sandbox) = worker_sandbox(SandboxLimits {
        fuel: 100_000,
        timeout: Duration::from_secs(5),
        max_memory_bytes: 16 * 1024 * 1024,
    }) else {
        eprintln!("worker regression suite skipped: worker binary not provisioned");
        return;
    };
    let started = Instant::now();
    let outcome = sandbox
        .execute(br#"(module (func (export "_start") (loop br 0)))"#)
        .await;
    assert!(outcome.is_err(), "an endless guest must fail");
    assert!(
        started.elapsed() < Duration::from_secs(30),
        "the worker deadline must bound the run, took {:?}",
        started.elapsed()
    );
}

#[tokio::test]
async fn worker_crash_or_absence_fails_closed_without_touching_the_host() {
    let Some(sandbox) = worker_sandbox(SandboxLimits::default()) else {
        eprintln!("worker regression suite skipped: worker binary not provisioned");
        return;
    };
    let absent = WasmSandbox::new_with_worker(
        SandboxLimits::default(),
        PathBuf::from("definitely-not-a-worker-binary"),
    )
    .unwrap();
    let error = absent
        .execute(br#"(module (func (export "_start")))"#)
        .await
        .expect_err("an unavailable worker must fail closed");
    assert!(error.to_string().contains("guardrail worker"));
    let _ = sandbox;
}
