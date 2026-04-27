//! Tests for the trajectory flamegraph renderer (G11).

use pi_agent_core::{SessionEntry, SessionEntryKind};
use pi_ai::{Message, ToolCall, ToolResult, Usage};
use pi_coding_agent::native::trajectory::flamegraph::render;
use serde_json::json;

fn entry(id: &str, kind: SessionEntryKind) -> SessionEntry {
    SessionEntry {
        id: id.into(),
        parent_id: None,
        timestamp: 0,
        kind,
    }
}

#[test]
fn renders_complete_html_document() {
    let html = render("session-abc", &[]);
    assert!(html.contains("<!DOCTYPE html>"));
    assert!(html.contains("</html>"));
    assert!(html.contains("session-abc"));
}

#[test]
fn renders_user_assistant_pair() {
    let branch = vec![
        entry(
            "u1",
            SessionEntryKind::User {
                message: Message::user_text("hello world"),
            },
        ),
        entry(
            "a1",
            SessionEntryKind::Assistant {
                message: Message::assistant_text("hi there"),
            },
        ),
    ];
    let html = render("s1", &branch);
    assert!(html.contains("class=\"block user\""));
    assert!(html.contains("class=\"block assistant-text\""));
    assert!(html.contains("turn 1"));
    assert!(!html.contains("turn 2"), "single turn → no turn 2");
}

#[test]
fn splits_into_multiple_turns_on_each_user_message() {
    let branch = vec![
        entry("u1", SessionEntryKind::User { message: Message::user_text("first") }),
        entry("a1", SessionEntryKind::Assistant { message: Message::assistant_text("ok") }),
        entry("u2", SessionEntryKind::User { message: Message::user_text("second") }),
        entry("a2", SessionEntryKind::Assistant { message: Message::assistant_text("ok2") }),
        entry("u3", SessionEntryKind::User { message: Message::user_text("third") }),
        entry("a3", SessionEntryKind::Assistant { message: Message::assistant_text("ok3") }),
    ];
    let html = render("s1", &branch);
    assert!(html.contains("turn 1"));
    assert!(html.contains("turn 2"));
    assert!(html.contains("turn 3"));
    assert!(!html.contains("turn 4"));
}

#[test]
fn tool_calls_and_results_render_within_their_turn() {
    let branch = vec![
        entry("u1", SessionEntryKind::User { message: Message::user_text("run ls") }),
        entry(
            "tc",
            SessionEntryKind::ToolCall {
                call: ToolCall {
                    id: "tc".into(),
                    name: "bash".into(),
                    input: json!({"command": "ls /tmp"}),
                },
            },
        ),
        entry(
            "tr",
            SessionEntryKind::ToolResult {
                result: ToolResult {
                    tool_use_id: "tc".into(),
                    model_output: "file1\nfile2".into(),
                    display: None,
                    is_error: false,
                },
            },
        ),
    ];
    let html = render("s1", &branch);
    assert!(html.contains("class=\"block tool-call\""));
    assert!(html.contains("class=\"block tool-result\""));
    assert!(html.contains("bash"));
}

#[test]
fn errored_tool_result_uses_error_class() {
    let branch = vec![
        entry("u1", SessionEntryKind::User { message: Message::user_text("x") }),
        entry(
            "tc",
            SessionEntryKind::ToolCall {
                call: ToolCall {
                    id: "tc".into(),
                    name: "bash".into(),
                    input: json!({"command": "false"}),
                },
            },
        ),
        entry(
            "tr",
            SessionEntryKind::ToolResult {
                result: ToolResult {
                    tool_use_id: "tc".into(),
                    model_output: "command failed".into(),
                    display: None,
                    is_error: true,
                },
            },
        ),
    ];
    let html = render("s1", &branch);
    assert!(html.contains("class=\"block tool-error\""));
}

#[test]
fn usage_entries_are_skipped_in_render_but_drive_total() {
    // Usage drives the denominator but is not its own block.
    let branch = vec![
        entry("u1", SessionEntryKind::User { message: Message::user_text("hi") }),
        entry(
            "use",
            SessionEntryKind::Usage {
                usage: Usage {
                    input_tokens: 100,
                    output_tokens: 200,
                    cache_read_tokens: 0,
                    cache_write_tokens: 0,
                    reasoning_tokens: 0,
                    cost_usd: 0.001,
                },
            },
        ),
    ];
    let html = render("s1", &branch);
    // No "block usage" class.
    assert!(!html.contains("class=\"block usage\""));
    // Total is reported in the stats.
    assert!(html.contains("estimated tokens: 300"));
}

#[test]
fn estimates_tokens_from_chars_when_no_usage() {
    let branch = vec![
        entry(
            "u1",
            SessionEntryKind::User {
                // 40 chars / 4 = 10 tokens estimate.
                message: Message::user_text("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"),
            },
        ),
    ];
    let html = render("s1", &branch);
    // Just sanity check it didn't panic and total is plausible.
    assert!(html.contains("estimated tokens: 10"));
}

#[test]
fn html_escapes_dangerous_characters_in_labels() {
    let branch = vec![
        entry(
            "u1",
            SessionEntryKind::User {
                message: Message::user_text("<script>alert('x')</script>"),
            },
        ),
    ];
    let html = render("s1", &branch);
    assert!(!html.contains("<script>alert"));
    assert!(html.contains("&lt;script&gt;"));
}

#[test]
fn outcome_and_evolve_marker_render_as_meta_blocks() {
    let branch = vec![
        entry("u1", SessionEntryKind::User { message: Message::user_text("hi") }),
        entry(
            "em",
            SessionEntryKind::EvolveMarker {
                agents_md_hash: "abc".into(),
                generation: 5,
                lineage: vec![],
            },
        ),
        entry(
            "oc",
            SessionEntryKind::Outcome {
                success: true,
                source: pi_agent_core::OutcomeSource::Heuristic,
                score: Some(0.85),
                notes: None,
            },
        ),
    ];
    let html = render("s1", &branch);
    assert!(html.contains("evolve gen 5"));
    assert!(html.contains("outcome: win"));
}
