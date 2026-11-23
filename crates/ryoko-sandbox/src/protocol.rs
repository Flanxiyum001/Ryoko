//! Typed request/response boundary between the host and the isolated
//! guardrail worker process.
//!
//! The host sends exactly one [`WorkerRequest`] on the worker's stdin and
//! reads exactly one [`WorkerResponse`] from its stdout. The request carries
//! only the artifact bytes and the enforced limits: no host paths, no
//! environment and no credentials cross the boundary.

use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::{SandboxError, SandboxLimits};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkerRequest {
    pub artifact_hex: String,
    pub fuel: u64,
    pub timeout_ms: u64,
    pub max_memory_bytes: usize,
}

impl WorkerRequest {
    pub fn new(artifact: &[u8], limits: &SandboxLimits) -> Self {
        Self {
            artifact_hex: encode_hex(artifact),
            fuel: limits.fuel,
            timeout_ms: limits.timeout.as_millis() as u64,
            max_memory_bytes: limits.max_memory_bytes,
        }
    }

    pub fn limits(&self) -> SandboxLimits {
        SandboxLimits {
            fuel: self.fuel,
            timeout: Duration::from_millis(self.timeout_ms),
            max_memory_bytes: self.max_memory_bytes,
        }
    }

    pub fn artifact(&self) -> Result<Vec<u8>, SandboxError> {
        decode_hex(&self.artifact_hex).map_err(SandboxError::Compile)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum WorkerResponse {
    Passed {
        fuel_consumed: u64,
        duration_ms: u64,
        message: String,
    },
    Failed {
        error: String,
    },
}

fn encode_hex(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push(HEX[(byte >> 4) as usize] as char);
        out.push(HEX[(byte & 0x0f) as usize] as char);
    }
    out
}

fn decode_hex(value: &str) -> Result<Vec<u8>, String> {
    fn nibble(byte: u8) -> Result<u8, String> {
        match byte {
            b'0'..=b'9' => Ok(byte - b'0'),
            b'a'..=b'f' => Ok(byte - b'a' + 10),
            _ => Err(format!("invalid hex digit {byte:?}")),
        }
    }
    let bytes = value.as_bytes();
    if !bytes.len().is_multiple_of(2) {
        return Err("odd-length artifact encoding".into());
    }
    let mut out = Vec::with_capacity(bytes.len() / 2);
    for pair in bytes.chunks(2) {
        out.push(nibble(pair[0])? << 4 | nibble(pair[1])?);
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hex_roundtrip_preserves_artifacts() {
        let artifact: &[u8] = b"\0\x01\xfe\x7f wasm magic \r\n\xff";
        let encoded = encode_hex(artifact);
        assert_eq!(decode_hex(&encoded).unwrap(), artifact);
        assert!(decode_hex("0").is_err());
        assert!(decode_hex("zz").is_err());
    }

    #[test]
    fn worker_request_carries_only_artifact_and_limits() {
        let request = WorkerRequest::new(
            b"(module)",
            &SandboxLimits {
                fuel: 123,
                timeout: Duration::from_secs(7),
                max_memory_bytes: 4096,
            },
        );
        assert_eq!(request.fuel, 123);
        assert_eq!(request.timeout_ms, 7_000);
        assert_eq!(request.max_memory_bytes, 4096);
        assert_eq!(request.artifact().unwrap(), b"(module)");
        let json = serde_json::to_string(&request).unwrap();
        assert!(json.contains("artifact_hex"));
        assert!(!json.contains("RUSTUP_HOME") && !json.contains("HOME"));
    }

    #[test]
    fn worker_response_is_tagged_and_roundtrips() {
        let passed = WorkerResponse::Passed {
            fuel_consumed: 42,
            duration_ms: 9,
            message: "clean exit".into(),
        };
        let json = serde_json::to_string(&passed).unwrap();
        assert!(json.contains("\"status\":\"passed\""));
        match serde_json::from_str::<WorkerResponse>(&json).unwrap() {
            WorkerResponse::Passed {
                fuel_consumed,
                duration_ms,
                message,
            } => {
                assert_eq!(fuel_consumed, 42);
                assert_eq!(duration_ms, 9);
                assert_eq!(message, "clean exit");
            }
            other => panic!("unexpected response {other:?}"),
        }
        let failed = WorkerResponse::Failed {
            error: "trap".into(),
        };
        assert!(
            serde_json::to_string(&failed)
                .unwrap()
                .contains("\"status\":\"failed\"")
        );
    }
}
