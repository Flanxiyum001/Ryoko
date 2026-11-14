//! Provider-agnostic model integration layer.
//!
//! Every backend (OpenAI-compatible HTTP endpoints, local runtimes, scripted
//! mocks) is normalised into the same [`ChatRequest`] / [`StreamEvent`] pair.
//! Agents never see vendor-specific payloads, and all token expenditure flows
//! through a single [`UsageLedger`].

mod anthropic;
mod config;
mod ledger;
mod mock;
mod openai;
mod router;
mod types;

pub use anthropic::{AnthropicConfig, AnthropicProvider, anthropic_wire_body};
pub use config::{ProviderCatalog, ProviderProfile, WireProtocol};
pub use ledger::{Budget, LedgerSnapshot, UsageLedger};
pub use mock::{MockProvider, ScriptedTurn};
pub use openai::{OpenAiCompatConfig, OpenAiCompatProvider};
pub use router::{Completion, ModelRouter, ProviderEntry};
pub use types::{
    ChatRequest, ContentBlock, Message, ModelError, ModelProvider, ModelStream, Pricing,
    ProviderCapabilities, Role, StreamEvent, ToolCall, ToolDefinition, Usage,
};
