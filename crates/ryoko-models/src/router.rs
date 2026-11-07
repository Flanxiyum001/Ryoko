use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;

use futures::StreamExt;
use serde::{Deserialize, Serialize};

use crate::ledger::UsageLedger;
use crate::types::{
    ChatRequest, ModelError, ModelProvider, ModelStream, Pricing, StreamEvent, ToolCall, Usage,
};

#[derive(Clone)]
pub struct ProviderEntry {
    pub provider: Arc<dyn ModelProvider>,
}

impl std::fmt::Debug for ProviderEntry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ProviderEntry")
            .field("provider", &self.provider.id())
            .finish()
    }
}

/// Fully aggregated assistant turn.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Completion {
    pub provider: String,
    pub model: String,
    pub text: String,
    pub tool_calls: Vec<ToolCall>,
    pub usage: Usage,
    pub stop_reason: Option<crate::types::StopReason>,
}

/// Routes `provider/model` identifiers to registered backends and funnels every
/// reported usage event into the shared ledger.
#[derive(Clone)]
pub struct ModelRouter {
    providers: HashMap<String, ProviderEntry>,
    default_provider: String,
    ledger: UsageLedger,
    fallback_pricing: Option<Pricing>,
}

impl std::fmt::Debug for ModelRouter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ModelRouter")
            .field("default_provider", &self.default_provider)
            .field("providers", &self.providers.keys().collect::<Vec<_>>())
            .finish()
    }
}

impl ModelRouter {
    pub fn new(default_provider: impl Into<String>) -> Self {
        Self {
            providers: HashMap::new(),
            default_provider: default_provider.into(),
            ledger: UsageLedger::new(None),
            fallback_pricing: None,
        }
    }

    pub fn with_ledger(mut self, ledger: UsageLedger) -> Self {
        self.ledger = ledger;
        self
    }

    pub fn with_fallback_pricing(mut self, pricing: Pricing) -> Self {
        self.fallback_pricing = Some(pricing);
        self
    }

    pub fn register(&mut self, provider: Arc<dyn ModelProvider>) -> &mut Self {
        let id = provider.id().to_owned();
        self.providers.insert(id, ProviderEntry { provider });
        self
    }

    pub fn default_provider(&self) -> &str {
        &self.default_provider
    }

    pub fn ledger(&self) -> &UsageLedger {
        &self.ledger
    }

    pub fn providers(&self) -> impl Iterator<Item = &str> {
        self.providers.keys().map(String::as_str)
    }

    fn resolve(&self, model: &str) -> Result<(&ProviderEntry, String), ModelError> {
        let (provider_id, bare_model) = match model.split_once('/') {
            Some((provider, rest)) if !provider.is_empty() && !rest.is_empty() => (provider, rest),
            Some(_) => {
                return Err(ModelError::UnknownModel {
                    model: model.to_owned(),
                });
            }
            None => (self.default_provider.as_str(), model),
        };

        let entry = self
            .providers
            .get(provider_id)
            .ok_or_else(|| ModelError::UnknownProvider {
                provider: provider_id.to_owned(),
            })?;
        Ok((entry, bare_model.to_owned()))
    }

    pub async fn stream(&self, request: ChatRequest) -> Result<ModelStream, ModelError> {
        self.ledger.check_budget()?;
        let (entry, bare_model) = self.resolve(&request.model)?;

        let mut provider_request = request;
        provider_request.model = bare_model.clone();

        let provider_id = entry.provider.id().to_owned();
        let pricing = entry
            .provider
            .pricing(&bare_model)
            .or(self.fallback_pricing);
        let ledger = self.ledger.clone();

        let stream = entry.provider.stream(provider_request).await?;
        let mapped = stream.map(move |item| {
            if let Ok(StreamEvent::Usage(usage)) = &item {
                ledger.record(&provider_id, &bare_model, *usage, pricing);
            }
            item
        });
        Ok(Box::pin(mapped))
    }

    pub async fn complete(&self, request: ChatRequest) -> Result<Completion, ModelError> {
        let requested_model = request.model.clone();
        let stream = self.stream(request).await?;
        let mut completion = Completion {
            model: requested_model,
            ..Completion::default()
        };
        collect_stream(stream, &mut completion).await?;
        Ok(completion)
    }

    pub fn snapshot(&self) -> crate::ledger::LedgerSnapshot {
        self.ledger.snapshot()
    }
}

pub async fn collect_stream(
    mut stream: ModelStream,
    completion: &mut Completion,
) -> Result<(), ModelError> {
    let mut partial_calls: BTreeMap<u32, (String, String, String)> = BTreeMap::new();
    let mut provider_seen = false;

    while let Some(event) = stream.next().await {
        match event? {
            StreamEvent::Start { model } => {
                completion.model = model;
                provider_seen = true;
            }
            StreamEvent::TextDelta { text } => completion.text.push_str(&text),
            StreamEvent::ToolCallDelta {
                index,
                id,
                name,
                arguments_delta,
            } => {
                let entry = partial_calls.entry(index).or_default();
                if let Some(id) = id {
                    entry.0 = id;
                }
                if let Some(name) = name {
                    entry.1 = name;
                }
                entry.2.push_str(&arguments_delta);
            }
            StreamEvent::Usage(usage) => completion.usage += usage,
            StreamEvent::Finished { stop_reason } => {
                completion.stop_reason = Some(stop_reason);
            }
        }
    }

    for (_, (id, name, arguments)) in partial_calls {
        let arguments =
            serde_json::from_str(&arguments).unwrap_or(serde_json::Value::String(arguments));
        completion.tool_calls.push(ToolCall {
            id,
            name,
            arguments,
        });
    }

    if !provider_seen {
        tracing::debug!("model stream finished without a start event");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mock::{MockProvider, ScriptedToolCall, ScriptedTurn};
    use crate::types::{Message, Role};

    #[tokio::test]
    async fn aggregates_stream_and_records_usage() {
        let mock = Arc::new(MockProvider::new("mock").with_pricing(Pricing::new(2.0, 10.0)));
        mock.push_turn(
            ScriptedTurn::chunks(["Hel", "lo, ", "Ryoko"])
                .with_usage(Usage::new(100, 20))
                .with_tool_call(ScriptedToolCall::new(
                    "call-1",
                    "read_file",
                    serde_json::json!({"path": "src/main.rs"}),
                )),
        );

        let mut router = ModelRouter::new("mock");
        router.register(mock.clone());

        let completion = router
            .complete(
                ChatRequest::new("mock-1")
                    .with_system("be precise")
                    .with_message(Message::text(Role::User, "hello")),
            )
            .await
            .unwrap();

        assert_eq!(completion.text, "Hello, Ryoko");
        assert_eq!(completion.tool_calls.len(), 1);
        assert_eq!(completion.tool_calls[0].name, "read_file");
        assert_eq!(
            completion.tool_calls[0].arguments,
            serde_json::json!({"path": "src/main.rs"})
        );
        assert_eq!(completion.usage, Usage::new(100, 20));
        assert_eq!(router.snapshot().total.requests, 1);

        let requests = mock.requests();
        assert_eq!(requests[0].system.as_deref(), Some("be precise"));
        assert_eq!(requests[0].model, "mock-1");
    }

    #[tokio::test]
    async fn routes_prefixed_models_and_rejects_unknown_providers() {
        let mock = Arc::new(MockProvider::new("mock"));
        mock.push_turn(ScriptedTurn::text("ok"));
        let mut router = ModelRouter::new("mock");
        router.register(mock.clone());

        router
            .complete(ChatRequest::new("mock/fast-model"))
            .await
            .unwrap();
        assert_eq!(mock.requests()[0].model, "fast-model");

        let err = router
            .complete(ChatRequest::new("nope/some-model"))
            .await
            .unwrap_err();
        assert!(matches!(err, ModelError::UnknownProvider { .. }));
    }

    #[tokio::test]
    async fn budget_blocks_dispatch() {
        let mock = Arc::new(MockProvider::new("mock").with_pricing(Pricing::new(100.0, 100.0)));
        mock.push_turn(ScriptedTurn::text("too expensive").with_usage(Usage::new(10_000, 10_000)));
        let ledger = UsageLedger::with_budget(0.0001);
        let mut router = ModelRouter::new("mock").with_ledger(ledger);
        router.register(mock);

        router.complete(ChatRequest::new("mock-1")).await.unwrap();

        let err = router
            .complete(ChatRequest::new("mock-1"))
            .await
            .unwrap_err();
        assert!(matches!(err, ModelError::BudgetExceeded { .. }));
    }
}
