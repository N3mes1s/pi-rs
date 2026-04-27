//! Tests for the agentic outcome judge (G2).
//!
//! Covers:
//! - Feature extraction over a session branch (deterministic).
//! - Verdict JSON parsing (strict, with backtick/prose tolerance).
//! - Heuristic-only fallback that runs when the smol model is unavailable.
//!
//! The Judge::judge integration path (provider HTTP call) is not exercised
//! here — it's a thin wrapper around the auto-approve judge's already-
//! tested provider plumbing. The verdict parser is the security-critical
//! surface and is covered exhaustively.

use pi_agent_core::{OutcomeSource, SessionEntry, SessionEntryKind};
use pi_ai::{Message, ToolCall, ToolResult};
use pi_coding_agent::native::trajectory::{
    extract, features_only_outcome, parse_verdict, JudgeError, JudgeVerdict,
};
use serde_json::json;

fn entry(id: &str, kind: SessionEntryKind) -> SessionEntry {
    SessionEntry {
        id: id.into(),
        parent_id: None,
        timestamp: 0,
        kind,
    }
}

fn user(id: &str, text: &str) -> SessionEntry {
    entry(
        id,
        SessionEntryKind::User {
            message: Message::user_text(text),
        },
    )
}

fn bash_call(call_id: &str, command: &str) -> SessionEntry {
    entry(
        call_id,
        SessionEntryKind::ToolCall {
            call: ToolCall {
                id: call_id.into(),
                name: "bash".into(),
                input: json!({"command": command}),
            },
        },
    )
}

fn bash_result(call_id: &str, exit: i64) -> SessionEntry {
    entry(
        &format!("{call_id}-r"),
        SessionEntryKind::ToolResult {
            result: ToolResult {
                tool_use_id: call_id.into(),
                model_output: format!("output\n\n[exit {exit}]"),
                display: Some(json!({"kind": "bash", "exit": exit})),
                is_error: exit != 0,
            },
        },
    )
}

fn read_call(call_id: &str, path: &str) -> SessionEntry {
    entry(
        call_id,
        SessionEntryKind::ToolCall {
            call: ToolCall {
                id: call_id.into(),
                name: "read".into(),
                input: json!({"file_path": path}),
            },
        },
    )
}

// ─── feature extraction ────────────────────────────────────────────────

#[test]
fn extract_picks_up_test_runs_with_exit_code() {
    let branch = vec![
        user("u1", "run tests"),
        bash_call("c1", "cargo test --workspace"),
        bash_result("c1", 0),
        bash_call("c2", "cargo test"),
        bash_result("c2", 1),
    ];
    let f = extract(&branch);
    assert_eq!(f.test_runs.len(), 2);
    assert_eq!(f.test_runs[0].exit, 0);
    assert_eq!(f.test_runs[1].exit, 1);
}

#[test]
fn extract_separates_test_from_compile() {
    let branch = vec![
        bash_call("c1", "cargo build"),
        bash_result("c1", 0),
        bash_call("c2", "cargo test"),
        bash_result("c2", 0),
    ];
    let f = extract(&branch);
    assert_eq!(f.test_runs.len(), 1);
    assert_eq!(f.compile_runs.len(), 1);
}

#[test]
fn extract_flags_repeated_reads() {
    let branch = vec![
        read_call("c1", "src/main.rs"),
        read_call("c2", "src/main.rs"),
        read_call("c3", "src/main.rs"),
        read_call("c4", "src/main.rs"),
    ];
    let f = extract(&branch);
    assert_eq!(f.repeated_reads.len(), 1);
    assert_eq!(f.repeated_reads[0].count, 4);
    assert!(f.repeated_reads[0].target.contains("src/main.rs"));
}

#[test]
fn extract_marks_recovered_edit() {
    // edit:err, then edit:ok on same path → recovered=true.
    let mk_edit_call = |id: &str| {
        entry(
            id,
            SessionEntryKind::ToolCall {
                call: ToolCall {
                    id: id.into(),
                    name: "edit".into(),
                    input: json!({"path": "src/x.rs"}),
                },
            },
        )
    };
    let mk_edit_err = |id: &str| {
        entry(
            &format!("{id}-r"),
            SessionEntryKind::ToolResult {
                result: ToolResult {
                    tool_use_id: id.into(),
                    model_output: "err".into(),
                    display: None,
                    is_error: true,
                },
            },
        )
    };
    let mk_edit_ok = |id: &str| {
        entry(
            &format!("{id}-r"),
            SessionEntryKind::ToolResult {
                result: ToolResult {
                    tool_use_id: id.into(),
                    model_output: "ok".into(),
                    display: None,
                    is_error: false,
                },
            },
        )
    };
    let branch = vec![
        mk_edit_call("c1"),
        mk_edit_err("c1"),
        mk_edit_call("c2"),
        mk_edit_ok("c2"),
    ];
    let f = extract(&branch);
    assert_eq!(f.edit_errors.len(), 1);
    assert!(f.edit_errors[0].recovered, "should be marked recovered");
}

#[test]
fn extract_counts_turns() {
    let branch = vec![
        user("u1", "do stuff"),
        bash_call("c1", "ls"),
        bash_result("c1", 0),
        entry(
            "a1",
            SessionEntryKind::Assistant {
                message: Message::assistant_text("done"),
            },
        ),
    ];
    let f = extract(&branch);
    assert_eq!(f.turn_counts.user, 1);
    assert_eq!(f.turn_counts.assistant, 1);
    assert_eq!(f.turn_counts.tool_calls, 1);
    assert_eq!(f.turn_counts.tool_results, 1);
    assert_eq!(f.turn_counts.tool_errors, 0);
}

// ─── features-only outcome ─────────────────────────────────────────────

#[test]
fn no_signals_yields_no_outcome() {
    let branch = vec![user("u1", "hi")];
    let f = extract(&branch);
    assert!(features_only_outcome(&f).is_none());
}

#[test]
fn passing_tests_produce_heuristic_win() {
    let branch = vec![
        user("u1", "run tests"),
        bash_call("c1", "cargo test"),
        bash_result("c1", 0),
    ];
    let f = extract(&branch);
    let oc = features_only_outcome(&f).unwrap();
    match oc {
        SessionEntryKind::Outcome { success, source, score, .. } => {
            assert!(success);
            assert_eq!(source, OutcomeSource::Heuristic);
            assert!(score.unwrap() > 0.9);
        }
        _ => panic!("wrong kind"),
    }
}

#[test]
fn failing_tests_produce_heuristic_loss() {
    let branch = vec![
        user("u1", "run tests"),
        bash_call("c1", "cargo test"),
        bash_result("c1", 1),
    ];
    let f = extract(&branch);
    let oc = features_only_outcome(&f).unwrap();
    match oc {
        SessionEntryKind::Outcome { success, score, .. } => {
            assert!(!success);
            // Failing tests + ending-on-error termination → mean ~ -0.65,
            // which maps to ~0.175 in unit range. Anywhere in the lower
            // third counts as a clear loss.
            assert!(score.unwrap() < 0.3, "score = {score:?}");
        }
        _ => panic!("wrong kind"),
    }
}

// ─── verdict parsing ───────────────────────────────────────────────────

#[test]
fn parse_clean_verdict_json() {
    let v = parse_verdict(
        r#"{"success":true,"score":0.85,"reason":"task done","salient_wins":["tests pass"],"salient_failures":[]}"#,
    )
    .unwrap();
    assert!(v.success);
    assert!((v.score - 0.85).abs() < 1e-6);
    assert_eq!(v.salient_wins, vec!["tests pass"]);
    assert!(v.salient_failures.is_empty());
}

#[test]
fn parse_with_prose_and_backticks() {
    let v = parse_verdict(
        "Here is my verdict:\n```json\n{\"success\": false, \"score\": 0.7, \"reason\": \"agent looped\"}\n```\n",
    )
    .unwrap();
    assert!(!v.success);
    assert_eq!(v.reason, "agent looped");
}

#[test]
fn missing_score_field_is_bad_response() {
    // success+reason but no score -> serde uses default... but our struct
    // doesn't default score, so this should fail.
    let err = parse_verdict(r#"{"success":true,"reason":"ok"}"#).unwrap_err();
    assert!(matches!(err, JudgeError::BadResponse(_)));
}

#[test]
fn parse_clamps_score_to_unit_range() {
    let v = parse_verdict(r#"{"success":true,"score":1.7,"reason":"x"}"#).unwrap();
    assert_eq!(v.score, 1.0);
    let v = parse_verdict(r#"{"success":true,"score":-0.3,"reason":"x"}"#).unwrap();
    assert_eq!(v.score, 0.0);
}

#[test]
fn empty_optional_arrays_default() {
    let v = parse_verdict(r#"{"success":true,"score":0.8,"reason":"x"}"#).unwrap();
    assert!(v.salient_wins.is_empty());
    assert!(v.salient_failures.is_empty());
}

#[test]
fn not_json_is_bad_response() {
    let err = parse_verdict("approve, looks good").unwrap_err();
    assert!(matches!(err, JudgeError::BadResponse(_)));
}

#[test]
fn verdict_serialisation_round_trips() {
    let v = JudgeVerdict {
        success: true,
        score: 0.75,
        reason: "done".into(),
        salient_wins: vec!["compiled".into()],
        salient_failures: vec![],
    };
    let json = serde_json::to_string(&v).unwrap();
    let back: JudgeVerdict = serde_json::from_str(&json).unwrap();
    assert_eq!(v, back);
}
