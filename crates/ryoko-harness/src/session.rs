//! Durable session journaling and execution traces.
//!
//! Directly inspired by DeepSeek Harness's append-only `SessionEvent` trace
//! architecture. Detailed traces provide full visibility into what the model
//! saw before making decisions, what tools were called, timing, and why
//! failures or interventions occurred.

use std::fs;
use std::io;
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Instant;

use chrono::Utc;
use serde::{Deserialize, Serialize};

use crate::harness::PlanItem;

/// A durable event recorded during a harness execution session.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum SessionEvent {
    SessionStarted {
        session_id: String,
        goal: String,
        profile: String,
        model: String,
        timestamp: String,
    },
    TurnStarted {
        turn: usize,
        timestamp: String,
    },
    ModelRequested {
        turn: usize,
        model: String,
        message_count: usize,
        prompt_chars: usize,
    },
    ModelResponded {
        turn: usize,
        text_length: usize,
        tool_calls: Vec<String>,
        stop_reason: Option<String>,
        duration_ms: u64,
    },
    ToolStarted {
        turn: usize,
        call_id: String,
        name: String,
        args: serde_json::Value,
    },
    ToolFinished {
        turn: usize,
        call_id: String,
        name: String,
        ok: bool,
        output_chars: usize,
        duration_ms: u64,
    },
    LoopIntervention {
        turn: usize,
        tool_name: String,
        repetitions: usize,
    },
    ChangeStaged {
        path: String,
        original_bytes: Option<usize>,
        staged_bytes: usize,
    },
    PlanUpdated {
        items: Vec<PlanItem>,
    },
    MemorySaved {
        note: String,
    },
    ContextCompacted {
        turn: usize,
        elided_count: usize,
    },
    SubAgentStarted {
        task: String,
    },
    SubAgentFinished {
        summary_chars: usize,
    },
    TurnEnded {
        turn: usize,
        duration_ms: u64,
    },
    SessionEnded {
        turns: usize,
        stop_cause: String,
        total_changes: usize,
        duration_ms: u64,
    },
}

/// Thread-safe append-only journal capturing every step of a harness run.
#[derive(Debug, Clone, Default)]
pub struct SessionJournal {
    events: Arc<Mutex<Vec<SessionEvent>>>,
    start_time: Arc<Mutex<Option<Instant>>>,
}

impl SessionJournal {
    pub fn new() -> Self {
        Self {
            events: Arc::new(Mutex::new(Vec::new())),
            start_time: Arc::new(Mutex::new(Some(Instant::now()))),
        }
    }

    /// Records a new event in the append-only journal.
    pub fn record(&self, event: SessionEvent) {
        if let Ok(mut lock) = self.events.lock() {
            lock.push(event);
        }
    }

    /// Returns a copy of all recorded events.
    pub fn events(&self) -> Vec<SessionEvent> {
        self.events
            .lock()
            .map(|guard| guard.clone())
            .unwrap_or_default()
    }

    /// Returns total elapsed milliseconds since journal initialization.
    pub fn elapsed_ms(&self) -> u64 {
        self.start_time
            .lock()
            .ok()
            .and_then(|t| *t)
            .map(|start| start.elapsed().as_millis() as u64)
            .unwrap_or(0)
    }

    /// Current UTC ISO-8601 timestamp string.
    pub fn now_iso() -> String {
        Utc::now().to_rfc3339()
    }

    /// Exports the journal to a JSON file.
    pub fn save_to_json(&self, path: &Path) -> io::Result<()> {
        let events = self.events();
        let json = serde_json::to_string_pretty(&events)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let mut options = fs::OpenOptions::new();
        options.write(true).create(true).truncate(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        use std::io::Write;
        let mut file = options.open(path)?;
        file.write_all(json.as_bytes())?;
        file.sync_all()
    }

    /// Imports a session journal from a JSON file.
    pub fn load_from_json(path: &Path) -> io::Result<Self> {
        let text = fs::read_to_string(path)?;
        let events: Vec<SessionEvent> = serde_json::from_str(&text)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
        Ok(Self {
            events: Arc::new(Mutex::new(events)),
            start_time: Arc::new(Mutex::new(None)),
        })
    }

    /// Generates a human-readable execution trace suitable for debugging and inspection.
    pub fn inspect(&self) -> String {
        let events = self.events();
        if events.is_empty() {
            return "Session trace is empty.".to_owned();
        }

        let mut out = String::new();
        out.push_str("=== Ryoko Harness Session Trace ===\n");

        for event in events {
            match event {
                SessionEvent::SessionStarted {
                    session_id,
                    goal,
                    profile,
                    model,
                    timestamp,
                } => {
                    out.push_str(&format!(
                        "[{timestamp}] SESSION START | ID: {session_id} | Profile: {profile} | Model: {model}\n"
                    ));
                    out.push_str(&format!("Goal: {goal}\n---\n"));
                }
                SessionEvent::TurnStarted { turn, timestamp } => {
                    out.push_str(&format!("\n▶ TURN {turn} [{timestamp}]\n"));
                }
                SessionEvent::ModelRequested {
                    turn,
                    model,
                    message_count,
                    prompt_chars,
                } => {
                    out.push_str(&format!(
                        "  [T{turn} Req] {model} with {message_count} msgs ({prompt_chars} chars)\n"
                    ));
                }
                SessionEvent::ModelResponded {
                    turn,
                    text_length,
                    tool_calls,
                    stop_reason,
                    duration_ms,
                } => {
                    let tools = if tool_calls.is_empty() {
                        "none".to_owned()
                    } else {
                        tool_calls.join(", ")
                    };
                    out.push_str(&format!(
                        "  [T{turn} Res] {duration_ms}ms | text: {text_length} chars | tools: [{tools}] | stop: {}\n",
                        stop_reason.as_deref().unwrap_or("completed")
                    ));
                }
                SessionEvent::ToolStarted {
                    turn,
                    call_id: _,
                    name,
                    args,
                } => {
                    let args_str = serde_json::to_string(&args).unwrap_or_default();
                    let truncated = truncate_preview(&args_str, 120);
                    out.push_str(&format!("  [T{turn} Tool:Run] {name}({truncated})\n"));
                }
                SessionEvent::ToolFinished {
                    turn,
                    call_id: _,
                    name,
                    ok,
                    output_chars,
                    duration_ms,
                } => {
                    let status = if ok { "OK" } else { "ERR" };
                    out.push_str(&format!(
                        "  [T{turn} Tool:End] {name} -> {status} ({output_chars} chars, {duration_ms}ms)\n"
                    ));
                }
                SessionEvent::LoopIntervention {
                    turn,
                    tool_name,
                    repetitions,
                } => {
                    out.push_str(&format!(
                        "  [T{turn} GUARD] Detected {repetitions} repeated calls to {tool_name}; intervention injected.\n"
                    ));
                }
                SessionEvent::ChangeStaged {
                    path,
                    original_bytes,
                    staged_bytes,
                } => {
                    let orig = original_bytes
                        .map(|b| format!("{b} B"))
                        .unwrap_or_else(|| "new file".to_owned());
                    out.push_str(&format!("  [STAGE] `{path}`: {orig} -> {staged_bytes} B\n"));
                }
                SessionEvent::PlanUpdated { items } => {
                    out.push_str("  [PLAN] Updated:\n");
                    for (idx, item) in items.iter().enumerate() {
                        out.push_str(&format!(
                            "    {}. [{:?}] {}\n",
                            idx + 1,
                            item.status,
                            item.task
                        ));
                    }
                }
                SessionEvent::MemorySaved { note } => {
                    out.push_str(&format!("  [MEMORY] Saved note: {note}\n"));
                }
                SessionEvent::ContextCompacted { turn, elided_count } => {
                    out.push_str(&format!(
                        "  [T{turn} COMPACT] Elided {elided_count} older tool results to save tokens.\n"
                    ));
                }
                SessionEvent::SubAgentStarted { task } => {
                    let preview = truncate_preview(&task, 60);
                    out.push_str(&format!("  [SUB-AGENT] Started: \"{preview}\"\n"));
                }
                SessionEvent::SubAgentFinished { summary_chars } => {
                    out.push_str(&format!(
                        "  [SUB-AGENT] Finished (summary: {summary_chars} chars)\n"
                    ));
                }
                SessionEvent::TurnEnded { turn, duration_ms } => {
                    out.push_str(&format!("  [T{turn} Done] took {duration_ms}ms\n"));
                }
                SessionEvent::SessionEnded {
                    turns,
                    stop_cause,
                    total_changes,
                    duration_ms,
                } => {
                    out.push_str(&format!(
                        "\n=== SESSION END | Turns: {turns} | Stop: {stop_cause} | Changes: {total_changes} files | Duration: {duration_ms}ms ===\n"
                    ));
                }
            }
        }

        out
    }
}

fn truncate_preview(text: &str, max_bytes: usize) -> String {
    if text.len() > max_bytes {
        let mut end = max_bytes;
        while end > 0 && !text.is_char_boundary(end) {
            end -= 1;
        }
        format!("{}…", &text[..end])
    } else {
        text.to_owned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_truncate_preview_utf8_boundaries() {
        // Multi-byte unicode: 🦀 is 4 bytes (F0 9F A6 80)
        let s = "🦀🦀🦀"; // 12 bytes
        // Cutting at 6 bytes falls in the middle of the second crab (byte 4..8)
        let truncated = truncate_preview(s, 6);
        assert_eq!(truncated, "🦀…");

        // 3-byte characters (e.g. Japanese kanji: 漢 is 3 bytes)
        let s2 = "漢字テスト"; // 5 * 3 = 15 bytes
        // Cutting at 5 bytes falls in the middle of '字' (bytes 3..6)
        let truncated2 = truncate_preview(s2, 5);
        assert_eq!(truncated2, "漢…");

        let ascii = "short";
        assert_eq!(truncate_preview(ascii, 10), "short");
    }
}
