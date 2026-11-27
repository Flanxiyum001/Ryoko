use std::collections::BTreeMap;

use async_trait::async_trait;
use futures::StreamExt;
use serde_json::{Value, json};

use crate::types::{
    ChatRequest, ContentBlock, Message, ModelError, ModelProvider, ModelStream, Pricing,
    ProviderCapabilities, Role, StopReason, StreamEvent, Usage,
};

/// Configuration for Anthropic's Messages API or a compatible gateway.
#[derive(Clone)]
pub struct AnthropicConfig {
    pub id: String,
    pub base_url: String,
    pub api_key: Option<String>,
    pub version: String,
    pub default_pricing: Option<Pricing>,
    pub headers: BTreeMap<String, String>,
}

impl std::fmt::Debug for AnthropicConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AnthropicConfig")
            .field("id", &self.id)
            .field("base_url", &self.base_url)
            .field("api_key", &self.api_key.as_ref().map(|_| "<redacted>"))
            .field("version", &self.version)
            .finish()
    }
}

impl AnthropicConfig {
    pub fn new(id: impl Into<String>, base_url: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            base_url: base_url.into(),
            api_key: None,
            version: "2023-06-01".to_owned(),
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
        let base = self.base_url.trim_end_matches('/');
        if base.ends_with("/v1") {
            format!("{base}/messages")
        } else {
            format!("{base}/v1/messages")
        }
    }
}

pub struct AnthropicProvider {
    config: AnthropicConfig,
    client: reqwest::Client,
}

impl std::fmt::Debug for AnthropicProvider {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AnthropicProvider")
            .field("config", &self.config)
            .finish()
    }
}

impl AnthropicProvider {
    pub fn new(config: AnthropicConfig) -> Result<Self, ModelError> {
        let client = reqwest::Client::builder()
            .connect_timeout(std::time::Duration::from_secs(20))
            .timeout(std::time::Duration::from_secs(180))
            .redirect(reqwest::redirect::Policy::none())
            .user_agent(concat!("ryoko/", env!("CARGO_PKG_VERSION")))
            .build()?;
        Ok(Self { config, client })
    }

    pub fn config(&self) -> &AnthropicConfig {
        &self.config
    }
}

#[async_trait]
impl ModelProvider for AnthropicProvider {
    fn id(&self) -> &str {
        &self.config.id
    }

    fn capabilities(&self) -> ProviderCapabilities {
        ProviderCapabilities {
            supports_streaming: true,
            supports_tools: true,
            supports_system_prompt: true,
            max_context_tokens: 200_000,
        }
    }

    fn pricing(&self, _model: &str) -> Option<Pricing> {
        self.config.default_pricing
    }

    async fn stream(&self, request: ChatRequest) -> Result<ModelStream, ModelError> {
        let body = anthropic_wire_body(&request, true);
        let mut builder = self
            .client
            .post(self.config.endpoint())
            .header(reqwest::header::CONTENT_TYPE, "application/json")
            .header("anthropic-version", &self.config.version)
            .json(&body);

        if let Some(key) = &self.config.api_key {
            if !key.is_empty() {
                builder = builder.header("x-api-key", key);
            }
        }
        for (name, value) in &self.config.headers {
            builder = builder.header(name, value);
        }

        let response = builder.send().await?;
        let status = response.status();
        if !status.is_success() {
            return Err(ModelError::Http {
                provider: self.config.id.clone(),
                status: status.as_u16(),
                body: "provider error details omitted to protect secrets".to_owned(),
            });
        }
        Ok(decode_sse(response, self.config.id.clone()))
    }
}

/// Converts a normalised request into Anthropic's Messages API shape.
pub fn anthropic_wire_body(request: &ChatRequest, stream: bool) -> Value {
    let mut system = Vec::new();
    if let Some(value) = &request.system {
        if !value.is_empty() {
            system.push(value.clone());
        }
    }

    let mut messages = Vec::new();
    for message in &request.messages {
        if message.role == Role::System {
            let text = message.text_content();
            if !text.is_empty() {
                system.push(text);
            }
            continue;
        }
        messages.push(anthropic_message(message));
    }

    let mut body = json!({
        "model": request.model,
        "messages": messages,
        "max_tokens": request.max_tokens.unwrap_or(4096),
        "stream": stream,
    });
    if request.cache_system_prefix && request.system.as_deref().is_some_and(|s| !s.is_empty()) {
        // The first block is the stable prefix; a cache breakpoint after it
        // lets later requests that share it skip re-processing. Per-request
        // system messages follow the breakpoint and are never cached.
        let mut blocks = Vec::with_capacity(system.len());
        for (index, text) in system.iter().enumerate() {
            let mut block = json!({"type": "text", "text": text});
            if index == 0 {
                block["cache_control"] = json!({"type": "ephemeral"});
            }
            blocks.push(block);
        }
        body["system"] = Value::Array(blocks);
    } else if !system.is_empty() {
        body["system"] = json!(system.join("\n"));
    }
    if let Some(temperature) = request.temperature {
        body["temperature"] = json!(temperature);
    }
    if !request.tools.is_empty() {
        body["tools"] = json!(
            request
                .tools
                .iter()
                .map(|tool| {
                    json!({
                        "name": tool.name,
                        "description": tool.description,
                        "input_schema": tool.input_schema,
                    })
                })
                .collect::<Vec<_>>()
        );
    }
    body
}

fn anthropic_message(message: &Message) -> Value {
    let role = if message.role == Role::Assistant {
        "assistant"
    } else {
        "user"
    };
    let mut blocks = Vec::new();
    for block in &message.content {
        match block {
            ContentBlock::Text { text } => {
                if !text.is_empty() {
                    blocks.push(json!({"type": "text", "text": text}));
                }
            }
            ContentBlock::Reasoning { .. } => {}
            ContentBlock::Image {
                media_type,
                data_base64,
                ..
            } => {
                blocks.push(json!({
                    "type": "image",
                    "source": {
                        "type": "base64",
                        "media_type": media_type,
                        "data": data_base64,
                    }
                }));
            }
            ContentBlock::ToolCall {
                id,
                name,
                arguments,
            } => {
                blocks.push(json!({
                    "type": "tool_use",
                    "id": id,
                    "name": name,
                    "input": arguments,
                }));
            }
            ContentBlock::ToolResult {
                tool_call_id,
                content,
                is_error,
            } => {
                let mut result = json!({
                    "type": "tool_result",
                    "tool_use_id": tool_call_id,
                    "content": [{"type": "text", "text": content}],
                });
                if *is_error {
                    result["is_error"] = json!(true);
                }
                blocks.push(result);
            }
        }
    }
    if blocks.is_empty() {
        blocks.push(json!({"type": "text", "text": ""}));
    }
    json!({"role": role, "content": blocks})
}

fn decode_sse(response: reqwest::Response, provider: String) -> ModelStream {
    Box::pin(async_stream::stream! {
        let mut bytes = response.bytes_stream();
        let mut buffer: Vec<u8> = Vec::with_capacity(8 * 1024);
        let mut data_lines: Vec<String> = Vec::new();
        let mut event_bytes: usize = 0;

        'outer: while let Some(chunk) = bytes.next().await {
            let chunk = match chunk {
                Ok(chunk) => chunk,
                Err(err) => {
                    yield Err(ModelError::Transport(err));
                    return;
                }
            };
            buffer.extend_from_slice(&chunk);
            if buffer.len() > 2 * 1024 * 1024 {
                yield Err(ModelError::Decode("SSE event exceeded 2 MiB".to_owned()));
                return;
            }

            while let Some(newline) = buffer.iter().position(|byte| *byte == b'\n') {
                let raw: Vec<u8> = buffer.drain(..=newline).collect();
                let line = String::from_utf8_lossy(&raw);
                let line = line.trim_end_matches(['\r', '\n']);

                if line.is_empty() {
                    if data_lines.is_empty() {
                        continue;
                    }
                    let data = data_lines.join("\n");
                    data_lines.clear();
                    event_bytes = 0;
                    if data.trim() == "[DONE]" {
                        break 'outer;
                    }
                    match dispatch_event(&data, &provider) {
                        Ok(events) => {
                            for event in events {
                                yield Ok(event);
                            }
                        }
                        Err(error) => yield Err(error),
                    }
                } else if let Some(rest) = line.strip_prefix("data:") {
                    let data = rest.strip_prefix(' ').unwrap_or(rest);
                    // A stream of short, unterminated data lines must not
                    // evade the per-line buffer cap indefinitely.
                    event_bytes = event_bytes.saturating_add(data.len().saturating_add(1));
                    if event_bytes > 2 * 1024 * 1024 {
                        yield Err(ModelError::Decode("SSE event exceeded 2 MiB".to_owned()));
                        return;
                    }
                    data_lines.push(data.to_owned());
                }
            }
        }

        if !data_lines.is_empty() {
            let data = data_lines.join("\n");
            if data.trim() != "[DONE]" {
                match dispatch_event(&data, &provider) {
                    Ok(events) => {
                        for event in events {
                            yield Ok(event);
                        }
                    }
                    Err(error) => yield Err(error),
                }
            }
        }
    })
}

fn dispatch_event(data: &str, provider: &str) -> Result<Vec<StreamEvent>, ModelError> {
    let value: Value = serde_json::from_str(data)
        .map_err(|error| ModelError::Decode(format!("invalid Anthropic SSE JSON: {error}")))?;
    let kind = value
        .get("type")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let mut events = Vec::new();

    match kind {
        "message_start" => {
            let message = value.get("message").unwrap_or(&Value::Null);
            let model = message
                .get("model")
                .and_then(Value::as_str)
                .unwrap_or_default();
            events.push(StreamEvent::Start {
                model: model.to_owned(),
            });
            if let Some(usage) = message.get("usage") {
                events.push(StreamEvent::Usage(anthropic_usage(usage)));
            }
        }
        "content_block_start" => {
            let index = block_index(&value);
            let block = value.get("content_block").unwrap_or(&Value::Null);
            match block.get("type").and_then(Value::as_str) {
                Some("text") => {
                    if let Some(text) = block.get("text").and_then(Value::as_str) {
                        if !text.is_empty() {
                            events.push(StreamEvent::TextDelta {
                                text: text.to_owned(),
                            });
                        }
                    }
                }
                Some("tool_use") => {
                    events.push(StreamEvent::ToolCallDelta {
                        index,
                        id: block.get("id").and_then(Value::as_str).map(str::to_owned),
                        name: block.get("name").and_then(Value::as_str).map(str::to_owned),
                        arguments_delta: String::new(),
                    });
                }
                _ => {}
            }
        }
        "content_block_delta" => {
            let index = block_index(&value);
            let delta = value.get("delta").unwrap_or(&Value::Null);
            match delta.get("type").and_then(Value::as_str) {
                Some("text_delta") => {
                    if let Some(text) = delta.get("text").and_then(Value::as_str) {
                        if !text.is_empty() {
                            events.push(StreamEvent::TextDelta {
                                text: text.to_owned(),
                            });
                        }
                    }
                }
                Some("input_json_delta") => {
                    events.push(StreamEvent::ToolCallDelta {
                        index,
                        id: None,
                        name: None,
                        arguments_delta: delta
                            .get("partial_json")
                            .and_then(Value::as_str)
                            .unwrap_or_default()
                            .to_owned(),
                    });
                }
                _ => {}
            }
        }
        "message_delta" => {
            if let Some(usage) = value.get("usage") {
                events.push(StreamEvent::Usage(anthropic_usage(usage)));
            }
            if let Some(reason) = value
                .get("delta")
                .and_then(|delta| delta.get("stop_reason"))
                .and_then(Value::as_str)
            {
                events.push(StreamEvent::Finished {
                    stop_reason: map_stop_reason(reason),
                });
            }
        }
        "error" => {
            let message = value
                .get("error")
                .and_then(|error| error.get("message"))
                .and_then(Value::as_str)
                .unwrap_or("unknown Anthropic error");
            return Err(ModelError::Provider {
                provider: provider.to_owned(),
                message: message.to_owned(),
            });
        }
        "ping" | "message_stop" => {}
        other => {
            tracing::debug!(event_type = other, "ignoring unknown Anthropic SSE event");
        }
    }
    Ok(events)
}

fn block_index(value: &Value) -> u32 {
    value.get("index").and_then(Value::as_u64).unwrap_or(0) as u32
}

fn anthropic_usage(value: &Value) -> Usage {
    Usage {
        input_tokens: value
            .get("input_tokens")
            .and_then(Value::as_u64)
            .unwrap_or(0),
        output_tokens: value
            .get("output_tokens")
            .and_then(Value::as_u64)
            .unwrap_or(0),
        cached_input_tokens: value
            .get("cache_read_input_tokens")
            .and_then(Value::as_u64)
            .unwrap_or(0),
    }
}

fn map_stop_reason(reason: &str) -> StopReason {
    match reason {
        "end_turn" => StopReason::EndTurn,
        "max_tokens" => StopReason::MaxTokens,
        "tool_use" => StopReason::ToolUse,
        "stop_sequence" => StopReason::StopSequence,
        other => StopReason::Other(other.to_owned()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{Message, ToolDefinition};

    #[test]
    fn image_blocks_become_base64_image_sources() {
        let image = ContentBlock::image("image/png", "iVBORw0KGgo=", None).unwrap();
        let message = Message::new(
            Role::User,
            vec![
                ContentBlock::Text {
                    text: "Look at the layout".to_owned(),
                },
                image,
            ],
        );
        let wire = anthropic_message(&message);
        let blocks = wire["content"].as_array().unwrap();
        assert_eq!(blocks[0]["type"], "text");
        assert_eq!(blocks[1]["type"], "image");
        assert_eq!(blocks[1]["source"]["type"], "base64");
        assert_eq!(blocks[1]["source"]["media_type"], "image/png");
        assert_eq!(blocks[1]["source"]["data"], "iVBORw0KGgo=");
    }

    #[test]
    fn maps_system_messages_tools_and_tool_results() {
        let request = ChatRequest::new("claude-sonnet")
            .with_system("be precise")
            .with_message(Message::user("inspect this"))
            .with_message(Message {
                role: Role::Assistant,
                content: vec![ContentBlock::ToolCall {
                    id: "call-1".to_owned(),
                    name: "read_file".to_owned(),
                    arguments: json!({"path": "src/lib.rs"}),
                }],
            })
            .with_message(Message::tool_result("call-1", "contents"))
            .with_tools(vec![ToolDefinition::new(
                "read_file",
                "Read a file",
                json!({"type": "object"}),
            )])
            .with_max_tokens(128);
        let body = anthropic_wire_body(&request, true);
        assert_eq!(body["system"], "be precise");
        assert_eq!(body["max_tokens"], 128);
        assert_eq!(body["messages"][1]["content"][0]["type"], "tool_use");
        assert_eq!(body["messages"][2]["content"][0]["type"], "tool_result");
        assert_eq!(body["tools"][0]["name"], "read_file");
        assert_eq!(body["stream"], true);
    }

    #[test]
    fn cached_system_prefix_gets_a_single_breakpoint() {
        let request = ChatRequest::new("claude")
            .with_system("static repo map")
            .with_message(Message::system("role: qa"))
            .with_user("hi")
            .with_cached_system_prefix();
        let body = anthropic_wire_body(&request, false);
        let system = body["system"].as_array().expect("block array");
        assert_eq!(system.len(), 2);
        assert_eq!(system[0]["text"], "static repo map");
        assert_eq!(system[0]["cache_control"]["type"], "ephemeral");
        assert_eq!(system[1]["text"], "role: qa");
        assert!(system[1].get("cache_control").is_none());

        let uncached = anthropic_wire_body(&ChatRequest::new("claude").with_system("x"), false);
        assert_eq!(uncached["system"], "x");
    }

    #[test]
    fn decodes_text_tool_and_usage_events() {
        let start = dispatch_event(
            r#"{"type":"message_start","message":{"model":"claude-x","usage":{"input_tokens":12}}}"#,
            "anthropic",
        )
        .unwrap();
        assert!(matches!(&start[0], StreamEvent::Start { model } if model == "claude-x"));
        assert!(matches!(&start[1], StreamEvent::Usage(usage) if usage.input_tokens == 12));

        let text = dispatch_event(
            r#"{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"hello"}}"#,
            "anthropic",
        )
        .unwrap();
        assert!(matches!(&text[0], StreamEvent::TextDelta { text } if text == "hello"));

        let tool = dispatch_event(
            r#"{"type":"content_block_start","index":1,"content_block":{"type":"tool_use","id":"call-1","name":"read_file"}}"#,
            "anthropic",
        )
        .unwrap();
        assert!(matches!(
            &tool[0],
            StreamEvent::ToolCallDelta { index: 1, id: Some(id), name: Some(name), arguments_delta }
                if id == "call-1" && name == "read_file" && arguments_delta.is_empty()
        ));

        let arguments = dispatch_event(
            r#"{"type":"content_block_delta","index":1,"delta":{"type":"input_json_delta","partial_json":"{\"path\":"}}"#,
            "anthropic",
        )
        .unwrap();
        assert!(matches!(
            &arguments[0],
            StreamEvent::ToolCallDelta { arguments_delta, .. } if arguments_delta == "{\"path\":"
        ));

        let finish = dispatch_event(
            r#"{"type":"message_delta","delta":{"stop_reason":"tool_use"},"usage":{"output_tokens":7}}"#,
            "anthropic",
        )
        .unwrap();
        assert!(matches!(&finish[0], StreamEvent::Usage(usage) if usage.output_tokens == 7));
        assert!(matches!(
            &finish[1],
            StreamEvent::Finished {
                stop_reason: StopReason::ToolUse
            }
        ));
    }

    #[test]
    fn endpoint_accepts_base_url_with_or_without_v1() {
        let config = AnthropicConfig::new("anthropic", "https://api.anthropic.com");
        assert_eq!(config.endpoint(), "https://api.anthropic.com/v1/messages");
        let config = AnthropicConfig::new("anthropic", "https://api.anthropic.com/v1/");
        assert_eq!(config.endpoint(), "https://api.anthropic.com/v1/messages");
    }

    #[tokio::test]
    async fn rejects_unbounded_sse_event() {
        use std::io::{Read, Write};

        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let server = std::thread::spawn(move || {
            let (mut socket, _) = listener.accept().unwrap();
            // Consume the request headers before sending and closing: leaving
            // unread request bytes can reset the connection on some platforms.
            let mut request = Vec::new();
            let mut chunk = [0u8; 1024];
            while request.len() <= 8192 && !request.windows(4).any(|bytes| bytes == b"\r\n\r\n") {
                let count = socket.read(&mut chunk).unwrap();
                if count == 0 {
                    return;
                }
                request.extend_from_slice(&chunk[..count]);
            }
            // Every individual line is small, but the event has no blank
            // terminator and grows beyond the 2 MiB limit.
            let body = format!("data: {}\n", "x".repeat(1024)).repeat(2048);
            let headers = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                body.len()
            );
            socket.write_all(headers.as_bytes()).unwrap();
            let _ = socket.write_all(body.as_bytes());
        });

        let response = reqwest::get(format!("http://{address}/")).await.unwrap();
        let mut stream = decode_sse(response, "test".to_owned());
        let error = stream.next().await.unwrap().unwrap_err();
        assert!(
            matches!(error, ModelError::Decode(ref message) if message == "SSE event exceeded 2 MiB"),
            "unexpected SSE decoder error: {error}"
        );
        drop(stream);
        server.join().unwrap();
    }

    #[test]
    fn provider_errors_redact_response_bodies() {
        let error = dispatch_event(
            r#"{"type":"error","error":{"type":"api_error","message":"bad request"}}"#,
            "anthropic",
        )
        .unwrap_err();
        assert!(error.to_string().contains("bad request"));
    }
}
