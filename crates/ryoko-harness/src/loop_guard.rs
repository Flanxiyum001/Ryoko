//! Loop and thrashing guard for the agent tool loop.
//!
//! Inspired by DeepSeek Harness's loop prevention mechanisms. Detects
//! repetitive or cycling tool calls and intervenes before tokens and turns
//! are exhausted.

use serde_json::Value;
use std::collections::VecDeque;

/// Tracks recent tool invocations to identify infinite loops and thrashing.
#[derive(Debug, Clone)]
pub struct LoopGuard {
    recent_calls: VecDeque<(String, String)>,
    history_limit: usize,
    repetition_threshold: usize,
}

impl Default for LoopGuard {
    fn default() -> Self {
        Self::new(8, 3)
    }
}

impl LoopGuard {
    pub fn new(history_limit: usize, repetition_threshold: usize) -> Self {
        Self {
            recent_calls: VecDeque::with_capacity(history_limit),
            history_limit,
            repetition_threshold,
        }
    }

    /// Records a tool call and checks whether an intervention should occur.
    ///
    /// Returns `Some(warning_message)` if the tool call represents a loop, or `None` if safe.
    pub fn check_and_record(&mut self, name: &str, arguments: &Value) -> Option<String> {
        self.check_and_record_with_revision(name, arguments, 0)
    }

    /// Records a tool call with a state revision and checks whether an intervention should occur.
    pub fn check_and_record_with_revision(
        &mut self,
        name: &str,
        arguments: &Value,
        revision: usize,
    ) -> Option<String> {
        let args_repr = if revision > 0 {
            format!("{}:rev={}", arguments, revision)
        } else {
            arguments.to_string()
        };
        let key = (name.to_owned(), args_repr);

        // Count how many consecutive or recent identical calls exist
        let matches = self
            .recent_calls
            .iter()
            .filter(|call| **call == key)
            .count();

        if self.recent_calls.len() >= self.history_limit {
            self.recent_calls.pop_front();
        }
        self.recent_calls.push_back(key);

        if matches + 1 >= self.repetition_threshold {
            Some(format!(
                "Loop protection triggered: you have called `{name}` with identical arguments {} times. \
                 Do not repeat this call. Re-evaluate the output you received or try a different approach.",
                matches + 1
            ))
        } else {
            None
        }
    }

    /// Invalidates recorded read history for a file path after a successful edit.
    pub fn invalidate_file(&mut self, path: &str) {
        self.recent_calls.retain(|(name, args)| {
            if name == "read_file" {
                let json_part = args.split(":rev=").next().unwrap_or(args.as_str());
                if let Ok(val) = serde_json::from_str::<Value>(json_part) {
                    if val.get("path").and_then(Value::as_str) == Some(path) {
                        return false;
                    }
                }
            }
            true
        });
    }

    /// Clears the loop guard history.
    pub fn reset(&mut self) {
        self.recent_calls.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn test_loop_guard_detects_consecutive_repeats() {
        let mut guard = LoopGuard::new(6, 3);
        let args = json!({"path": "src/main.rs"});

        assert!(guard.check_and_record("read_file", &args).is_none());
        assert!(guard.check_and_record("read_file", &args).is_none());

        let warning = guard.check_and_record("read_file", &args);
        assert!(warning.is_some());
        assert!(warning.unwrap().contains("Loop protection triggered"));
    }

    #[test]
    fn test_loop_guard_allows_different_args() {
        let mut guard = LoopGuard::new(6, 3);
        let args1 = json!({"path": "src/main.rs"});
        let args2 = json!({"path": "src/lib.rs"});

        assert!(guard.check_and_record("read_file", &args1).is_none());
        assert!(guard.check_and_record("read_file", &args2).is_none());
        assert!(guard.check_and_record("read_file", &args1).is_none());
        assert!(guard.check_and_record("read_file", &args2).is_none());
    }

    #[test]
    fn test_loop_guard_invalidation_after_edit() {
        let mut guard = LoopGuard::new(6, 3);
        let args = json!({"path": "src/main.rs"});

        assert!(guard.check_and_record("read_file", &args).is_none());
        assert!(guard.check_and_record("read_file", &args).is_none());

        // Invalidate read history when edit succeeds
        guard.invalidate_file("src/main.rs");

        // Reading again should now start fresh without triggering immediately
        assert!(guard.check_and_record("read_file", &args).is_none());
        assert!(guard.check_and_record("read_file", &args).is_none());
        assert!(guard.check_and_record("read_file", &args).is_some());
    }

    #[test]
    fn test_loop_guard_revision_tracking() {
        let mut guard = LoopGuard::new(6, 3);
        let args = json!({"path": "src/main.rs"});

        // Read revision 0 twice
        assert!(
            guard
                .check_and_record_with_revision("read_file", &args, 0)
                .is_none()
        );
        assert!(
            guard
                .check_and_record_with_revision("read_file", &args, 0)
                .is_none()
        );

        // Revision increments after edit
        assert!(
            guard
                .check_and_record_with_revision("read_file", &args, 1)
                .is_none()
        );
        assert!(
            guard
                .check_and_record_with_revision("read_file", &args, 1)
                .is_none()
        );
        assert!(
            guard
                .check_and_record_with_revision("read_file", &args, 1)
                .is_some()
        );
    }
}
