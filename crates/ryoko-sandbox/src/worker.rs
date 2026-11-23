//! Isolated guardrail worker process boundary.
//!
//! The worker is a separate binary (`wasm-guardrail-worker`) that reads one
//! typed [`WorkerRequest`] from stdin, evaluates the artifact with empty WASI
//! state and answers with one typed [`WorkerResponse`] on stdout. The host
//! clears the worker environment, pipes only the three standard streams and
//! bounds the wall clock. Guest traps, deadline overruns and worker crashes
//! are all converted into bounded failed validations: a trap may abort the
//! worker (Wasmtime 26 aborts on Windows), but then the host merely observes
//! EOF without a response instead of dying with it.
//!
//! A fresh worker is spawned for every evaluation; no worker outlives its own
//! request and stale processes can never be reused implicitly.

use std::io::{Read, Write};
use std::path::Path;
use std::process::Stdio;
use std::time::{Duration, Instant};

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::process::Command;

use crate::evaluate::{build_engine, evaluate_module};
use crate::protocol::{WorkerRequest, WorkerResponse};
use crate::{SandboxError, SandboxLimits, SandboxOutcome};

const WORKER_PROTOCOL_GRACE: Duration = Duration::from_millis(500);
const MAX_REPORTED_STDERR_CHARS: usize = 512;

/// Entry point of the `wasm-guardrail-worker` binary. Reads one request from
/// stdin, evaluates it and writes one response. Exit code 0 means a typed
/// response was delivered (including a Failed response), 2 means malformed
/// input or an unwritable pipe; the host treats every non-response as a
/// failed validation.
pub fn run_worker() -> i32 {
    let mut input = Vec::new();
    if std::io::stdin().read_to_end(&mut input).is_err() {
        return 2;
    }
    let response = match serde_json::from_slice::<WorkerRequest>(&input) {
        Ok(request) => handle_request(request),
        Err(_) => return 2,
    };
    let Ok(mut payload) = serde_json::to_vec(&response) else {
        return 2;
    };
    payload.push(b'\n');
    let stdout = std::io::stdout();
    let mut locked = stdout.lock();
    if locked
        .write_all(&payload)
        .and_then(|()| locked.flush())
        .is_err()
    {
        return 2;
    }
    0
}

fn handle_request(request: WorkerRequest) -> WorkerResponse {
    let evaluation = request.artifact().and_then(|artifact| {
        let limits = request.limits();
        build_engine().and_then(|engine| evaluate_module(engine, &artifact, &limits))
    });
    match evaluation {
        Ok(outcome) => WorkerResponse::Passed {
            fuel_consumed: outcome.fuel_consumed,
            duration_ms: outcome.duration_ms,
            message: outcome.message,
        },
        Err(err) => WorkerResponse::Failed {
            error: err.to_string(),
        },
    }
}

/// Host side: runs one evaluation in a freshly spawned worker process.
pub(crate) async fn execute_in_worker(
    worker_path: &Path,
    limits: &SandboxLimits,
    artifact: &[u8],
) -> Result<SandboxOutcome, SandboxError> {
    let started = Instant::now();
    let request = WorkerRequest::new(artifact, limits);
    let payload = serde_json::to_vec(&request)
        .map_err(|err| SandboxError::Worker(format!("could not encode worker request: {err}")))?;

    let mut child = Command::new(worker_path)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .env_clear()
        .kill_on_drop(true)
        .spawn()
        .map_err(|err| SandboxError::Worker(format!("guardrail worker unavailable: {err}")))?;

    let mut stdin = child.stdin.take().expect("worker stdin is piped");
    stdin
        .write_all(&payload)
        .await
        .map_err(|err| SandboxError::Worker(format!("guardrail worker pipe failed: {err}")))?;
    drop(stdin);

    let mut stdout = child.stdout.take().expect("worker stdout is piped");
    let mut stderr = child.stderr.take().expect("worker stderr is piped");
    let deadline = limits.timeout.saturating_add(WORKER_PROTOCOL_GRACE);
    let read = tokio::time::timeout(deadline, async move {
        let mut response = Vec::new();
        stdout
            .read_to_end(&mut response)
            .await
            .map_err(|err| SandboxError::Worker(format!("guardrail worker pipe failed: {err}")))?;
        let mut diagnostics = Vec::new();
        let _ = stderr.read_to_end(&mut diagnostics).await;
        Ok::<_, SandboxError>((response, diagnostics))
    })
    .await;

    let outcome = match read {
        Ok(Ok((response, diagnostics))) => parse_response(&response, &diagnostics),
        Ok(Err(err)) => Err(err),
        Err(_) => Err(SandboxError::Execution(format!(
            "guardrail worker exceeded its {} ms isolation deadline and was terminated",
            deadline.as_millis()
        ))),
    };

    let _ = child.kill().await;
    let _ = child.wait().await;
    outcome.map(|outcome| SandboxOutcome {
        duration_ms: started.elapsed().as_millis() as u64,
        ..outcome
    })
}

fn parse_response(bytes: &[u8], diagnostics: &[u8]) -> Result<SandboxOutcome, SandboxError> {
    if bytes.is_empty() {
        return Err(SandboxError::Worker(format!(
            "guardrail worker exited without a response{}",
            describe_diagnostics(diagnostics)
        )));
    }
    let response: WorkerResponse = serde_json::from_slice(bytes).map_err(|err| {
        SandboxError::Worker(format!(
            "guardrail worker answered with an unreadable response: {err}{}",
            describe_diagnostics(diagnostics)
        ))
    })?;
    match response {
        WorkerResponse::Passed {
            fuel_consumed,
            message,
            ..
        } => Ok(SandboxOutcome {
            passed: true,
            fuel_consumed,
            duration_ms: 0,
            message,
        }),
        WorkerResponse::Failed { error } => Err(SandboxError::Execution(error)),
    }
}

fn describe_diagnostics(diagnostics: &[u8]) -> String {
    let text = String::from_utf8_lossy(diagnostics).trim().to_string();
    if text.is_empty() {
        return String::new();
    }
    let bound = text
        .char_indices()
        .nth(MAX_REPORTED_STDERR_CHARS)
        .map_or(text.as_str(), |(idx, _)| &text[..idx]);
    format!(" (worker stderr: {bound})")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn missing_worker_fails_closed_without_touching_the_host() {
        let limits = SandboxLimits {
            fuel: 10_000,
            timeout: Duration::from_secs(1),
            max_memory_bytes: 1024 * 1024,
        };
        let outcome = execute_in_worker(
            Path::new("definitely-not-a-worker-binary"),
            &limits,
            br#"(module (func (export "_start")))"#,
        )
        .await;
        let error = outcome.unwrap_err();
        assert!(error.to_string().contains("guardrail worker unavailable"));
        assert!(matches!(error, SandboxError::Worker(_)));
    }

    #[test]
    fn truncated_diagnostics_are_bounded() {
        let noise = vec![b'x'; MAX_REPORTED_STDERR_CHARS * 4];
        let described = describe_diagnostics(&noise);
        assert!(described.len() < MAX_REPORTED_STDERR_CHARS * 4 + 4);
        assert_eq!(describe_diagnostics(&[]), "");
    }
}
