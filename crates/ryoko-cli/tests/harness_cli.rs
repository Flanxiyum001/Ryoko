use std::fs;
use tempfile::tempdir;

#[test]
fn harness_profiles_lists_all_profiles() {
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_ryoko"))
        .arg("harness")
        .arg("profiles")
        .output()
        .expect("runs ryoko harness profiles");

    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("Available Ryoko Harness profiles:"));
    assert!(stdout.contains("code"));
    assert!(stdout.contains("standard"));
    assert!(stdout.contains("headless"));
    assert!(stdout.contains("minimal"));
}

#[test]
fn dsh_alias_works_for_profiles() {
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_ryoko"))
        .arg("dsh")
        .arg("profiles")
        .output()
        .expect("runs ryoko dsh profiles");

    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("Available Ryoko Harness profiles:"));
}

#[test]
fn harness_trace_inspects_saved_journal() {
    let dir = tempdir().unwrap();
    let journal_path = dir.path().join("trace.json");

    let sample_json = r#"[
        {
            "type": "session_started",
            "session_id": "test-session-123",
            "goal": "Write tests",
            "profile": "code",
            "model": "deepseek/deepseek-chat",
            "timestamp": "2026-11-27T16:00:00Z"
        },
        {
            "type": "turn_started",
            "turn": 1,
            "timestamp": "2026-11-27T16:00:01Z"
        },
        {
            "type": "tool_started",
            "turn": 1,
            "call_id": "call-1",
            "name": "read_file",
            "args": {"path": "src/lib.rs"}
        },
        {
            "type": "tool_finished",
            "turn": 1,
            "call_id": "call-1",
            "name": "read_file",
            "ok": true,
            "output_chars": 250,
            "duration_ms": 15
        },
        {
            "type": "session_ended",
            "turns": 1,
            "stop_cause": "Completed",
            "total_changes": 0,
            "duration_ms": 450
        }
    ]"#;

    fs::write(&journal_path, sample_json).unwrap();

    let output = std::process::Command::new(env!("CARGO_BIN_EXE_ryoko"))
        .arg("harness")
        .arg("trace")
        .arg(journal_path.to_str().unwrap())
        .output()
        .expect("runs ryoko harness trace");

    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("Ryoko Harness Session Trace"));
    assert!(stdout.contains("test-session-123"));
    assert!(stdout.contains("read_file"));
    assert!(stdout.contains("SESSION END"));
}
