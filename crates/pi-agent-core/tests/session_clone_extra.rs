//! Extra coverage for `SessionManager::clone_branch`.
//!
//! The happy path is in `session_clone.rs`. This file exercises:
//!   - cloning a branch that interleaves several Tool/ToolResult pairs;
//!   - cloning a branch whose tail is a Compaction entry;
//!   - cloning an unknown / never-opened session id (must Err).

use pi_agent_core::{SessionEntryKind, SessionManager};
use pi_ai::{Message, ToolCall, ToolResult};

#[test]
fn clone_branch_replays_multiple_tool_pairs_in_order() {
    let mgr = SessionManager::in_memory();
    let src = mgr.create("anthropic", "sonnet").unwrap();

    // user → tool → tool_result → assistant → tool → tool_result
    mgr.append(
        &src.id,
        SessionEntryKind::User {
            message: Message::user_text("run two tools"),
        },
    )
    .unwrap();

    for tag in ["a", "b"] {
        mgr.append(
            &src.id,
            SessionEntryKind::ToolCall {
                call: ToolCall {
                    id: format!("t-{tag}"),
                    name: "shell".into(),
                    input: serde_json::json!({"cmd": tag}),
                },
            },
        )
        .unwrap();
        mgr.append(
            &src.id,
            SessionEntryKind::ToolResult {
                result: ToolResult {
                    tool_use_id: format!("t-{tag}"),
                    model_output: format!("out-{tag}"),
                    display: None,
                    is_error: false,
                },
            },
        )
        .unwrap();
    }
    mgr.append(
        &src.id,
        SessionEntryKind::Assistant {
            message: Message::assistant_text("done"),
        },
    )
    .unwrap();

    let cloned = mgr.clone_branch(&src.id).unwrap();
    let new_branch = mgr.current_branch(&cloned.id);
    let kinds: Vec<&str> = new_branch
        .iter()
        .map(|e| match &e.kind {
            SessionEntryKind::Meta { .. } => "meta",
            SessionEntryKind::User { .. } => "user",
            SessionEntryKind::Assistant { .. } => "assistant",
            SessionEntryKind::ToolCall { .. } => "tool_call",
            SessionEntryKind::ToolResult { .. } => "tool_result",
            SessionEntryKind::Compaction { .. } => "compaction",
            SessionEntryKind::SystemPrompt { .. } => "system_prompt",
            SessionEntryKind::Usage { .. } => "usage",
            SessionEntryKind::ContextLoad { .. } => "context_load",
            SessionEntryKind::Outcome { .. } => "outcome",
            SessionEntryKind::EvolveMarker { .. } => "evolve_marker",
            SessionEntryKind::RoutingDecision { .. } => "routing_decision",
            SessionEntryKind::SandboxAction { .. } => "sandbox_action",
        })
        .collect();
    assert_eq!(
        kinds,
        vec![
            "meta",
            "user",
            "tool_call",
            "tool_result",
            "tool_call",
            "tool_result",
            "assistant",
        ]
    );
    // Tool call inputs survive the round trip.
    let mut tool_inputs: Vec<String> = new_branch
        .iter()
        .filter_map(|e| match &e.kind {
            SessionEntryKind::ToolCall { call } => call
                .input
                .get("cmd")
                .and_then(|v| v.as_str())
                .map(String::from),
            _ => None,
        })
        .collect();
    tool_inputs.sort();
    assert_eq!(tool_inputs, vec!["a".to_string(), "b".to_string()]);
}

#[test]
fn clone_branch_replays_terminal_compaction_entry() {
    let mgr = SessionManager::in_memory();
    let src = mgr.create("openai", "gpt-4o-mini").unwrap();
    mgr.append(
        &src.id,
        SessionEntryKind::User {
            message: Message::user_text("a long thread"),
        },
    )
    .unwrap();
    mgr.append(
        &src.id,
        SessionEntryKind::Compaction {
            summary: "earlier turns collapsed for context".into(),
            replaced_ids: vec!["x".into(), "y".into(), "z".into()],
        },
    )
    .unwrap();

    let cloned = mgr.clone_branch(&src.id).unwrap();
    assert_eq!(cloned.provider, "openai");
    assert_eq!(cloned.model, "gpt-4o-mini");

    let new_branch = mgr.current_branch(&cloned.id);
    // Last replayed entry must be a Compaction, with the summary intact.
    let last = new_branch.last().expect("non-empty branch");
    match &last.kind {
        SessionEntryKind::Compaction {
            summary,
            replaced_ids,
        } => {
            assert_eq!(summary, "earlier turns collapsed for context");
            assert_eq!(replaced_ids.len(), 3);
        }
        other => panic!("expected Compaction tail, got {other:?}"),
    }
}

#[test]
fn clone_branch_for_unknown_id_returns_err() {
    let mgr = SessionManager::in_memory();
    let r = mgr.clone_branch("does-not-exist");
    assert!(r.is_err(), "expected Err for unknown id, got {r:?}");
}
