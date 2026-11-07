use std::collections::BTreeMap;

use async_trait::async_trait;
use futures::StreamExt;
use serde_json::{Value, json};

use crate::types::{
    ChatRequest, ContentBlock, Message, ModelError, ModelProvider, ModelStream, Pricing,
    ProviderCapabilities, Role, StopReason, StreamEvent, Usage,
};

/// Configuration for any endpoint that speaks the OpenAI chat-completions wire
/// protocol (OpenAI, Azure-compatible gateways, Ollama, vLLM, OpenRouter, ...).
#[derive(Clone)]
pub struct OpenAiCompatConfig {
    pub id: String,
    pub base_url: String,
    /// Optional: local runtimes such as Ollama do not require a key.
    pub api_key: Option<String>,
    pub chat_path: String,
    pub default_pricing: Option<Pricing>,
    pub headers: BTreeMap<String, String>,
}

impl std::fmt::Debug for OpenAiCompatConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OpenAiCompatConfig")
            .field("id", &self.id)
            .field("base_url", &self.base_url)
            .field("api_key", &self.api_key.as_ref().map(|_| "<redacted>"))
            .field("chat_path", &self.chat_path)
            .finish()
    }
}

impl OpenAiCompatConfig {
    pub fn new(id: impl Into<String>, base_url: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            base_url: base_url.into(),
            api_key: None,
            chat_path: "/chat/completions".to_owned(),
            default_pricing: None,
            headers: BTreeMap::new(),
        }
    }

    pub fn with_api_key(mut self, api_key: impl Into<String>) -> Self {
        self.api_key = Some(api_key.into());
        self
    }

    pub fn with_pricing(mut self, pricing: Pricing) -> Self {
        self.default_pricing = Some(pricing);
        self
    }

    pub fn with_header(mut self, name: impl Into<String>, value: impl Into<String>) -> Self {
        self.headers.insert(name.into(), value.into());
        self
    }

    fn endpoint(&self) -> String {
        format!(
            "{}{}",
            self.base_url.trim_end_matches('/'),
            if self.chat_path.starts_with('/') {
                self.chat_path.clone()
            } else {
                format!("/{}", self.chat_path)
            }
        )
    }
}

pub struct OpenAiCompatProvider {
    config: OpenAiCompatConfig,
    client: reqwest::Client,
}

impl std::fmt::Debug for OpenAiCompatProvider {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OpenAiCompatProvider")
            .field("config", &self.config)
            .finish()
    }
}

impl OpenAiCompatProvider {
    pub fn new(config: OpenAiCompatConfig) -> Result<Self, ModelError> {
        let client = reqwest::Client::builder()
            .connect_timeout(std::time::Duration::from_secs(20))
            .timeout(std::time::Duration::from_secs(180))
            .redirect(reqwest::redirect::Policy::none())
            .user_agent(concat!("ryoko/", env!("CARGO_PKG_VERSION")))
            .build()?;
        Ok(Self { config, client })
    }

    pub fn config(&self) -> &OpenAiCompatConfig {
        &self.config
    }
}

#[async_trait]
impl ModelProvider for OpenAiCompatProvider {
    fn id(&self) -> &str {
        &self.config.id
    }

    fn capabilities(&self) -> ProviderCapabilities {
        ProviderCapabilities {
            supports_streaming: true,
            supports_tools: true,
            supports_system_prompt: true,
            max_context_tokens: 128_000,
        }
    }

    fn pricing(&self, _model: &str) -> Option<Pricing> {
        self.config.default_pricing
    }

    async fn stream(&self, request: ChatRequest) -> Result<ModelStream, ModelError> {
        let body = wire_body(&request, true);
        let mut builder = self
            .client
            .post(self.config.endpoint())
            .header(reqwest::header::CONTENT_TYPE, "application/json")
            .json(&body);

        if let Some(key) = &self.config.api_key {
            if !key.is_empty() {
                builder = builder.bearer_auth(key);
            }
        }
        for (name, value) in &self.config.headers {
            builder = builder.header(name, value);
        }

        let response = builder.send().await?;
        let status = response.status();
        if !status.is_success() {
            // Provider error bodies can echo credentials and be arbitrarily large.
            // Keep only the status and provider identifier in propagated errors.
            return Err(ModelError::Http {
                provider: self.config.id.clone(),
                status: status.as_u16(),
                body: "provider error details omitted to protect secrets".to_owned(),
            });
        }

        Ok(decode_sse(response, self.config.id.clone()))
    }
}

fn truncate(text: &str, max: usize) -> String {
    if text.len() <= max {
        return text.to_owned();
    }
    let mut end = max;
    while end > 0 && !text.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}...", &text[..end])
}

/// Converts a normalized request into the OpenAI wire shape.
pub fn wire_body(request: &ChatRequest, stream: bool) -> Value {
    let mut messages: Vec<Value> = Vec::new();

    if let Some(system) = &request.system {
        if !system.is_empty() {
            messages.push(json!({"role": "system", "content": system}));
        }
    }

    for message in &request.messages {
        messages.push(wire_message(message));
    }

    let mut body = json!({
        "model": request.model,
        "messages": messages,
        "stream": stream,
    });
    if stream {
        body["stream_options"] = json!({"include_usage": true});
    }
    if let Some(temperature) = request.temperature {
        body["temperature"] = json!(temperature);
    }
    if let Some(max_tokens) = request.max_tokens {
        body["max_tokens"] = json!(max_tokens);
    }
    if !request.tools.is_empty() {
        let tools: Vec<Value> = request
            .tools
            .iter()
            .map(|tool| {
                json!({
                    "type": "function",
                    "function": {
                        "name": tool.name,
                        "description": tool.description,
                        "parameters": tool.input_schema,
                    }
                })
            })
            .collect();
        body["tools"] = Value::Array(tools);
    }
    body
}

fn wire_message(message: &Message) -> Value {
    let mut text = Vec::new();
    let mut tool_calls = Vec::new();
    let mut tool_call_id = None;

    for block in &message.content {
        match block {
            ContentBlock::Text { text: value } => text.push(value.clone()),
            ContentBlock::ToolCall {
                id,
                name,
                arguments,
            } => {
                tool_calls.push(json!({
                    "id": id,
                    "type": "function",
                    "function": {
                        "name": name,
                        "arguments": arguments.to_string(),
                    }
                }));
            }
            ContentBlock::ToolResult {
                tool_call_id: id,
                content,
                ..
            } => {
                tool_call_id = Some(id.clone());
                text.push(content.clone());
            }
        }
    }

    let role = match message.role {
        Role::System => "system",
        Role::User => "user",
        Role::Assistant => "assistant",
        Role::Tool => "tool",
    };
    let content = text.join("\n");
    let mut wire = json!({"role": role});
    if !content.is_empty() || tool_calls.is_empty() {
        wire["content"] = Value::String(content);
    } else {
        wire["content"] = Value::Null;
    }
    if !tool_calls.is_empty() {
        wire["tool_calls"] = Value::Array(tool_calls);
    }
    if let Some(id) = tool_call_id {
        wire["tool_call_id"] = Value::String(id);
    }
    wire
}

fn decode_sse(response: reqwest::Response, provider: String) -> ModelStream {
    Box::pin(async_stream::stream! {
        let mut bytes = response.bytes_stream();
        let mut buffer: Vec<u8> = Vec::with_capacity(8 * 1024);
        let mut data_lines: Vec<String> = Vec::new();
        let mut finished = false;

        'outer: while let Some(chunk) = bytes.next().await {
            let chunk = match chunk {
                Ok(chunk) => chunk,
                Err(err) => {
                    yield Err(ModelError::Transport(err));
                    return;
                }
            };
            buffer.extend_from_slice(&chunk);
            // An untrusted server can send an endlessly growing SSE line.
            if buffer.len() > 2 * 1024 * 1024 {
                yield Err(ModelError::Decode("SSE event exceeded 2 MiB".to_owned()));
                return;
            }

            while let Some(newline) = buffer.iter().position(|b| *b == b'\n') {
                let raw: Vec<u8> = buffer.drain(..=newline).collect();
                let line = String::from_utf8_lossy(&raw);
                let line = line.trim_end_matches(['\r', '\n']);

                if line.is_empty() {
                    if data_lines.is_empty() {
                        continue;
                    }
                    let data = data_lines.join("\n");
                    data_lines.clear();
                    match dispatch_event(&data, &provider) {
                        Ok(events) => {
                            for event in events {
                                yield Ok(event);
                            }
                        }
                        Err(err) => yield Err(err),
                    }
                    if data.trim() == "[DONE]" {
                        finished = true;
                        break 'outer;
                    }
                } else if let Some(rest) = line.strip_prefix("data:") {
                    data_lines.push(rest.strip_prefix(' ').unwrap_or(rest).to_owned());
                }
                // `event:`, `id:`, `retry:` and comment lines are not needed:
                // the OpenAI protocol puts the type inside the JSON payload.
            }
        }

        if !finished && !data_lines.is_empty() {
            let data = data_lines.join("\n");
            if data.trim() != "[DONE]" {
                match dispatch_event(&data, &provider) {
                    Ok(events) => {
                        for event in events {
                            yield Ok(event);
                        }
                    }
                    Err(err) => yield Err(err),
                }
            }
        }
    })
}

fn dispatch_event(data: &str, provider: &str) -> Result<Vec<StreamEvent>, ModelError> {
    let value: Value = serde_json::from_str(data)
        .map_err(|err| ModelError::Decode(format!("invalid SSE JSON: {err}")))?;

    if let Some(error) = value.get("error") {
        let message = error
            .get("message")
            .and_then(Value::as_str)
            .unwrap_or("unknown provider error");
        return Err(ModelError::Provider {
            provider: provider.to_owned(),
            message: message.to_owned(),
        });
    }

    let mut events = Vec::new();
    let mut finished = false;

    if let Some(choices) = value.get("choices").and_then(Value::as_array) {
        for choice in choices {
            let delta = choice.get("delta").unwrap_or(&Value::Null);

            if let Some(content) = delta.get("content").and_then(Value::as_str) {
                if !content.is_empty() {
                    events.push(StreamEvent::TextDelta {
                        text: content.to_owned(),
                    });
                }
            }

            if let Some(calls) = delta.get("tool_calls").and_then(Value::as_array) {
                for call in calls {
                    let index = call.get("index").and_then(Value::as_u64).unwrap_or(0) as u32;
                    let id = call.get("id").and_then(Value::as_str).map(str::to_owned);
                    let function = call.get("function").unwrap_or(&Value::Null);
                    let name = function
                        .get("name")
                        .and_then(Value::as_str)
                        .map(str::to_owned);
                    let arguments = function
                        .get("arguments")
                        .and_then(Value::as_str)
                        .unwrap_or("")
                        .to_owned();
                    events.push(StreamEvent::ToolCallDelta {
                        index,
                        id,
                        name,
                        arguments_delta: arguments,
                    });
                }
            }

            if let Some(reason) = choice.get("finish_reason").and_then(Value::as_str) {
                finished = true;
                events.push(StreamEvent::Finished {
                    stop_reason: map_stop_reason(reason),
                });
            }
        }
    }

    if let Some(usage) = value.get("usage").filter(|v| !v.is_null()) {
        let prompt = usage
            .get("prompt_tokens")
            .and_then(Value::as_u64)
            .unwrap_or(0);
        let completion = usage
            .get("completion_tokens")
            .and_then(Value::as_u64)
            .unwrap_or(0);
        let cached = usage
            .get("prompt_tokens_details")
            .and_then(|details| details.get("cached_tokens"))
            .and_then(Value::as_u64)
            .unwrap_or(0);
        events.push(StreamEvent::Usage(Usage {
            input_tokens: prompt,
            output_tokens: completion,
            cached_input_tokens: cached,
        }));
    }

    if !finished && events.is_empty() && value.get("choices").is_some() {
        // Role-only preamble or keep-alive payload; nothing to surface.
    }

    Ok(events)
}

fn map_stop_reason(reason: &str) -> StopReason {
    match reason {
        "stop" => StopReason::EndTurn,
        "length" => StopReason::MaxTokens,
        "tool_calls" | "function_call" => StopReason::ToolUse,
        "content_filter" => StopReason::Other("content_filter".to_owned()),
        other => StopReason::Other(other.to_owned()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::ToolDefinition;

    #[test]
    fn maps_normalized_messages_to_openai_wire_format() {
        let request = ChatRequest::new("gpt-4.1")
            .with_system("You are Ryoko.")
            .with_message(Message::user("Read the file"))
            .with_message(Message {
                role: Role::Assistant,
                content: vec![
                    ContentBlock::Text {
                        text: "On it.".to_owned(),
                    },
                    ContentBlock::ToolCall {
                        id: "call_9".to_owned(),
                        name: "read_file".to_owned(),
                        arguments: json!({"path": "src/lib.rs"}),
                    },
                ],
            })
            .with_message(Message::tool_result("call_9", "fn main() {}"))
            .with_tools(vec![ToolDefinition::new(
                "read_file",
                "Read a file",
                json!({"type": "object"}),
            )])
            .with_max_tokens(256);

        let body = wire_body(&request, true);
        assert_eq!(body["model"], "gpt-4.1");
        assert_eq!(body["stream"], true);
        assert_eq!(body["stream_options"]["include_usage"], true);
        assert_eq!(body["messages"][0]["role"], "system");
        assert_eq!(
            body["messages"][2]["tool_calls"][0]["function"]["name"],
            "read_file"
        );
        assert_eq!(body["messages"][3]["role"], "tool");
        assert_eq!(body["messages"][3]["tool_call_id"], "call_9");
        assert_eq!(body["tools"][0]["function"]["name"], "read_file");
        assert_eq!(body["max_tokens"], 256);
    }

    #[test]
    fn decodes_text_usage_and_finish_reason() {
        let text = dispatch_event(
            r#"{"choices":[{"delta":{"content":"Hello"},"finish_reason":null}]}"#,
            "openai",
        )
        .unwrap();
        assert!(matches!(&text[0], StreamEvent::TextDelta { text } if text == "Hello"));

        let usage = dispatch_event(
            r#"{"choices":[],"usage":{"prompt_tokens":10,"completion_tokens":4,"prompt_tokens_details":{"cached_tokens":2}}}"#,
            "openai",
        )
        .unwrap();
        assert!(matches!(
            &usage[0],
            StreamEvent::Usage(Usage {
                input_tokens: 10,
                output_tokens: 4,
                cached_input_tokens: 2
            })
        ));

        let done = dispatch_event(
            r#"{"choices":[{"delta":{},"finish_reason":"tool_calls"}]}"#,
            "openai",
        )
        .unwrap();
        assert!(matches!(
            &done[0],
            StreamEvent::Finished {
                stop_reason: StopReason::ToolUse
            }
        ));
    }

    #[test]
    fn decodes_tool_call_deltas() {
        let events = dispatch_event(
            r#"{"choices":[{"delta":{"tool_calls":[{"index":0,"id":"call_1","function":{"name":"read_file","arguments":"{\"path\":"}}]}}]}"#,
            "openai",
        )
        .unwrap();
        assert!(matches!(
            &events[0],
            StreamEvent::ToolCallDelta {
                index: 0,
                id: Some(id),
                name: Some(name),
                arguments_delta
            } if id == "call_1" && name == "read_file" && arguments_delta == r#"{"path":"#
        ));
    }

    #[test]
    fn surfaces_provider_error_payloads() {
        let err = dispatch_event(
            r#"{"error":{"message":"rate limited","type":"rate_limit"}}"#,
            "openai",
        )
        .unwrap_err();
        assert!(matches!(err, ModelError::Provider { .. }));
    }
}
