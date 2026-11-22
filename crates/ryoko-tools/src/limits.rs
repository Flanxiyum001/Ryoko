//! Execution limits and budgets for tools.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

/// Configuration for tool execution timeouts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TimeoutConfig {
    /// Default timeout for tool execution.
    pub default: Duration,
    /// Maximum timeout allowed.
    pub max: Duration,
}

impl Default for TimeoutConfig {
    fn default() -> Self {
        Self {
            default: Duration::from_secs(30),
            max: Duration::from_secs(300),
        }
    }
}

/// Limits for tool execution.
#[derive(Debug, Clone)]
pub struct ToolLimits {
    /// Maximum execution time for a single tool call.
    pub timeout: Duration,
    /// Maximum output size in bytes.
    pub max_output_bytes: usize,
    /// Maximum number of tool calls per task.
    pub max_calls_per_task: u64,
    /// Maximum number of concurrent tool executions.
    pub max_concurrency: usize,
    /// Whether to allow network access.
    pub allow_network: bool,
    /// Whether to allow file writes.
    pub allow_writes: bool,
    /// Whether to allow command execution.
    pub allow_exec: bool,
}

impl Default for ToolLimits {
    fn default() -> Self {
        Self {
            timeout: Duration::from_secs(30),
            max_output_bytes: 1024 * 1024, // 1 MB
            max_calls_per_task: 100,
            max_concurrency: 4,
            allow_network: false,
            allow_writes: true,
            allow_exec: false,
        }
    }
}

impl ToolLimits {
    /// Create limits for a read-only task.
    pub fn read_only() -> Self {
        Self {
            allow_writes: false,
            allow_exec: false,
            ..Default::default()
        }
    }

    /// Create limits for a code generation task.
    pub fn code_generation() -> Self {
        Self {
            timeout: Duration::from_secs(120),
            max_output_bytes: 4 * 1024 * 1024, // 4 MB
            max_calls_per_task: 50,
            allow_exec: true,
            ..Default::default()
        }
    }

    /// Create limits for a test execution task.
    pub fn test_execution() -> Self {
        Self {
            timeout: Duration::from_secs(300),
            max_output_bytes: 16 * 1024 * 1024, // 16 MB
            max_calls_per_task: 10,
            allow_exec: true,
            ..Default::default()
        }
    }
}

/// Budget for tool calls, tracked per task and globally.
#[derive(Debug, Clone)]
pub struct ToolBudget {
    /// Total calls allowed.
    total_calls: AtomicU64,
    /// Calls made so far.
    calls_made: AtomicU64,
    /// Per-tool call limits.
    per_tool_limits: Arc<tokio::sync::RwLock<std::collections::HashMap<String, u64>>>,
    /// Per-tool calls made.
    per_tool_calls: Arc<tokio::sync::RwLock<std::collections::HashMap<String, u64>>>,
}

impl Default for ToolBudget {
    fn default() -> Self {
        Self::unlimited()
    }
}

impl ToolBudget {
    /// Create an unlimited budget.
    pub fn unlimited() -> Self {
        Self {
            total_calls: AtomicU64::new(u64::MAX),
            calls_made: AtomicU64::new(0),
            per_tool_limits: Arc::new(tokio::sync::RwLock::new(std::collections::HashMap::new())),
            per_tool_calls: Arc::new(tokio::sync::RwLock::new(std::collections::HashMap::new())),
        }
    }

    /// Create a budget with a total call limit.
    pub fn new(total_calls: u64) -> Self {
        Self {
            total_calls: AtomicU64::new(total_calls),
            calls_made: AtomicU64::new(0),
            per_tool_limits: Arc::new(tokio::sync::RwLock::new(std::collections::HashMap::new())),
            per_tool_calls: Arc::new(tokio::sync::RwLock::new(std::collections::HashMap::new())),
        }
    }

    /// Set a per-tool call limit.
    pub async fn set_tool_limit(&self, tool: &str, limit: u64) {
        self.per_tool_limits.write().await.insert(tool.to_owned(), limit);
    }

    /// Check if a call is allowed and reserve budget.
    pub async fn check_and_reserve(&self, tool: &str, calls: u64) -> crate::error::ToolResult<()> {
        // Check total budget
        let current = self.calls_made.fetch_add(calls, Ordering::SeqCst);
        if current + calls > self.total_calls.load(Ordering::SeqCst) {
            self.calls_made.fetch_sub(calls, Ordering::SeqCst);
            return Err(crate::error::ToolError::BudgetExceeded(
                format!("total tool call budget exhausted ({}/{})", current + calls, self.total_calls.load(Ordering::SeqCst))
            ));
        }

        // Check per-tool budget
        let mut per_tool = self.per_tool_calls.write().await;
        let tool_current = per_tool.entry(tool.to_owned()).or_insert(0);
        if let Some(limit) = self.per_tool_limits.read().await.get(tool) {
            if *tool_current + calls > *limit {
                *tool_current -= calls;
                return Err(crate::error::ToolError::BudgetExceeded(
                    format!("tool '{}' call budget exhausted ({}/{})", tool, *tool_current + calls, limit)
                ));
            }
        }
        *tool_current += calls;

        Ok(())
    }

    /// Get remaining total calls.
    pub fn remaining_total(&self) -> u64 {
        self.total_calls.load(Ordering::Relaxed).saturating_sub(self.calls_made.load(Ordering::Relaxed))
    }

    /// Get remaining calls for a tool.
    pub async fn remaining_for_tool(&self, tool: &str) -> Option<u64> {
        let limits = self.per_tool_limits.read().await;
        let calls = self.per_tool_calls.read().await;
        limits.get(tool).map(|limit| {
            limit.saturating_sub(*calls.get(tool).unwrap_or(&0))
        })
    }

    /// Get calls made for a tool.
    pub async fn calls_for_tool(&self, tool: &str) -> u64 {
        self.per_tool_calls.read().await.get(tool).copied().unwrap_or(0)
    }
}

/// Output size limits configuration.
#[derive(Debug, Clone)]
pub struct OutputLimits {
    /// Maximum text output size in bytes.
    pub max_text_bytes: usize,
    /// Maximum structured data size in bytes.
    pub max_data_bytes: usize,
    /// Maximum number of files in output.
    pub max_files: usize,
    /// Maximum size per file in bytes.
    pub max_file_bytes: usize,
}

impl Default for OutputLimits {
    fn default() -> Self {
        Self {
            max_text_bytes: 64 * 1024,      // 64 KB
            max_data_bytes: 256 * 1024,     // 256 KB
            max_files: 100,
            max_file_bytes: 4 * 1024 * 1024, // 4 MB
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn budget_tracks_calls() {
        let budget = ToolBudget::new(5);
        assert_eq!(budget.remaining_total(), 5);

        budget.check_and_reserve("tool1", 1).await.unwrap();
        assert_eq!(budget.remaining_total(), 4);

        budget.check_and_reserve("tool1", 2).await.unwrap();
        assert_eq!(budget.remaining_total(), 2);

        // Exceed budget
        let result = budget.check_and_reserve("tool1", 3).await;
        assert!(matches!(result, Err(crate::error::ToolError::BudgetExceeded(_))));
    }

    #[tokio::test]
    async fn budget_per_tool_limits() {
        let budget = ToolBudget::new(100);
        budget.set_tool_limit("expensive_tool", 2).await;

        budget.check_and_reserve("expensive_tool", 1).await.unwrap();
        budget.check_and_reserve("expensive_tool", 1).await.unwrap();

        let result = budget.check_and_reserve("expensive_tool", 1).await;
        assert!(matches!(result, Err(crate::error::ToolError::BudgetExceeded(_))));

        // Other tools still work
        budget.check_and_reserve("other_tool", 10).await.unwrap();
    }

    #[test]
    fn limits_presets() {
        let read_only = ToolLimits::read_only();
        assert!(!read_only.allow_writes);
        assert!(!read_only.allow_exec);

        let code_gen = ToolLimits::code_generation();
        assert!(code_gen.allow_writes);
        assert!(code_gen.allow_exec);
        assert_eq!(code_gen.timeout, Duration::from_secs(120));

        let test_exec = ToolLimits::test_execution();
        assert!(test_exec.allow_exec);
        assert_eq!(test_exec.timeout, Duration::from_secs(300));
    }
}