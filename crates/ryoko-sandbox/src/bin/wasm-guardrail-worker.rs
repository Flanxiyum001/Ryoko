//! `wasm-guardrail-worker`: one WASM guardrail evaluation per process.
//!
//! The binary reads a single `WorkerRequest` from stdin, evaluates the
//! artifact in a fresh wasmtime store with no filesystem, network or
//! environment preopens, and answers with a single `WorkerResponse` on
//! stdout. It is spawned by the host with a cleared environment and killed
//! on every exit path; a guest trap may abort this process, which the host
//! reports as a bounded failed validation.

fn main() {
    std::process::exit(ryoko_sandbox::worker::run_worker());
}
