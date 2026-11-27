//! `wasm-plugin-worker`: one WASI plugin invocation per isolated process.

fn main() {
    std::process::exit(ryoko_scripting::worker::run_worker());
}
