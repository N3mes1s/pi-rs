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
        entry(
            "u1",
            SessionEntryKind::User {
                message: Message::user_text("first"),
            },
        ),
        entry(
            "a1",
            SessionEntryKind::Assistant {
                message: Message::assistant_text("ok"),
            },
        ),
        entry(
            "u2",
            SessionEntryKind::User {
                message: Message::user_text("second"),
            },
        ),
        entry(
            "a2",
            SessionEntryKind::Assistant {
                message: Message::assistant_text("ok2"),
            },
        ),
        entry(
            "u3",
            SessionEntryKind::User {
                message: Message::user_text("third"),
            },
        ),
        entry(
            "a3",
            SessionEntryKind::Assistant {
                message: Message::assistant_text("ok3"),
            },
        ),
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
        entry(
            "u1",
            SessionEntryKind::User {
                message: Message::user_text("run ls"),
            },
        ),
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
        entry(
            "u1",
            SessionEntryKind::User {
                message: Message::user_text("x"),
            },
        ),
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
        entry(
            "u1",
            SessionEntryKind::User {
                message: Message::user_text("hi"),
            },
        ),
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
    let branch = vec![entry(
        "u1",
        SessionEntryKind::User {
            // 40 chars: bytes/4 estimated 10 tokens; RFD-0014's
            // real BPE tokenizer returns somewhere in [3, 14].
            message: Message::user_text("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"),
        },
    )];
    let html = render("s1", &branch);
    // Just sanity-check the render didn't panic and emitted a token
    // estimate. Exact count varies with the tokenizer encoding
    // (see RFD 0014).
    assert!(
        html.contains("estimated tokens"),
        "html missing estimate: {html}"
    );
}

#[test]
fn html_escapes_dangerous_characters_in_labels() {
    let branch = vec![entry(
        "u1",
        SessionEntryKind::User {
            message: Message::user_text("<script>alert('x')</script>"),
        },
    )];
    let html = render("s1", &branch);
    assert!(!html.contains("<script>alert"));
    assert!(html.contains("&lt;script&gt;"));
}

#[test]
fn outcome_and_evolve_marker_render_as_meta_blocks() {
    let branch = vec![
        entry(
            "u1",
            SessionEntryKind::User {
                message: Message::user_text("hi"),
            },
        ),
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

// ─── RFD 0012: JSON output ─────────────────────────────────────────────

#[test]
fn json_format_round_trip_produces_expected_block_kinds() {
    use pi_coding_agent::native::trajectory::flamegraph::{build_trajectory, render_json, Format};
    use serde_json::Value;

    let branch = vec![
        entry(
            "m",
            SessionEntryKind::Meta {
                cwd: "/tmp".into(),
                provider: "anthropic".into(),
                model: "sonnet".into(),
                title: None,
            },
        ),
        entry(
            "ctx",
            SessionEntryKind::ContextLoad {
                source: "/repo/AGENTS.md".into(),
                bytes: 800,
                tokens: Some(200),
            },
        ),
        entry(
            "u1",
            SessionEntryKind::User {
                message: Message::user_text("run ls"),
            },
        ),
        entry(
            "a1",
            SessionEntryKind::Assistant {
                message: Message::assistant_text("ok, running"),
            },
        ),
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
        entry(
            "use",
            SessionEntryKind::Usage {
                usage: Usage {
                    input_tokens: 100,
                    output_tokens: 200,
                    cache_read_tokens: 0,
                    cache_write_tokens: 0,
                    reasoning_tokens: 0,
                    cost_usd: 0.0042,
                },
            },
        ),
        entry(
            "oc",
            SessionEntryKind::Outcome {
                success: true,
                source: pi_agent_core::OutcomeSource::LlmJudge,
                score: Some(0.9),
                notes: None,
            },
        ),
    ];

    // Confirm Format::Json parses from the public string form.
    assert_eq!(Format::parse("json"), Some(Format::Json));
    assert_eq!(Format::parse("html"), Some(Format::Html));
    assert_eq!(Format::parse("xml"), None);

    let traj = build_trajectory("session-abc", &branch);
    let json = render_json(&traj);
    let v: Value = serde_json::from_str(&json).expect("valid json");
    assert_eq!(v["session_id"], "session-abc");
    assert_eq!(v["estimated_tokens"], 300);

    let turns = v["turns"].as_array().expect("turns array");
    let mut all_kinds: Vec<String> = Vec::new();
    for t in turns {
        for b in t["blocks"].as_array().unwrap() {
            all_kinds.push(b["kind"].as_str().unwrap().to_string());
        }
    }
    for needle in [
        "meta",
        "user",
        "assistant_text",
        "tool_call",
        "tool_result",
        "outcome",
        "context_load",
    ] {
        assert!(
            all_kinds.iter().any(|k| k == needle),
            "missing kind {needle:?} in {all_kinds:?}"
        );
    }

    // The assistant_text block in the same turn as Usage carries the cost.
    let mut found_cost = false;
    for t in turns {
        for b in t["blocks"].as_array().unwrap() {
            if b["kind"] == "assistant_text" {
                if let Some(c) = b.get("cost_usd").and_then(|v| v.as_f64()) {
                    assert!((c - 0.0042).abs() < 1e-9);
                    found_cost = true;
                }
            }
        }
    }
    assert!(found_cost, "expected cost_usd on assistant_text block");

    // Outcome block exposes structured outcome.
    let outcome = turns
        .iter()
        .flat_map(|t| t["blocks"].as_array().unwrap().iter())
        .find(|b| b["kind"] == "outcome")
        .expect("outcome");
    assert_eq!(outcome["outcome"]["success"], true);
}

#[test]
fn multi_round_turn_attributes_each_usage_to_its_preceding_assistant_text() {
    // RFD bugfix #3: a multi-round turn (user → assistant₁ → tool →
    // result → usage₁ → assistant₂ → tool → result → usage₂ → …)
    // must give each assistant_text block its own usage's cost,
    // not the turn-total back-filled onto every block.
    use pi_coding_agent::native::trajectory::flamegraph::{build_trajectory, render_json};
    use serde_json::Value;

    let mk_round = |n: u32, cost: f64| -> Vec<SessionEntry> {
        vec![
            entry(
                &format!("a{n}"),
                SessionEntryKind::Assistant {
                    message: Message::assistant_text(format!("round {n}").as_str()),
                },
            ),
            entry(
                &format!("tc{n}"),
                SessionEntryKind::ToolCall {
                    call: ToolCall {
                        id: format!("tc{n}"),
                        name: "bash".into(),
                        input: json!({"command": format!("echo {n}")}),
                    },
                },
            ),
            entry(
                &format!("tr{n}"),
                SessionEntryKind::ToolResult {
                    result: ToolResult {
                        tool_use_id: format!("tc{n}"),
                        model_output: format!("out {n}"),
                        display: None,
                        is_error: false,
                    },
                },
            ),
            entry(
                &format!("us{n}"),
                SessionEntryKind::Usage {
                    usage: Usage {
                        input_tokens: 10,
                        output_tokens: 20,
                        cache_read_tokens: 0,
                        cache_write_tokens: 0,
                        reasoning_tokens: 0,
                        cost_usd: cost,
                    },
                },
            ),
        ]
    };

    let mut branch = vec![entry(
        "u1",
        SessionEntryKind::User {
            message: Message::user_text("do a thing"),
        },
    )];
    branch.extend(mk_round(1, 0.01));
    branch.extend(mk_round(2, 0.02));
    branch.extend(mk_round(3, 0.03));

    let traj = build_trajectory("multi-round", &branch);
    let v: Value = serde_json::from_str(&render_json(&traj)).unwrap();
    let turns = v["turns"].as_array().unwrap();
    assert_eq!(turns.len(), 1, "single user → single turn");

    let costs: Vec<f64> = turns[0]["blocks"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|b| b["kind"] == "assistant_text")
        .map(|b| b["cost_usd"].as_f64().expect("cost present"))
        .collect();
    assert_eq!(costs, vec![0.01, 0.02, 0.03]);
}

#[test]
fn assistant_text_without_following_usage_has_no_cost() {
    use pi_coding_agent::native::trajectory::flamegraph::{build_trajectory, render_json};
    use serde_json::Value;

    let branch = vec![
        entry(
            "u1",
            SessionEntryKind::User {
                message: Message::user_text("hi"),
            },
        ),
        entry(
            "a1",
            SessionEntryKind::Assistant {
                message: Message::assistant_text("hello"),
            },
        ),
        // No Usage entry — cost stays None and the field is omitted.
    ];

    let traj = build_trajectory("no-usage", &branch);
    let v: Value = serde_json::from_str(&render_json(&traj)).unwrap();
    let block = v["turns"][0]["blocks"]
        .as_array()
        .unwrap()
        .iter()
        .find(|b| b["kind"] == "assistant_text")
        .unwrap();
    assert!(
        block.get("cost_usd").is_none(),
        "cost_usd should be omitted"
    );
}
