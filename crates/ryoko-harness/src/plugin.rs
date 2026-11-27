//! DeepSeek-inspired "Everything is a Plugin" architecture.
//!
//! Provides a composable plugin framework modeled on DeepSeek Harness's
//! plugin lifecycle hooks, allowing behavior (guards, tracing, diff review,
//! metrics) to attach to agent turns and tool calls without tightly coupling
//! to the core loop.

use std::path::PathBuf;
use std::sync::Arc;

use ryoko_models::{ChatRequest, Completion, ToolCall};

use crate::harness::HarnessOutcome;

/// Decision returned by [`HarnessPlugin::pre_model_request`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StepDecision {
    /// Proceed with normal model completion.
    Continue,
    /// Stop the agent loop early with a reason.
    Halt(String),
    /// Inject a reminder or guidance message into the conversation context.
    InjectNotice(String),
}

/// Decision returned by [`HarnessPlugin::pre_tool_call`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ToolDecision {
    /// Proceed with executing the tool.
    Allow,
    /// Refuse tool execution with an error message returned to the model.
    Deny(String),
    /// Bypass tool execution and return an immediate synthetic result.
    Override(String),
}

/// Metadata about the active harness run.
#[derive(Debug, Clone)]
pub struct SessionMeta {
    pub session_id: String,
    pub goal: String,
    pub root: PathBuf,
    pub model: String,
}

/// Runtime context passed into plugin lifecycle callbacks.
#[derive(Debug, Clone)]
pub struct PluginContext {
    pub session_id: String,
    pub root: PathBuf,
    pub model: String,
}

/// Lifecycle hook contract for Ryoko Harness plugins.
pub trait HarnessPlugin: Send + Sync {
    /// Unique identifier of the plugin.
    fn name(&self) -> &str;

    /// Short human-readable summary of what this plugin provides.
    fn description(&self) -> &str {
        ""
    }

    /// Invoked once when the agent session starts.
    fn on_session_start(&self, _ctx: &PluginContext, _meta: &SessionMeta) {}

    /// Invoked before each model turn begins.
    fn on_turn_start(&self, _ctx: &PluginContext, _turn: usize) {}

    /// Inspect or modify the model request before it is sent to the router.
    fn pre_model_request(
        &self,
        _ctx: &PluginContext,
        _turn: usize,
        _request: &mut ChatRequest,
    ) -> Result<StepDecision, String> {
        Ok(StepDecision::Continue)
    }

    /// Invoked when the model returns an assistant message and tool requests.
    fn on_model_response(&self, _ctx: &PluginContext, _turn: usize, _completion: &Completion) {}

    /// Intercept or validate a proposed tool call before it executes.
    fn pre_tool_call(
        &self,
        _ctx: &PluginContext,
        _call: &ToolCall,
    ) -> Result<ToolDecision, String> {
        Ok(ToolDecision::Allow)
    }

    /// Observe the outcome of a tool call after execution.
    fn post_tool_call(
        &self,
        _ctx: &PluginContext,
        _call: &ToolCall,
        _result: &Result<String, String>,
    ) {
    }

    /// Invoked at the end of each model turn.
    fn on_turn_end(&self, _ctx: &PluginContext, _turn: usize) {}

    /// Invoked when the entire harness run completes or terminates.
    fn on_session_end(&self, _ctx: &PluginContext, _outcome: &HarnessOutcome) {}
}

/// Registry of mounted plugins executed in order.
#[derive(Default, Clone)]
pub struct PluginRegistry {
    plugins: Vec<Arc<dyn HarnessPlugin>>,
}

impl PluginRegistry {
    pub fn new() -> Self {
        Self {
            plugins: Vec::new(),
        }
    }

    /// Mounts a plugin into the registry.
    pub fn register(&mut self, plugin: Arc<dyn HarnessPlugin>) {
        self.plugins.push(plugin);
    }

    pub fn plugins(&self) -> &[Arc<dyn HarnessPlugin>] {
        &self.plugins
    }

    pub fn dispatch_session_start(&self, ctx: &PluginContext, meta: &SessionMeta) {
        for plugin in &self.plugins {
            plugin.on_session_start(ctx, meta);
        }
    }

    pub fn dispatch_turn_start(&self, ctx: &PluginContext, turn: usize) {
        for plugin in &self.plugins {
            plugin.on_turn_start(ctx, turn);
        }
    }

    pub fn dispatch_pre_model_request(
        &self,
        ctx: &PluginContext,
        turn: usize,
        request: &mut ChatRequest,
    ) -> Result<StepDecision, String> {
        for plugin in &self.plugins {
            match plugin.pre_model_request(ctx, turn, request)? {
                StepDecision::Continue => continue,
                other => return Ok(other),
            }
        }
        Ok(StepDecision::Continue)
    }

    pub fn dispatch_model_response(
        &self,
        ctx: &PluginContext,
        turn: usize,
        completion: &Completion,
    ) {
        for plugin in &self.plugins {
            plugin.on_model_response(ctx, turn, completion);
        }
    }

    pub fn dispatch_pre_tool_call(
        &self,
        ctx: &PluginContext,
        call: &ToolCall,
    ) -> Result<ToolDecision, String> {
        for plugin in &self.plugins {
            match plugin.pre_tool_call(ctx, call)? {
                ToolDecision::Allow => continue,
                decision => return Ok(decision),
            }
        }
        Ok(ToolDecision::Allow)
    }

    pub fn dispatch_post_tool_call(
        &self,
        ctx: &PluginContext,
        call: &ToolCall,
        result: &Result<String, String>,
    ) {
        for plugin in &self.plugins {
            plugin.post_tool_call(ctx, call, result);
        }
    }

    pub fn dispatch_turn_end(&self, ctx: &PluginContext, turn: usize) {
        for plugin in &self.plugins {
            plugin.on_turn_end(ctx, turn);
        }
    }

    pub fn dispatch_session_end(&self, ctx: &PluginContext, outcome: &HarnessOutcome) {
        for plugin in &self.plugins {
            plugin.on_session_end(ctx, outcome);
        }
    }
}
