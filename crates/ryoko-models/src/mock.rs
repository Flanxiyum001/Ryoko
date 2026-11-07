use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use futures::stream;

use crate::types::{
    ChatRequest, ModelError, ModelProvider, ModelStream, Pricing, ProviderCapabilities, StopReason,
    StreamEvent, Usage,
};

#[derive(Debug, Clone)]
pub struct ScriptedToolCall {
    pub id: String,
    pub name: String,
    pub arguments: serde_json::Value,
}

impl ScriptedToolCall {
    pub fn new(
        id: impl Into<String>,
        name: impl Into<String>,
        arguments: serde_json::Value,
    ) -> Self {
        Self {
            id: id.into(),
            name: name.into(),
            arguments,
        }
    }
}

/// One deterministic assistant turn returned by [`MockProvider`].
#[derive(Debug, Clone)]
pub struct ScriptedTurn {
    pub text_chunks: Vec<String>,
    pub tool_calls: Vec<ScriptedToolCall>,
    pub usage: Usage,
    pub stop_reason: StopReason,
}

impl ScriptedTurn {
    pub fn text(text: impl Into<String>) -> Self {
        Self {
            text_chunks: vec![text.into()],
            tool_calls: Vec::new(),
            usage: Usage::new(0, 0),
            stop_reason: StopReason::EndTurn,
        }
    }

    pub fn chunks<I, S>(chunks: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        Self {
            text_chunks: chunks.into_iter().map(Into::into).collect(),
            ..Self::text("")
        }
    }

    pub fn with_usage(mut self, usage: Usage) -> Self {
        self.usage = usage;
        self
    }

    pub fn with_tool_call(mut self, call: ScriptedToolCall) -> Self {
        self.tool_calls.push(call);
        self.stop_reason = StopReason::ToolUse;
        self
    }

    pub fn with_stop_reason(mut self, reason: StopReason) -> Self {
        self.stop_reason = reason;
        self
    }
}

/// A scripted provider: no network, fully deterministic, and it records every
/// request so tests can assert on the normalised wire shape.
#[derive(Debug, Clone)]
pub struct MockProvider {
    id: String,
    script: Arc<Mutex<VecDeque<ScriptedTurn>>>,
    requests: Arc<Mutex<Vec<ChatRequest>>>,
    pricing: Option<Pricing>,
    capabilities: ProviderCapabilities,
}

impl MockProvider {
    pub fn new(id: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            script: Arc::new(Mutex::new(VecDeque::new())),
            requests: Arc::new(Mutex::new(Vec::new())),
            pricing: None,
            capabilities: ProviderCapabilities {
                supports_streaming: true,
                supports_tools: true,
                supports_system_prompt: true,
                max_context_tokens: 200_000,
            },
        }
    }

    pub fn with_pricing(mut self, pricing: Pricing) -> Self {
        self.pricing = Some(pricing);
        self
    }

    pub fn with_capabilities(mut self, capabilities: ProviderCapabilities) -> Self {
        self.capabilities = capabilities;
        self
    }

    pub fn push_turn(&self, turn: ScriptedTurn) {
        self.script
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push_back(turn);
    }

    pub fn requests(&self) -> Vec<ChatRequest> {
        self.requests
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }
}

#[async_trait]
impl ModelProvider for MockProvider {
    fn id(&self) -> &str {
        &self.id
    }

    fn capabilities(&self) -> ProviderCapabilities {
        self.capabilities.clone()
    }

    fn pricing(&self, _model: &str) -> Option<Pricing> {
        self.pricing
    }

    async fn stream(&self, request: ChatRequest) -> Result<ModelStream, ModelError> {
        self.requests
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push(request.clone());

        let turn = self
            .script
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .pop_front()
            .ok_or_else(|| ModelError::Provider {
                provider: self.id.clone(),
                message: "mock script exhausted".to_owned(),
            })?;

        let mut events = vec![StreamEvent::Start {
            model: request.model.clone(),
        }];
        for chunk in turn.text_chunks {
            if !chunk.is_empty() {
                events.push(StreamEvent::TextDelta { text: chunk });
            }
        }
        for (index, call) in turn.tool_calls.iter().enumerate() {
            events.push(StreamEvent::ToolCallDelta {
                index: index as u32,
                id: Some(call.id.clone()),
                name: Some(call.name.clone()),
                arguments_delta: call.arguments.to_string(),
            });
        }
        events.push(StreamEvent::Usage(turn.usage));
        events.push(StreamEvent::Finished {
            stop_reason: turn.stop_reason,
        });

        Ok(Box::pin(stream::iter(events.into_iter().map(Ok))))
    }
}
