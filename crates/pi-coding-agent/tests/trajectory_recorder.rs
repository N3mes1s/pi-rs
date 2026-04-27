//! Tests for `finalize_session` (G3).
//!
//! Exercises the heuristic-fallback path (no judge configured). The
//! agentic-judge path requires live provider credentials so it's not
//! covered here — provider plumbing is the same as auto_approve::judge
//! which has its own end-to-end tests.

use pi_agent_core::{OutcomeSource, SessionEntryKind, SessionManager};
use pi_ai::{Message, ToolCall, ToolResult};
use pi_coding_agent::native::trajectory::finalize_session;
use serde_json::json;

fn fresh_manager() -> (SessionManager, tempfile::TempDir, tempfile::TempDir) {
    let base = tempfile::tempdir().unwrap();
    let cwd = tempfile::tempdir().unwrap();
    let mgr =
        SessionManager::on_disk(base.path().to_path_buf(), cwd.path().to_path_buf()).unwrap();
    (mgr, base, cwd)
}

#[tokio::test]
async fn empty_session_returns_none() {
    let (mgr, _b, _c) = fresh_manager();
    let meta = mgr.create("anthropic", "sonnet").unwrap();
    // Session has only a Meta entry; no signals.
    let result = finalize_session(&mgr, &meta.id, None).await;
    assert!(result.is_none());
}

#[tokio::test]
async fn fallback_outcome_appended_to_session_jsonl() {
    let (mgr, _b, _c) = fresh_manager();
    let meta = mgr.create("anthropic", "sonnet").unwrap();

    // User asks; agent runs cargo test successfully.
    mgr.append(
        &meta.id,
        SessionEntryKind::User {
            message: Message::user_text("run tests"),
        },
    )
    .unwrap();
    mgr.append(
        &meta.id,
        SessionEntryKind::ToolCall {
            call: ToolCall {
                id: "c1".into(),
                name: "bash".into(),
                input: json!({"command": "cargo test"}),
            },
        },
    )
    .unwrap();
    mgr.append(
        &meta.id,
        SessionEntryKind::ToolResult {
            result: ToolResult {
                tool_use_id: "c1".into(),
                model_output: "ok\n\n[exit 0]".into(),
                display: Some(json!({"kind": "bash", "exit": 0})),
                is_error: false,
            },
        },
    )
    .unwrap();

    // No judge → features-only fallback should fire.
    let outcome = finalize_session(&mgr, &meta.id, None).await.expect("outcome");
    match outcome {
        SessionEntryKind::Outcome { success, source, .. } => {
            assert!(success);
            assert_eq!(source, OutcomeSource::Heuristic);
        }
        _ => panic!("wrong kind"),
    }

    // The Outcome should be the last entry on disk.
    let txt = std::fs::read_to_string(&meta.path).unwrap();
    let last_line = txt.lines().last().unwrap();
    assert!(last_line.contains("\"kind\":\"outcome\""), "last line: {last_line}");
}

#[tokio::test]
async fn finalize_is_idempotent() {
    let (mgr, _b, _c) = fresh_manager();
    let meta = mgr.create("anthropic", "sonnet").unwrap();

    mgr.append(
        &meta.id,
        SessionEntryKind::User {
            message: Message::user_text("run tests"),
        },
    )
    .unwrap();
    mgr.append(
        &meta.id,
        SessionEntryKind::ToolCall {
            call: ToolCall {
                id: "c1".into(),
                name: "bash".into(),
                input: json!({"command": "cargo test"}),
            },
        },
    )
    .unwrap();
    mgr.append(
        &meta.id,
        SessionEntryKind::ToolResult {
            result: ToolResult {
                tool_use_id: "c1".into(),
                model_output: "[exit 0]".into(),
                display: Some(json!({"kind": "bash", "exit": 0})),
                is_error: false,
            },
        },
    )
    .unwrap();

    let first = finalize_session(&mgr, &meta.id, None).await.unwrap();
    // Count outcome entries on disk.
    let count_outcomes = || {
        std::fs::read_to_string(&meta.path)
            .unwrap()
            .lines()
            .filter(|l| l.contains("\"kind\":\"outcome\""))
            .count()
    };
    assert_eq!(count_outcomes(), 1);

    // Second call: should not double-append.
    let second = finalize_session(&mgr, &meta.id, None).await.unwrap();
    assert_eq!(count_outcomes(), 1);

    // Both calls returned the same verdict.
    match (first, second) {
        (
            SessionEntryKind::Outcome { success: a, .. },
            SessionEntryKind::Outcome { success: b, .. },
        ) => assert_eq!(a, b),
        _ => panic!("wrong kinds"),
    }
}

#[tokio::test]
async fn no_signals_no_outcome_no_disk_change() {
    let (mgr, _b, _c) = fresh_manager();
    let meta = mgr.create("anthropic", "sonnet").unwrap();

    // User+assistant, no tool calls — no heuristic signals fire.
    mgr.append(
        &meta.id,
        SessionEntryKind::User {
            message: Message::user_text("hi"),
        },
    )
    .unwrap();
    mgr.append(
        &meta.id,
        SessionEntryKind::Assistant {
            message: Message::assistant_text("hello"),
        },
    )
    .unwrap();

    let before = std::fs::read_to_string(&meta.path).unwrap();
    let result = finalize_session(&mgr, &meta.id, None).await;
    let after = std::fs::read_to_string(&meta.path).unwrap();

    assert!(result.is_none());
    assert_eq!(before, after, "disk should be untouched");
}
