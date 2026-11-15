//! Native Google Gemini generateContent adapter (not OpenAI compatibility).
//! The API key is passed in x-goog-api-key, never in URLs or diagnostics.
use std::collections::HashMap;
use std::time::Duration;

use async_trait::async_trait;
use futures::{StreamExt, stream};
use serde_json::{Value, json};

use crate::types::{
    ChatRequest, ContentBlock, ModelError, ModelProvider, ModelStream, Pricing,
    ProviderCapabilities, Role, StopReason, StreamEvent, Usage,
};

const MAX_RESPONSE_BYTES: usize = 4 * 1024 * 1024;

#[derive(Clone)]
pub struct GeminiConfig {
    pub id: String,
    pub base_url: String,
    pub api_key: Option<String>,
    pub default_pricing: Option<Pricing>,
}

impl std::fmt::Debug for GeminiConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GeminiConfig")
            .field("id", &self.id)
            .field("base_url", &self.base_url)
            .field("api_key", &self.api_key.as_ref().map(|_| "<redacted>"))
            .finish()
    }
}

impl GeminiConfig {
    pub fn new(id: impl Into<String>, base_url: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            base_url: base_url.into(),
            api_key: None,
            default_pricing: None,
        }
    }

    pub fn with_api_key(mut self, key: impl Into<String>) -> Self {
        self.api_key = Some(key.into());
        self
    }

    pub fn with_pricing(mut self, pricing: Pricing) -> Self {
        self.default_pricing = Some(pricing);
        self
    }

    fn endpoint(&self, model: &str) -> Result<String, ModelError> {
        if model.is_empty()
            || model.len() > 128
            || !model
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
            || model.contains("..")
        {
            return Err(ModelError::Configuration(
                "invalid Gemini model identifier".into(),
            ));
        }
        Ok(format!(
            "{}/models/{model}:generateContent",
            self.base_url.trim_end_matches('/')
        ))
    }
}

pub struct GeminiProvider {
    config: GeminiConfig,
    client: reqwest::Client,
}

impl std::fmt::Debug for GeminiProvider {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GeminiProvider")
            .field("config", &self.config)
            .finish()
    }
}

impl GeminiProvider {
    pub fn new(config: GeminiConfig) -> Result<Self, ModelError> {
        if config
            .api_key
            .as_ref()
            .is_none_or(|key| key.trim().is_empty())
        {
            return Err(ModelError::Configuration(
                "Gemini API requires an API key supplied through an environment variable".into(),
            ));
        }
        let client = reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(20))
            .timeout(Duration::from_secs(180))
            .redirect(reqwest::redirect::Policy::none())
            .user_agent(concat!("ryoko/", env!("CARGO_PKG_VERSION")))
            .build()?;
        Ok(Self { config, client })
    }
}

#[async_trait]
impl ModelProvider for GeminiProvider {
    fn id(&self) -> &str {
        &self.config.id
    }

    fn capabilities(&self) -> ProviderCapabilities {
        ProviderCapabilities {
            supports_streaming: false,
            supports_tools: true,
            supports_system_prompt: true,
            max_context_tokens: 128_000,
        }
    }

    fn pricing(&self, _model: &str) -> Option<Pricing> {
        self.config.default_pricing
    }

    async fn stream(&self, request: ChatRequest) -> Result<ModelStream, ModelError> {
        let endpoint = self.config.endpoint(&request.model)?;
        let body = gemini_wire_body(&request)?;
        let key = self
            .config
            .api_key
            .as_deref()
            .ok_or_else(|| ModelError::Configuration("Gemini API key is missing".into()))?;
        let response = self
            .client
            .post(endpoint)
            .header(reqwest::header::CONTENT_TYPE, "application/json")
            .header("x-goog-api-key", key)
            .json(&body)
            .send()
            .await?;
        if !response.status().is_success() {
            return Err(ModelError::Http {
                provider: self.config.id.clone(),
                status: response.status().as_u16(),
                body: "provider error details omitted to protect secrets".into(),
            });
        }
        let mut collected = Vec::new();
        let mut chunks = response.bytes_stream();
        while let Some(chunk) = chunks.next().await {
            let chunk = chunk?;
            if collected.len().saturating_add(chunk.len()) > MAX_RESPONSE_BYTES {
                return Err(ModelError::Decode("Gemini response exceeds 4 MiB".into()));
            }
            collected.extend_from_slice(&chunk);
        }
        let value: Value = serde_json::from_slice(&collected)
            .map_err(|_| ModelError::Decode("invalid Gemini JSON response".into()))?;
        let events = parse_gemini_response(&request.model, &value)?;
        Ok(Box::pin(stream::iter(events.into_iter().map(Ok))))
    }
}

/// Serialize chat messages into native Gemini contents and function messages.
pub fn gemini_wire_body(request: &ChatRequest) -> Result<Value, ModelError> {
    let mut system = Vec::new();
    if let Some(value) = request.system.as_ref().filter(|value| !value.is_empty()) {
        system.push(value.clone());
    }
    let mut contents = Vec::new();
    let mut tools_by_call: HashMap<&str, &str> = HashMap::new();
    for message in &request.messages {
        if message.role == Role::System {
            let text = message.text_content();
            if !text.is_empty() {
                system.push(text);
            }
            continue;
        }
        let mut parts = Vec::new();
        for block in &message.content {
            match block {
                ContentBlock::Text { text } => parts.push(json!({"text":text})),
                ContentBlock::ToolCall {
                    id,
                    name,
                    arguments,
                } => {
                    tools_by_call.insert(id, name);
                    parts.push(json!({"functionCall": {"id":id, "name":name, "args":arguments}}));
                }
                ContentBlock::ToolResult {
                    tool_call_id,
                    content,
                    is_error,
                } => {
                    let name = tools_by_call.get(tool_call_id.as_str()).ok_or_else(|| {
                        ModelError::Configuration(
                            "Gemini tool result has no preceding tool call".into(),
                        )
                    })?;
                    let response = if *is_error {
                        json!({"error":content})
                    } else {
                        json!({"result":content})
                    };
                    parts.push(json!({"functionResponse":{"id":tool_call_id, "name":name, "response":response}}));
                }
            }
        }
        if !parts.is_empty() {
            contents.push(json!({
                "role": if message.role == Role::Assistant { "model" } else { "user" },
                "parts": parts,
            }));
        }
    }
    if contents.is_empty() {
        return Err(ModelError::Configuration(
            "Gemini prompt cannot be empty".into(),
        ));
    }
    let mut body = json!({"contents": contents});
    if !system.is_empty() {
        body["systemInstruction"] = json!({"parts":[{"text":system.join("\n")}]});
    }
    let mut generation = json!({"maxOutputTokens": request.max_tokens.unwrap_or(2048)});
    if let Some(temperature) = request.temperature {
        generation["temperature"] = json!(temperature);
    }
    body["generationConfig"] = generation;
    if !request.tools.is_empty() {
        body["tools"] = json!([{"functionDeclarations":request.tools.iter().map(|tool| {
            json!({"name":tool.name, "description":tool.description, "parametersJsonSchema":tool.input_schema})
        }).collect::<Vec<_>>()}]);
    }
    Ok(body)
}

pub fn parse_gemini_response(model: &str, value: &Value) -> Result<Vec<StreamEvent>, ModelError> {
    let candidate = value["candidates"]
        .as_array()
        .and_then(|entries| entries.first())
        .ok_or_else(|| ModelError::Decode("Gemini returned no candidate".into()))?;
    let mut events = vec![StreamEvent::Start {
        model: model.to_owned(),
    }];
    let parts = candidate["content"]["parts"]
        .as_array()
        .ok_or_else(|| ModelError::Decode("Gemini candidate has no content parts".into()))?;
    for (index, part) in parts.iter().enumerate() {
        if let Some(text) = part.get("text").and_then(Value::as_str) {
            events.push(StreamEvent::TextDelta {
                text: text.to_owned(),
            });
        } else if let Some(call) = part.get("functionCall") {
            let name = call["name"].as_str().ok_or_else(|| {
                ModelError::Decode("Gemini function call is missing its name".into())
            })?;
            let args = call.get("args").cloned().unwrap_or_else(|| json!({}));
            let id = call["id"]
                .as_str()
                .map(str::to_owned)
                .unwrap_or_else(|| format!("gemini-tool-{index}"));
            events.push(StreamEvent::ToolCallDelta {
                index: index as u32,
                id: Some(id),
                name: Some(name.to_owned()),
                arguments_delta: args.to_string(),
            });
        }
    }
    if let Some(usage) = value.get("usageMetadata") {
        let input = usage["promptTokenCount"].as_u64().unwrap_or_default();
        let visible_output = usage["candidatesTokenCount"].as_u64().unwrap_or_default();
        let thinking = usage["thoughtsTokenCount"].as_u64().unwrap_or_default();
        let output = usage["totalTokenCount"]
            .as_u64()
            .map(|total| total.saturating_sub(input))
            .unwrap_or_default()
            .max(visible_output.saturating_add(thinking));
        events.push(StreamEvent::Usage(Usage {
            input_tokens: input,
            output_tokens: output,
            cached_input_tokens: usage["cachedContentTokenCount"]
                .as_u64()
                .unwrap_or_default(),
        }));
    }
    let stop_reason = match candidate["finishReason"].as_str() {
        Some("STOP") => StopReason::EndTurn,
        Some("MAX_TOKENS") => StopReason::MaxTokens,
        Some("FUNCTION_CALL") => StopReason::ToolUse,
        _ => StopReason::Other("gemini_other".into()),
    };
    events.push(StreamEvent::Finished { stop_reason });
    Ok(events)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{Message, ToolDefinition};

    #[test]
    fn native_gemini_request_uses_system_and_function_schema() {
        let request = ChatRequest::new("gemini-2.5-flash")
            .with_system("Explain carefully")
            .with_user("Hello")
            .with_tools(vec![ToolDefinition::new(
                "lookup",
                "Lookup a record",
                json!({"type":"object","properties":{"id":{"type":"string"}}}),
            )]);
        let body = gemini_wire_body(&request).unwrap();
        assert_eq!(
            body["systemInstruction"]["parts"][0]["text"],
            "Explain carefully"
        );
        assert_eq!(body["contents"][0]["parts"][0]["text"], "Hello");
        assert_eq!(
            body["tools"][0]["functionDeclarations"][0]["name"],
            "lookup"
        );
        assert_eq!(body["generationConfig"]["maxOutputTokens"], 2048);
    }

    #[test]
    fn parses_function_calls_and_usage() {
        let response = json!({
            "candidates":[{"content":{"parts":[{"text":"checking"},{"functionCall":{"id":"abc","name":"lookup","args":{"id":"2"}}}]},"finishReason":"STOP"}],
            "usageMetadata":{"promptTokenCount":10,"candidatesTokenCount":7,"cachedContentTokenCount":3,"thoughtsTokenCount":2,"totalTokenCount":19}
        });
        let events = parse_gemini_response("gemini-test", &response).unwrap();
        assert!(matches!(&events[1], StreamEvent::TextDelta { text } if text == "checking"));
        assert!(
            matches!(&events[2], StreamEvent::ToolCallDelta { name: Some(name), .. } if name == "lookup")
        );
        assert!(matches!(
            &events[3],
            StreamEvent::Usage(Usage {
                input_tokens: 10,
                output_tokens: 9,
                ..
            })
        ));
    }

    #[test]
    fn rejects_invalid_names_and_missing_keys() {
        assert!(
            GeminiConfig::new("gemini", "https://generativelanguage.googleapis.com/v1beta")
                .endpoint("../evil")
                .is_err()
        );
        assert!(
            GeminiProvider::new(GeminiConfig::new(
                "gemini",
                "https://generativelanguage.googleapis.com/v1beta"
            ))
            .is_err()
        );
    }

    #[test]
    fn maps_model_tool_roundtrip_without_losing_call_identity() {
        let request = ChatRequest::new("gemini")
            .with_user("Query")
            .with_message(Message::new(
                Role::Assistant,
                vec![ContentBlock::ToolCall {
                    id: "call-3".into(),
                    name: "lookup".into(),
                    arguments: json!({"x":1}),
                }],
            ))
            .with_message(Message::new(
                Role::Tool,
                vec![ContentBlock::ToolResult {
                    tool_call_id: "call-3".into(),
                    content: "OK".into(),
                    is_error: false,
                }],
            ));
        let body = gemini_wire_body(&request).unwrap();
        assert_eq!(
            body["contents"][2]["parts"][0]["functionResponse"]["name"],
            "lookup"
        );
    }
}
