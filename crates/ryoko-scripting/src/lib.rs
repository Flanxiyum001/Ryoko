//! WASI-sandboxed extensions.
//!
//! Plugins are ordinary WASI command modules. The host gives each invocation a
//! fresh store with empty arguments, empty environment and no preopened
//! directories; stdin carries one bounded JSON request and stdout must contain
//! one bounded JSON response. WASI preview 1 has no socket API, so a plugin
//! cannot reach the network. Fuel, an epoch deadline, a linear-memory cap and an
//! output cap bound every invocation.
//!
//! The ABI is language-neutral: a Rust, Python or JavaScript plugin compiled to
//! a WASI command module uses the same JSON request/response contract. Plugin
//! output is untrusted data and is validated before it becomes an agent result,
//! lint diagnostic or context outline.
//!
//! [`WasiPlugin`] is loaded only from an explicitly supplied manifest and wasm
//! path, and only when the host-owned [`PluginPolicy`] permits its id and kind.
//! A plugin cannot grant itself a role or a capability.

mod manifest;
mod run;
mod types;

pub use manifest::{
    PluginError, PluginHost, PluginKind, PluginLimits, PluginManifest, PluginPolicy, WasiPlugin,
};
pub use run::PluginAgent;
pub use types::{
    LintDiagnostic, LintSeverity, PLUGIN_SCHEMA, PluginContext, PluginOutput, PluginRequest,
    PluginResponse, PluginResult, PluginTask,
};
