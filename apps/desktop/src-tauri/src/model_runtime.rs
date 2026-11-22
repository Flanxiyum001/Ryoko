//! Model wiring for the editor workbench, mirroring the CLI's
//! `ModelArgs::load` without process-global state.
//!
//! The provider catalog is read from the workspace settings file
//! (`.ryoko/desktop/providers.json`, validated by [`crate::settings`]); API
//! keys stay in environment variables referenced by `api_key_env`, never in
//! the file itself. With no catalog, a direct OpenAI-compatible route is
//! configured from the environment (`RYOKO_MODEL`, `RYOKO_BASE_URL`,
//! `RYOKO_API_KEY` or `OPENAI_API_KEY`).

use std::path::Path;
use std::sync::Arc;

use ryoko_models::{
    ModelRouter, OpenAiCompatConfig, OpenAiCompatProvider, ProviderCatalog, ProviderProfile,
    UsageLedger,
};

fn env(name: &str) -> Option<String> {
    std::env::var(name).ok().filter(|value| !value.is_empty())
}

/// Resolves the model selection and a router that can serve it, preferring
/// the workspace catalog over the environment fallback.
pub fn load_router(root: &Path, model: Option<&str>) -> Result<(String, Arc<ModelRouter>), String> {
    let model = model
        .map(str::trim)
        .filter(|model| !model.is_empty())
        .map(str::to_owned)
        .or_else(|| env("RYOKO_MODEL"))
        .ok_or_else(|| {
            "no model selected: pick one in the settings panel or set RYOKO_MODEL".to_owned()
        })?;
    let ledger = UsageLedger::new(None);

    let catalog_path = crate::settings::settings_dir(root).join(crate::settings::PROVIDERS_FILE);
    if catalog_path.is_file() {
        let path = catalog_path
            .canonicalize()
            .map_err(|error| format!("cannot resolve {}: {error}", catalog_path.display()))?;
        let json = std::fs::read_to_string(&path)
            .map_err(|error| format!("cannot read provider catalog: {error}"))?;
        if json.len() > 256 * 1024 {
            return Err("provider catalog exceeds 256 KiB".to_owned());
        }
        let catalog = ProviderCatalog::parse_json(&json)
            .map_err(|error| format!("invalid provider catalog: {error}"))?;
        let router = catalog
            .build_router_for_models([model.as_str()])
            .map_err(|error| format!("invalid provider catalog: {error}"))?
            .with_ledger(ledger);
        return Ok((model, Arc::new(router)));
    }

    let base_url = env("RYOKO_BASE_URL").unwrap_or_else(|| "https://api.openai.com/v1".to_owned());
    let api_key = env("RYOKO_API_KEY").or_else(|| env("OPENAI_API_KEY"));
    let provider_id = model
        .split_once('/')
        .map(|(provider, _)| provider)
        .filter(|provider| !provider.is_empty())
        .unwrap_or("openai");

    ProviderProfile {
        id: provider_id.to_owned(),
        base_url: base_url.clone(),
        api_key_env: None,
        chat_path: None,
        allow_insecure_http: false,
        wire: None,
    }
    .validate()
    .map_err(|error| format!("invalid model provider route: {error}"))?;
    let mut config = OpenAiCompatConfig::new(provider_id, base_url);
    if let Some(api_key) = api_key {
        config = config.with_api_key(api_key);
    }
    let provider = OpenAiCompatProvider::new(config)
        .map_err(|error| format!("could not configure model provider: {error}"))?;
    let mut router = ModelRouter::new(provider_id).with_ledger(ledger);
    router.register(Arc::new(provider));
    Ok((model, Arc::new(router)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::path::PathBuf;

    struct TempWorkspace(PathBuf);

    impl TempWorkspace {
        fn new() -> Self {
            let root = std::env::temp_dir().join(format!(
                "ryoko-model-runtime-{}-{}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|duration| duration.as_nanos())
                    .unwrap_or_default()
            ));
            fs::create_dir_all(&root).expect("temp workspace");
            Self(root)
        }
    }

    impl Drop for TempWorkspace {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn a_missing_model_selection_is_refused_with_guidance() {
        let workspace = TempWorkspace::new();
        let error = load_router(&workspace.0, None).expect_err("no model anywhere");
        assert!(error.contains("RYOKO_MODEL") || error.contains("settings panel"));
    }

    #[test]
    fn a_workspace_catalog_builds_a_router_for_the_selected_model() {
        let workspace = TempWorkspace::new();
        crate::settings::save_providers(
            &workspace.0,
            &serde_json::json!({
                "default_provider": "ollama",
                "providers": [{
                    "id": "ollama",
                    "base_url": "http://127.0.0.1:11434/v1",
                    "api_key_env": "RYOKO_MUXCHECK_KEY"
                }]
            })
            .to_string(),
        )
        .expect("valid catalog saves");
        std::env::set_var("RYOKO_MUXCHECK_KEY", "test-token");
        let (model, _router) = load_router(&workspace.0, Some("qwen3-coder"))
            .expect("catalog covers a bare model id via default_provider");
        assert_eq!(model, "qwen3-coder");
    }

    #[test]
    fn a_model_unknown_to_the_catalog_is_refused_without_contacting_providers() {
        let workspace = TempWorkspace::new();
        crate::settings::save_providers(
            &workspace.0,
            &serde_json::json!({
                "default_provider": "ollama",
                "providers": [{
                    "id": "ollama",
                    "base_url": "http://127.0.0.1:11434/v1",
                    "api_key_env": "RYOKO_OLLAMA_KEY"
                }]
            })
            .to_string(),
        )
        .expect("valid catalog saves");
        assert!(load_router(&workspace.0, Some("missing/variant")).is_err());
    }
}
