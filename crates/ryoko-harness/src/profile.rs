//! DeepSeek-inspired Harness Profiles.
//!
//! DeepSeek Harness organizes capabilities into named profiles (`code`,
//! `standard`, `headless`, `minimal`). Each profile bundles specific toolsets,
//! execution budgets, prompt directives, and guardrail policies.

use serde::{Deserialize, Serialize};

use crate::harness::HarnessConfig;

/// Operating profile archetype for Ryoko's agent harness.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProfileKind {
    /// Specialized software engineering profile: staged edits, diff review,
    /// fast context indexing, plan tracking, loop guard.
    Code,
    /// Full interactive assistant: code editing, skills, persistent memory,
    /// context search, and optional approved command execution.
    Standard,
    /// Non-interactive automation / CI runner: strict turn limits, fast-fail
    /// on repeated errors, structured trace export.
    Headless,
    /// Ultra-lightweight restricted profile: minimal char budget (60k),
    /// read/edit operations, zero subagents, deny commands.
    Minimal,
}

/// A fully configured harness profile ready for execution.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HarnessProfile {
    pub kind: ProfileKind,
    pub name: String,
    pub description: String,
    pub config: HarnessConfig,
    pub enable_loop_guard: bool,
    pub enable_session_trace: bool,
}

impl HarnessProfile {
    /// Constructs the standard Code profile for software development.
    pub fn code(model: impl Into<String>) -> Self {
        let mut config = HarnessConfig::new(model);
        config.max_turns = 40;
        config.max_calls_per_turn = 16;
        config.max_output_tokens = 8192;
        config.context_char_budget = 240_000;
        config.allow_commands = false;
        config.max_delegation_depth = 1;

        Self {
            kind: ProfileKind::Code,
            name: "code".to_owned(),
            description:
                "Software engineering agent with staged edits, Fast Context, and diff review"
                    .to_owned(),
            config,
            enable_loop_guard: true,
            enable_session_trace: true,
        }
    }

    /// Constructs the Standard interactive assistant profile.
    pub fn standard(model: impl Into<String>) -> Self {
        let mut config = HarnessConfig::new(model);
        config.max_turns = 50;
        config.max_calls_per_turn = 20;
        config.max_output_tokens = 8192;
        config.context_char_budget = 280_000;
        config.allow_commands = false;
        config.max_delegation_depth = 2;

        Self {
            kind: ProfileKind::Standard,
            name: "standard".to_owned(),
            description: "Interactive project assistant with skills, memory, and full tools"
                .to_owned(),
            config,
            enable_loop_guard: true,
            enable_session_trace: true,
        }
    }

    /// Constructs the Headless CI/automation profile.
    pub fn headless(model: impl Into<String>) -> Self {
        let mut config = HarnessConfig::new(model);
        config.max_turns = 25;
        config.max_calls_per_turn = 12;
        config.max_output_tokens = 4096;
        config.context_char_budget = 180_000;
        config.allow_commands = false;
        config.max_delegation_depth = 0;

        Self {
            kind: ProfileKind::Headless,
            name: "headless".to_owned(),
            description:
                "Non-interactive automation profile with bounded turns and structured trace logging"
                    .to_owned(),
            config,
            enable_loop_guard: true,
            enable_session_trace: true,
        }
    }

    /// Constructs the Minimal constrained profile.
    pub fn minimal(model: impl Into<String>) -> Self {
        let mut config = HarnessConfig::new(model);
        config.max_turns = 15;
        config.max_calls_per_turn = 8;
        config.max_output_tokens = 4096;
        config.context_char_budget = 60_000;
        config.allow_commands = false;
        config.max_delegation_depth = 0;

        Self {
            kind: ProfileKind::Minimal,
            name: "minimal".to_owned(),
            description: "Lightweight profile with restricted tools, reduced memory budget, and no sub-agents".to_owned(),
            config,
            enable_loop_guard: true,
            enable_session_trace: true,
        }
    }

    /// Resolves a profile by name.
    pub fn from_name(name: &str, model: impl Into<String>) -> Result<Self, String> {
        match name.to_ascii_lowercase().as_str() {
            "code" => Ok(Self::code(model)),
            "standard" => Ok(Self::standard(model)),
            "headless" => Ok(Self::headless(model)),
            "minimal" => Ok(Self::minimal(model)),
            other => Err(format!(
                "unknown harness profile `{other}`; available profiles: code, standard, headless, minimal"
            )),
        }
    }

    /// Returns a catalog list of all shipped profile names and their summaries.
    pub fn available_profiles() -> &'static [(&'static str, &'static str)] {
        &[
            (
                "code",
                "Software engineering with staged edits, Fast Context, and diff review (default)",
            ),
            (
                "standard",
                "Full interactive assistant with skills, memory, and multi-turn tools",
            ),
            (
                "headless",
                "Non-interactive batch/CI runner with fast fail-safe and JSON trace logging",
            ),
            (
                "minimal",
                "Lightweight runner with restricted budgets and minimal token footprint",
            ),
        ]
    }
}
