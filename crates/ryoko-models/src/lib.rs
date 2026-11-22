//! Provider-agnostic model integration layer.
//!
//! Every backend (OpenAI-compatible HTTP endpoints, local runtimes, scripted
//! mocks) is normalised into the same [`ChatRequest`] / [`StreamEvent`] pair.
//! Agents never see vendor-specific payloads, and all token expenditure flows
//! through a single [`UsageLedger`].

mod anthropic;
mod config;
mod fim;
mod gemini;
mod ledger;
mod local;
mod mock;
mod openai;
mod router;
mod types;

pub use anthropic::{AnthropicConfig, AnthropicProvider, anthropic_wire_body};
pub use config::{ProviderCatalog, ProviderProfile, WireProtocol};
pub use fim::{FimClient, FimConfig};
pub use gemini::{GeminiConfig, GeminiProvider, gemini_wire_body, parse_gemini_response};
pub use ledger::{Budget, LedgerSnapshot, UsageLedger};
pub use local::{LocalModel, discover_local_models};
pub use mock::{MockProvider, ScriptedToolCall, ScriptedTurn};
pub use openai::{OpenAiCompatConfig, OpenAiCompatProvider};
pub use router::{Completion, ModelRouter, ProviderEntry};
pub use types::{
    ChatRequest, ContentBlock, Message, ModelError, ModelProvider, ModelStream, Pricing,
    ProviderCapabilities, Role, StreamEvent, ToolCall, ToolDefinition, Usage,
};
