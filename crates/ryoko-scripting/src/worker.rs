//! Isolated subprocess worker for WASI plugin execution.
//!
//! Mirrors `ryoko-sandbox::worker`: each invocation spawns a fresh
//! `wasm-plugin-worker` process with a cleared environment, sends one bounded
//! length-prefixed JSON request over stdin, and reads one bounded response
//! from stdout. On Windows this isolates the host IDE from Wasmtime 26 guest
//! trap aborts (`0xc0000409`).

use std::io::{Read, Write};
use std::path::Path;
use std::process::Stdio;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::process::Command;

use crate::manifest::{PluginError, PluginLimits, evaluate_sync};

const MAX_FRAME_REQUEST_BYTES: usize = 16 * 1024 * 1024;
const MAX_FRAME_RESPONSE_BYTES: usize = 4 * 1024 * 1024;

#[derive(Serialize, Deserialize)]
struct PluginWorkerRequest {
    wasm_bytes: Vec<u8>,
    input: Vec<u8>,
    limits: PluginLimits,
}

#[derive(Serialize, Deserialize)]
struct PluginWorkerResponse {
    ok: bool,
    output: Vec<u8>,
    error: Option<String>,
}

/// Entry point for the `wasm-plugin-worker` binary.
pub fn run_worker() -> i32 {
    let response = match read_request_sync().and_then(|req| {
        evaluate_sync(&req.wasm_bytes, req.limits, req.input).map(|output| PluginWorkerResponse {
            ok: true,
            output,
            error: None,
        })
    }) {
        Ok(resp) => resp,
        Err(error) => PluginWorkerResponse {
            ok: false,
            output: Vec::new(),
            error: Some(error.to_string()),
        },
    };
    if write_response_sync(&response).is_err() {
        return 2;
    }
    0
}

fn read_request_sync() -> Result<PluginWorkerRequest, PluginError> {
    let mut stdin = std::io::stdin().lock();
    let mut len_buf = [0u8; 4];
    stdin.read_exact(&mut len_buf).map_err(|error| {
        PluginError::Execution(format!("worker failed reading header: {error}"))
    })?;
    let len = u32::from_le_bytes(len_buf) as usize;
    if len == 0 || len > MAX_FRAME_REQUEST_BYTES {
        return Err(PluginError::Execution(
            "worker request frame exceeds limit".to_owned(),
        ));
    }
    let mut body = vec![0u8; len];
    stdin
        .read_exact(&mut body)
        .map_err(|error| PluginError::Execution(format!("worker failed reading body: {error}")))?;
    serde_json::from_slice(&body)
        .map_err(|error| PluginError::Protocol(format!("invalid worker request: {error}")))
}

fn write_response_sync(response: &PluginWorkerResponse) -> std::io::Result<()> {
    let body = serde_json::to_vec(response)
        .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidData, error))?;
    if body.len() > MAX_FRAME_RESPONSE_BYTES {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "worker response exceeds limit",
        ));
    }
    let mut stdout = std::io::stdout().lock();
    stdout.write_all(&(body.len() as u32).to_le_bytes())?;
    stdout.write_all(&body)?;
    stdout.flush()
}

/// Runs one WASI plugin invocation inside a fresh isolated worker process.
pub(crate) async fn execute_in_worker(
    worker_path: &Path,
    wasm_bytes: &[u8],
    limits: PluginLimits,
    input: Vec<u8>,
) -> Result<Vec<u8>, PluginError> {
    if !worker_path.is_file() {
        return Err(PluginError::Execution(format!(
            "isolated WASI plugin worker not found at {}",
            worker_path.display()
        )));
    }
    let request = PluginWorkerRequest {
        wasm_bytes: wasm_bytes.to_vec(),
        input,
        limits,
    };
    let encoded = serde_json::to_vec(&request)
        .map_err(|error| PluginError::Protocol(format!("cannot encode worker request: {error}")))?;
    if encoded.len() > MAX_FRAME_REQUEST_BYTES {
        return Err(PluginError::Protocol(
            "encoded worker request exceeds limit".to_owned(),
        ));
    }

    let mut command = Command::new(worker_path);
    command
        .env_clear()
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true);
    let mut child = command.spawn().map_err(|error| {
        PluginError::Execution(format!(
            "cannot spawn WASI plugin worker {}: {error}",
            worker_path.display()
        ))
    })?;

    let mut stdin = child
        .stdin
        .take()
        .ok_or_else(|| PluginError::Execution("worker stdin unavailable".to_owned()))?;
    let mut stdout = child
        .stdout
        .take()
        .ok_or_else(|| PluginError::Execution("worker stdout unavailable".to_owned()))?;

    let deadline = limits.timeout().saturating_add(Duration::from_secs(2));
    let io_future = async move {
        stdin
            .write_all(&(encoded.len() as u32).to_le_bytes())
            .await
            .map_err(|error| PluginError::Execution(format!("worker write failed: {error}")))?;
        stdin
            .write_all(&encoded)
            .await
            .map_err(|error| PluginError::Execution(format!("worker write failed: {error}")))?;
        drop(stdin);

        let mut len_buf = [0u8; 4];
        stdout.read_exact(&mut len_buf).await.map_err(|error| {
            PluginError::Execution(format!("plugin worker exited without a response: {error}"))
        })?;
        let len = u32::from_le_bytes(len_buf) as usize;
        if len == 0 || len > MAX_FRAME_RESPONSE_BYTES {
            return Err(PluginError::Execution(
                "plugin worker response exceeds limit".to_owned(),
            ));
        }
        let mut body = vec![0u8; len];
        stdout.read_exact(&mut body).await.map_err(|error| {
            PluginError::Execution(format!("plugin worker truncated response: {error}"))
        })?;
        let status = child.wait().await.map_err(|error| {
            PluginError::Execution(format!("plugin worker wait failed: {error}"))
        })?;
        if !status.success() {
            return Err(PluginError::Execution(format!(
                "plugin worker exited abnormally ({status})"
            )));
        }
        let response: PluginWorkerResponse = serde_json::from_slice(&body)
            .map_err(|error| PluginError::Protocol(format!("invalid worker response: {error}")))?;
        if response.ok {
            Ok(response.output)
        } else {
            Err(PluginError::Execution(response.error.unwrap_or_else(
                || "plugin worker reported failure".to_owned(),
            )))
        }
    };

    tokio::time::timeout(deadline, io_future)
        .await
        .map_err(|_| PluginError::Timeout(limits.timeout_ms))?
}
