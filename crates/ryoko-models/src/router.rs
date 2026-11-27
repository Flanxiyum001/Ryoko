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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning: Option<String>,
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
        let scope = request.scope.clone();
        self.ledger.check_budget_for(scope.as_deref())?;
        let (entry, bare_model) = self.resolve(&request.model)?;

        let mut provider_request = request;
        provider_request.model = bare_model.clone();

        let provider_id = entry.provider.id().to_owned();
        let pricing = entry
            .provider
            .pricing(&bare_model)
            .or(self.fallback_pricing);
        let ledger = self.ledger.clone();
        let reservation = if ledger.budget().is_some() {
            let pricing = pricing.ok_or_else(|| {
                ModelError::Configuration("bounded runs require pricing for every model".to_owned())
            })?;
            let output = provider_request.max_tokens.ok_or_else(|| {
                ModelError::Configuration(
                    "bounded runs require max_tokens for every request".to_owned(),
                )
            })?;
            // UTF-8 bytes upper-bound text token counts; include serialized
            // protocol overhead and price all input at the dearer rate.
            let input = serde_json::to_vec(&provider_request)
                .map_err(|error| ModelError::Configuration(error.to_string()))?
                .len() as u64
                + 1024;
            let tokens = input.saturating_add(u64::from(output));
            let input_rate = pricing.input_per_mtok.max(pricing.cached_input_per_mtok);
            let cost =
                (input as f64 * input_rate + output as f64 * pricing.output_per_mtok) / 1_000_000.0;
            ledger.reserve(scope.as_deref(), cost, tokens)?
        } else {
            None
        };

        let stream = entry.provider.stream(provider_request).await?;
        let mut reservation = reservation;
        let mapped = stream.map(move |item| {
            if let Ok(StreamEvent::Usage(usage)) = &item {
                if let Some(reserved) = reservation.take() {
                    reserved.settle(&provider_id, &bare_model, *usage, pricing);
                } else {
                    ledger.record_for(scope.as_deref(), &provider_id, &bare_model, *usage, pricing);
                }
            }
            item
        });
        Ok(Box::pin(mapped))
    }

    pub async fn complete(&self, request: ChatRequest) -> Result<Completion, ModelError> {
        let scope = request.scope.clone();
        // Start the per-task clock before opening the network connection.
        self.ledger.check_budget_for(scope.as_deref())?;
        let remaining = self.ledger.remaining_time_for(scope.as_deref());
        let work = async {
            let requested_model = request.model.clone();
            // The deadline includes provider startup and stream creation,
            // not merely collection once the remote connection exists.
            let stream = self.stream(request).await?;
            let mut completion = Completion {
                model: requested_model,
                ..Completion::default()
            };
            collect_stream(stream, &mut completion).await?;
            self.ledger.check_budget_for(scope.as_deref())?;
            Ok(completion)
        };
        if let Some(duration) = remaining {
            match tokio::time::timeout(duration, work).await {
                Ok(result) => result,
                Err(_) => Err(self.ledger.timeout_error(scope.as_deref())),
            }
        } else {
            work.await
        }
    }

    pub fn snapshot(&self) -> crate::ledger::LedgerSnapshot {
        self.ledger.snapshot()
    }
}

pub async fn collect_stream(
    mut stream: ModelStream,
    completion: &mut Completion,
) -> Result<(), ModelError> {
    // Explicit completion caps guard against unlimited model-controlled data.
    const MAX_TEXT_BYTES: usize = 4 * 1024 * 1024;
    const MAX_TOOL_CALLS: usize = 128;
    const MAX_TOOL_ARGUMENT_BYTES: usize = 1024 * 1024;
    let mut partial_calls: BTreeMap<u32, (String, String, String)> = BTreeMap::new();
    let mut provider_seen = false;

    while let Some(event) = stream.next().await {
        match event? {
            StreamEvent::Start { model } => {
                completion.model = model;
                provider_seen = true;
            }
            StreamEvent::TextDelta { text } => {
                if completion.text.len().saturating_add(text.len()) > MAX_TEXT_BYTES {
                    return Err(ModelError::Decode(
                        "model text exceeded completion size limit".into(),
                    ));
                }
                completion.text.push_str(&text);
            }
            StreamEvent::ReasoningDelta { text } => {
                let reasoning = completion.reasoning.get_or_insert_with(String::new);
                if reasoning.len().saturating_add(text.len()) > MAX_TEXT_BYTES {
                    return Err(ModelError::Decode(
                        "model reasoning exceeded completion size limit".into(),
                    ));
                }
                reasoning.push_str(&text);
            }
            StreamEvent::ToolCallDelta {
                index,
                id,
                name,
                arguments_delta,
            } => {
                if !partial_calls.contains_key(&index) && partial_calls.len() >= MAX_TOOL_CALLS {
                    return Err(ModelError::Decode("too many streamed tool calls".into()));
                }
                let entry = partial_calls.entry(index).or_default();
                if arguments_delta.len().saturating_add(entry.2.len()) > MAX_TOOL_ARGUMENT_BYTES {
                    return Err(ModelError::Decode(
                        "streamed tool arguments exceeded size limit".into(),
                    ));
                }
                if let Some(id) = id {
                    if id.len() > 512 {
                        return Err(ModelError::Decode("streamed tool call id too long".into()));
                    }
                    entry.0 = id;
                }
                if let Some(name) = name {
                    if name.len() > 512 {
                        return Err(ModelError::Decode("streamed tool name too long".into()));
                    }
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
    async fn aggregation_rejects_unbounded_provider_output() {
        let text_stream = futures::stream::iter([Ok(StreamEvent::TextDelta {
            text: "a".repeat(4 * 1024 * 1024 + 1),
        })]);
        assert!(matches!(
            collect_stream(Box::pin(text_stream), &mut Completion::default()).await,
            Err(ModelError::Decode(_))
        ));

        let tool_stream = futures::stream::iter([Ok(StreamEvent::ToolCallDelta {
            index: 0,
            id: Some("call".into()),
            name: Some("tool".into()),
            arguments_delta: "x".repeat(1024 * 1024 + 1),
        })]);
        assert!(matches!(
            collect_stream(Box::pin(tool_stream), &mut Completion::default()).await,
            Err(ModelError::Decode(_))
        ));
    }

    #[tokio::test]
    async fn budget_blocks_dispatch() {
        let mock = Arc::new(MockProvider::new("mock").with_pricing(Pricing::new(100.0, 100.0)));
        mock.push_turn(ScriptedTurn::text("too expensive").with_usage(Usage::new(10_000, 10_000)));
        let ledger = UsageLedger::with_budget(0.0001);
        let mut router = ModelRouter::new("mock").with_ledger(ledger);
        router.register(mock);

        let first = router
            .complete(ChatRequest::new("mock-1").with_max_tokens(1_000))
            .await
            .unwrap_err();
        assert!(matches!(first, ModelError::BudgetExceeded { .. }));

        let err = router
            .complete(ChatRequest::new("mock-1").with_max_tokens(1_000))
            .await
            .unwrap_err();
        assert!(matches!(err, ModelError::BudgetExceeded { .. }));
    }
}
