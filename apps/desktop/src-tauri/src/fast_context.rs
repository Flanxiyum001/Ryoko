//! Shared Fast Context index for the open workspace.
//!
//! Built on first use and refreshed when it is older than [`MAX_AGE`] or the
//! workspace changes, so chat and review get related code in milliseconds.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use ryoko_harness::{ContextHit, FastContext};
use serde::Serialize;
use tauri::State;

use crate::workspace::{WorkspaceState, current_root};

const MAX_AGE: Duration = Duration::from_secs(30);

#[derive(Default)]
pub struct FastContextState {
    cached: Mutex<Option<(PathBuf, Instant, Arc<FastContext>)>>,
}

impl FastContextState {
    /// Returns a fresh-enough index for `root`, building it if needed.
    pub fn index(&self, root: &Path) -> Result<Arc<FastContext>, String> {
        if let Some((cached_root, built, index)) =
            self.cached.lock().expect("fast context").as_ref()
        {
            if cached_root == root && built.elapsed() < MAX_AGE {
                return Ok(index.clone());
            }
        }
        let index = Arc::new(FastContext::build(root).map_err(|error| error.to_string())?);
        *self.cached.lock().expect("fast context") =
            Some((root.to_path_buf(), Instant::now(), index.clone()));
        Ok(index)
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FastContextResult {
    hits: Vec<ContextHit>,
    files_indexed: usize,
    search_ms: f64,
}

#[tauri::command]
pub async fn fast_context_search(
    query: String,
    limit: Option<usize>,
    workspace: State<'_, WorkspaceState>,
    state: State<'_, FastContextState>,
) -> Result<FastContextResult, String> {
    if query.trim().is_empty() || query.chars().count() > 500 {
        return Err("query must be 1–500 characters".into());
    }
    let root = current_root(&workspace)?;
    let index = state.index(&root)?;
    let started = Instant::now();
    let hits = index.search(&query, limit.unwrap_or(8).clamp(1, 30));
    Ok(FastContextResult {
        hits,
        files_indexed: index.file_count(),
        search_ms: started.elapsed().as_secs_f64() * 1000.0,
    })
}
