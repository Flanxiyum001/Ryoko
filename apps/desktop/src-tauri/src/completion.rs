//! Streaming local FIM completions for Monaco ghost text.
//!
//! Only a loopback runtime is accepted. Heavy DAG work stays on the model
//! router; this path deliberately has no cloud credentials and no agent loop.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use futures::StreamExt;
use ryoko_models::{FimClient, FimConfig};
use serde::Serialize;
use tauri::{AppHandle, Emitter, State};

const DEFAULT_BASE_URL: &str = "http://127.0.0.1:11434";
const DEFAULT_MODEL: &str = "qwen2.5-coder:1.5b";

#[derive(Default)]
pub struct CompletionState {
    generation: Arc<AtomicU64>,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct FimChunk {
    request_id: String,
    text: String,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct FimDone {
    request_id: String,
    cancelled: bool,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct FimError {
    request_id: String,
    message: String,
}

fn config() -> FimConfig {
    FimConfig::new(
        std::env::var("RYOKO_FIM_BASE_URL").unwrap_or_else(|_| DEFAULT_BASE_URL.to_owned()),
        std::env::var("RYOKO_FIM_MODEL").unwrap_or_else(|_| DEFAULT_MODEL.to_owned()),
    )
}

#[tauri::command]
pub async fn fim_complete(
    app: AppHandle,
    state: State<'_, CompletionState>,
    prefix: String,
    suffix: String,
    request_id: String,
) -> Result<(), String> {
    if request_id.len() > 100 || request_id.chars().any(char::is_control) {
        return Err("invalid completion request id".to_owned());
    }
    let generation = state.generation.clone();
    let request = generation.fetch_add(1, Ordering::SeqCst).saturating_add(1);
    let client = FimClient::new(config()).map_err(|error| error.to_string())?;
    let mut stream = client
        .stream(&prefix, &suffix)
        .await
        .map_err(|error| error.to_string())?;
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        while let Some(chunk) = stream.next().await {
            if generation.load(Ordering::SeqCst) != request {
                return;
            }
            match chunk {
                Ok(text) => {
                    if app
                        .emit(
                            "ryoko-fim-chunk",
                            FimChunk {
                                request_id: request_id.clone(),
                                text,
                            },
                        )
                        .is_err()
                    {
                        return;
                    }
                }
                Err(error) => {
                    let _ = app.emit(
                        "ryoko-fim-error",
                        FimError {
                            request_id,
                            message: error.to_string(),
                        },
                    );
                    return;
                }
            }
        }
        if generation.load(Ordering::SeqCst) == request {
            let _ = app.emit(
                "ryoko-fim-done",
                FimDone {
                    request_id,
                    cancelled: false,
                },
            );
        }
    });
    Ok(())
}

#[tauri::command]
pub fn fim_cancel(state: State<'_, CompletionState>) {
    state.generation.fetch_add(1, Ordering::SeqCst);
}
