use std::collections::BTreeMap;
use std::fmt;
use std::ops::{Add, AddAssign};
use std::pin::Pin;

use async_trait::async_trait;
use futures::Stream;
use serde::{Deserialize, Serialize};

/// Conversation role in a normalised request.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    System,
    User,
    Assistant,
    Tool,
}

impl fmt::Display for Role {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let name = match self {
            Self::System => "system",
            Self::User => "user",
            Self::Assistant => "assistant",
            Self::Tool => "tool",
        };
        f.write_str(name)
    }
}

/// A normalised content block. Vendors wrap text, tool calls and tool results
/// differently; the adapters flatten those differences into this enum.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ContentBlock {
    Text {
        text: String,
    },
    ToolCall {
        id: String,
        name: String,
        arguments: serde_json::Value,
    },
    ToolResult {
        tool_call_id: String,
        content: String,
        #[serde(default)]
        is_error: bool,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Message {
    pub role: Role,
    pub content: Vec<ContentBlock>,
}

impl Message {
    pub fn new(role: Role, content: Vec<ContentBlock>) -> Self {
        Self { role, content }
    }

    pub fn text(role: Role, text: impl Into<String>) -> Self {
        Self {
            role,
            content: vec![ContentBlock::Text { text: text.into() }],
        }
    }

    pub fn system(text: impl Into<String>) -> Self {
        Self::text(Role::System, text)
    }

    pub fn user(text: impl Into<String>) -> Self {
        Self::text(Role::User, text)
    }

    pub fn assistant(text: impl Into<String>) -> Self {
        Self::text(Role::Assistant, text)
    }

    pub fn tool_result(tool_call_id: impl Into<String>, content: impl Into<String>) -> Self {
        Self {
            role: Role::Tool,
            content: vec![ContentBlock::ToolResult {
                tool_call_id: tool_call_id.into(),
                content: content.into(),
                is_error: false,
            }],
        }
    }

    /// Flattens all text blocks; used for transcripts and token estimates.
    pub fn text_content(&self) -> String {
        let mut out = String::new();
        for block in &self.content {
            if let ContentBlock::Text { text } = block {
                if !out.is_empty() {
                    out.push('\n');
                }
                out.push_str(text);
            }
        }
        out
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolDefinition {
    pub name: String,
    pub description: String,
    pub input_schema: serde_json::Value,
}

impl ToolDefinition {
    pub fn new(
        name: impl Into<String>,
        description: impl Into<String>,
        input_schema: serde_json::Value,
    ) -> Self {
        Self {
            name: name.into(),
            description: description.into(),
            input_schema,
        }
    }
}

/// A provider-neutral completion request.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ChatRequest {
    pub model: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub system: Option<String>,
    #[serde(default)]
    pub messages: Vec<Message>,
    #[serde(default)]
    pub tools: Vec<ToolDefinition>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub temperature: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_tokens: Option<u32>,
    #[serde(default)]
    pub metadata: BTreeMap<String, String>,
}

impl ChatRequest {
    pub fn new(model: impl Into<String>) -> Self {
        Self {
            model: model.into(),
            system: None,
            messages: Vec::new(),
            tools: Vec::new(),
            temperature: None,
            max_tokens: None,
            metadata: BTreeMap::new(),
        }
    }

    pub fn with_system(mut self, system: impl Into<String>) -> Self {
        self.system = Some(system.into());
        self
    }

    pub fn with_message(mut self, message: Message) -> Self {
        self.messages.push(message);
        self
    }

    pub fn with_user(mut self, text: impl Into<String>) -> Self {
        self.messages.push(Message::user(text));
        self
    }

    pub fn with_tools(mut self, tools: Vec<ToolDefinition>) -> Self {
        self.tools = tools;
        self
    }

    pub fn with_max_tokens(mut self, max_tokens: u32) -> Self {
        self.max_tokens = Some(max_tokens);
        self
    }

    pub fn with_metadata(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.metadata.insert(key.into(), value.into());
        self
    }
}

/// Token expenditure reported by a provider.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Usage {
    #[serde(default)]
    pub input_tokens: u64,
    #[serde(default)]
    pub output_tokens: u64,
    #[serde(default)]
    pub cached_input_tokens: u64,
}

impl Usage {
    pub fn new(input_tokens: u64, output_tokens: u64) -> Self {
        Self {
            input_tokens,
            output_tokens,
            cached_input_tokens: 0,
        }
    }

    pub fn total_tokens(&self) -> u64 {
        self.input_tokens + self.output_tokens
    }
}

impl Add for Usage {
    type Output = Usage;

    fn add(self, rhs: Self) -> Self::Output {
        Usage {
            input_tokens: self.input_tokens + rhs.input_tokens,
            output_tokens: self.output_tokens + rhs.output_tokens,
            cached_input_tokens: self.cached_input_tokens + rhs.cached_input_tokens,
        }
    }
}

impl AddAssign for Usage {
    fn add_assign(&mut self, rhs: Self) {
        *self = *self + rhs;
    }
}

/// USD price per million tokens.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Pricing {
    pub input_per_mtok: f64,
    pub output_per_mtok: f64,
    #[serde(default)]
    pub cached_input_per_mtok: f64,
}

impl Pricing {
    pub const fn new(input_per_mtok: f64, output_per_mtok: f64) -> Self {
        Self {
            input_per_mtok,
            output_per_mtok,
            cached_input_per_mtok: input_per_mtok,
        }
    }

    pub const fn with_cached(mut self, cached_input_per_mtok: f64) -> Self {
        self.cached_input_per_mtok = cached_input_per_mtok;
        self
    }

    pub fn cost_usd(&self, usage: &Usage) -> f64 {
        let uncached_input = usage.input_tokens.saturating_sub(usage.cached_input_tokens);
        let input = uncached_input as f64 * self.input_per_mtok;
        let cached = usage.cached_input_tokens as f64 * self.cached_input_per_mtok;
        let output = usage.output_tokens as f64 * self.output_per_mtok;
        (input + cached + output) / 1_000_000.0
    }
}

impl Default for Pricing {
    fn default() -> Self {
        Self::new(0.0, 0.0)
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProviderCapabilities {
    pub supports_streaming: bool,
    pub supports_tools: bool,
    pub supports_system_prompt: bool,
    pub max_context_tokens: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StopReason {
    EndTurn,
    MaxTokens,
    ToolUse,
    StopSequence,
    Other(String),
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum StreamEvent {
    Start {
        model: String,
    },
    TextDelta {
        text: String,
    },
    ToolCallDelta {
        index: u32,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        id: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        name: Option<String>,
        arguments_delta: String,
    },
    Usage(Usage),
    Finished {
        stop_reason: StopReason,
    },
}

/// Fully aggregated completion produced from a stream.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ToolCall {
    pub id: String,
    pub name: String,
    pub arguments: serde_json::Value,
}

pub type ModelStream = Pin<Box<dyn Stream<Item = Result<StreamEvent, ModelError>> + Send>>;

#[derive(Debug, thiserror::Error)]
pub enum ModelError {
    #[error("invalid provider configuration: {0}")]
    Configuration(String),

    #[error("provider `{provider}` is not registered")]
    UnknownProvider { provider: String },

    #[error("model `{model}` could not be resolved; use `provider/model`")]
    UnknownModel { model: String },

    #[error("provider `{provider}` rejected the request (HTTP {status}): {body}")]
    Http {
        provider: String,
        status: u16,
        body: String,
    },

    #[error("transport error talking to model provider: {0}")]
    Transport(#[from] reqwest::Error),

    #[error("malformed provider response: {0}")]
    Decode(String),

    #[error("provider `{provider}` reported an error: {message}")]
    Provider { provider: String, message: String },

    #[error("token budget of ${budget_usd:.4} exhausted (spent ${spent_usd:.4})")]
    BudgetExceeded { budget_usd: f64, spent_usd: f64 },

    #[error("model request was cancelled")]
    Cancelled,

    #[error("model layer I/O error: {0}")]
    Io(String),
}

/// A single model backend.
#[async_trait]
pub trait ModelProvider: Send + Sync {
    /// Stable provider identifier, e.g. `openai`.
    fn id(&self) -> &str;

    fn capabilities(&self) -> ProviderCapabilities;

    /// Price list for a concrete model, when known.
    fn pricing(&self, _model: &str) -> Option<Pricing> {
        None
    }

    async fn stream(&self, request: ChatRequest) -> Result<ModelStream, ModelError>;
}
