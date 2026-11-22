//! Approval gate for sensitive tool operations.
//!
//! This module defines the approval mechanism that requires explicit operator
//! consent for sensitive or elevated operations. Agents can never grant
//! themselves approval through prompts, tool arguments, or generated files.

use async_trait::async_trait;
use ryoko_core::mcp::McpApprovalScope;
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use std::sync::Arc;
use tokio::sync::Mutex;

/// Decision made by an approval gate.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolApproval {
    pub tool: String,
    pub scope: ApprovalScope,
    pub allowed: bool,
    pub reused_grant: bool,
}

/// How long an approval grant applies.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ApprovalScope {
    /// Allow this specific call only.
    Once,
    /// Allow this tool for the rest of the run.
    ForRun,
    /// Deny this tool (and don't ask again for this run).
    Deny,
}

impl From<McpApprovalScope> for ApprovalScope {
    fn from(scope: McpApprovalScope) -> Self {
        match scope {
            McpApprovalScope::Once => ApprovalScope::Once,
            McpApprovalScope::ForRun => ApprovalScope::ForRun,
            McpApprovalScope::Deny => ApprovalScope::Deny,
        }
    }
}

impl From<ApprovalScope> for McpApprovalScope {
    fn from(scope: ApprovalScope) -> Self {
        match scope {
            ApprovalScope::Once => McpApprovalScope::Once,
            ApprovalScope::ForRun => McpApprovalScope::ForRun,
            ApprovalScope::Deny => McpApprovalScope::Deny,
        }
    }
}

/// Trait for an approval gate that asks a human operator for permission.
#[async_trait]
pub trait ApprovalGate: Send + Sync {
    /// Ask the operator for approval to run a tool with the given arguments.
    /// Returns the scope of the approval.
    async fn approve(&self, tool: &str, arguments: &serde_json::Value) -> ApprovalScope;
}

/// Session-based approval gate that remembers grants for the run.
pub struct SessionApprovalGate {
    inner: Arc<dyn ApprovalGate>,
    granted: Mutex<BTreeSet<String>>,
    denied: Mutex<BTreeSet<String>>,
    decisions: Mutex<Vec<ToolApproval>>,
}

impl SessionApprovalGate {
    pub fn new(inner: Arc<dyn ApprovalGate>) -> Self {
        Self {
            inner,
            granted: Mutex::new(BTreeSet::new()),
            denied: Mutex::new(BTreeSet::new()),
            decisions: Mutex::new(Vec::new()),
        }
    }

    /// Get all grants for this session.
    pub async fn grants(&self) -> Vec<String> {
        self.granted.lock().await.iter().cloned().collect()
    }

    /// Get all decisions made in this session.
    pub async fn decisions(&self) -> Vec<ToolApproval> {
        self.decisions.lock().await.clone()
    }

    /// Revoke all grants (e.g., when a new run starts).
    pub async fn revoke_grants(&self) {
        self.granted.lock().await.clear();
    }
}

#[async_trait]
impl ApprovalGate for SessionApprovalGate {
    async fn approve(&self, tool: &str, arguments: &serde_json::Value) -> ApprovalScope {
        if tool.trim().is_empty() {
            let decision = ToolApproval {
                tool: String::new(),
                scope: ApprovalScope::Deny,
                allowed: false,
                reused_grant: false,
            };
            self.decisions.lock().await.push(decision);
            return ApprovalScope::Deny;
        }

        // Check if denied
        if self.denied.lock().await.contains(tool) {
            let decision = ToolApproval {
                tool: tool.to_owned(),
                scope: ApprovalScope::Deny,
                allowed: false,
                reused_grant: true,
            };
            self.decisions.lock().await.push(decision);
            return ApprovalScope::Deny;
        }

        // Check if granted for the run
        if self.granted.lock().await.contains(tool) {
            let decision = ToolApproval {
                tool: tool.to_owned(),
                scope: ApprovalScope::ForRun,
                allowed: true,
                reused_grant: true,
            };
            self.decisions.lock().await.push(decision);
            return ApprovalScope::ForRun;
        }

        // Ask the operator
        let scope = self.inner.approve(tool, arguments).await;
        let allowed = scope != ApprovalScope::Deny;

        if allowed && scope == ApprovalScope::ForRun {
            self.granted.lock().await.insert(tool.to_owned());
        }
        if !allowed {
            self.denied.lock().await.insert(tool.to_owned());
        }

        let decision = ToolApproval {
            tool: tool.to_owned(),
            scope,
            allowed,
            reused_grant: false,
        };
        self.decisions.lock().await.push(decision);

        scope
    }
}

/// Terminal-based approval asker for CLI.
pub struct TerminalApprovalAsker {
    interactive: bool,
    pre_approved: BTreeSet<String>,
}

impl TerminalApprovalAsker {
    pub fn new() -> Self {
        Self {
            interactive: true,
            pre_approved: BTreeSet::new(),
        }
    }

    pub fn non_interactive_with(pre_approved: impl IntoIterator<Item = String>) -> Self {
        Self {
            interactive: false,
            pre_approved: pre_approved.into_iter().collect(),
        }
    }
}

fn is_non_interactive() -> bool {
    std::env::var("RYOKO_NONINTERACTIVE")
        .map(|v| matches!(v.trim(), "1" | "true" | "yes"))
        .unwrap_or(false)
}

fn scope_from_answer(answer: &str) -> ApprovalScope {
    match answer.trim().to_ascii_lowercase().as_str() {
        "o" | "once" | "y" | "yes" => ApprovalScope::Once,
        "r" | "run" => ApprovalScope::ForRun,
        _ => ApprovalScope::Deny,
    }
}

#[async_trait]
impl ApprovalGate for TerminalApprovalAsker {
    async fn approve(&self, tool: &str, arguments: &serde_json::Value) -> ApprovalScope {
        if !self.interactive || is_non_interactive() {
            // Headless mode: check pre-approved list
            if self.pre_approved.contains(tool) {
                return ApprovalScope::ForRun;
            }
            return ApprovalScope::Deny;
        }

        // Interactive mode: prompt the user
        eprintln!("Tool approval required: {}", tool);
        eprintln!("Arguments: {}", serde_json::to_string_pretty(arguments).unwrap_or_default());
        eprint!("Allow [o]nce, for this [r]un, or [d]eny (default)? ");

        use std::io::{self, Write};
        io::stdout().flush().ok();

        let mut answer = String::new();
        let result = tokio::time::timeout(
            std::time::Duration::from_secs(90),
            tokio::io::AsyncBufReadExt::read_line(&mut tokio::io::BufReader::new(tokio::io::stdin()), &mut answer),
        ).await;

        match result {
            Ok(Ok(_)) => scope_from_answer(&answer),
            _ => ApprovalScope::Deny,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    struct MockGate(bool);

    #[async_trait]
    impl ApprovalGate for MockGate {
        async fn approve(&self, _tool: &str, _arguments: &serde_json::Value) -> ApprovalScope {
            if self.0 { ApprovalScope::ForRun } else { ApprovalScope::Deny }
        }
    }

    #[tokio::test]
    async fn session_gate_caches_grants() {
        let gate = SessionApprovalGate::new(Arc::new(MockGate(true)));
        let scope1 = gate.approve("test_tool", &json!({})).await;
        let scope2 = gate.approve("test_tool", &json!({})).await;

        assert_eq!(scope1, ApprovalScope::ForRun);
        assert_eq!(scope2, ApprovalScope::ForRun);

        let decisions = gate.decisions().await;
        assert_eq!(decisions.len(), 2);
        assert!(decisions[1].reused_grant);
    }

    #[tokio::test]
    async fn session_gate_caches_denials() {
        let gate = SessionApprovalGate::new(Arc::new(MockGate(false)));
        let scope1 = gate.approve("test_tool", &json!({})).await;
        let scope2 = gate.approve("test_tool", &json!({})).await;

        assert_eq!(scope1, ApprovalScope::Deny);
        assert_eq!(scope2, ApprovalScope::Deny);

        let decisions = gate.decisions().await;
        assert_eq!(decisions.len(), 2);
        assert!(decisions[1].reused_grant);
    }

    #[tokio::test]
    async fn session_gate_revokes_grants() {
        let gate = SessionApprovalGate::new(Arc::new(MockGate(true)));
        let _ = gate.approve("test_tool", &json!({})).await;
        gate.revoke_grants().await;
        let grants = gate.grants().await;
        assert!(grants.is_empty());
    }

    #[test]
    fn scope_from_answer() {
        assert_eq!(scope_from_answer("o"), ApprovalScope::Once);
        assert_eq!(scope_from_answer("once"), ApprovalScope::Once);
        assert_eq!(scope_from_answer("YES"), ApprovalScope::Once);
        assert_eq!(scope_from_answer("r"), ApprovalScope::ForRun);
        assert_eq!(scope_from_answer("run"), ApprovalScope::ForRun);
        assert_eq!(scope_from_answer(""), ApprovalScope::Deny);
        assert_eq!(scope_from_answer("no"), ApprovalScope::Deny);
        assert_eq!(scope_from_answer("d"), ApprovalScope::Deny);
    }
}