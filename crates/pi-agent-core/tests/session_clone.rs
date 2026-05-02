//! Coverage for `SessionManager::clone_branch` — replays all non-Meta
//! entries from the source's active branch into a freshly created session.

use pi_agent_core::{SessionEntryKind, SessionManager};
use pi_ai::{Message, ToolCall, ToolResult};

#[test]
fn clone_branch_returns_new_id_and_replays_entries() {
    let dir = tempfile::tempdir().unwrap();
    let cwd = tempfile::tempdir().unwrap();
    let mgr = SessionManager::on_disk(dir.path().to_path_buf(), cwd.path().to_path_buf()).unwrap();
    let src = mgr.create("anthropic", "sonnet").unwrap();
    mgr.append(
        &src.id,
        SessionEntryKind::User {
            message: Message::user_text("hi"),
        },
    )
    .unwrap();
    mgr.append(
        &src.id,
        SessionEntryKind::Assistant {
            message: Message::assistant_text("hello"),
        },
    )
    .unwrap();
    mgr.append(
        &src.id,
        SessionEntryKind::ToolCall {
            call: ToolCall {
                id: "t1".into(),
                name: "shell".into(),
                input: serde_json::json!({"cmd": "ls"}),
            },
        },
    )
    .unwrap();
    mgr.append(
        &src.id,
        SessionEntryKind::ToolResult {
            result: ToolResult {
                tool_use_id: "t1".into(),
                model_output: "out".into(),
                display: None,
                is_error: false,
            },
        },
    )
    .unwrap();
    mgr.append(
        &src.id,
        SessionEntryKind::Compaction {
            summary: "older messages collapsed".into(),
            replaced_ids: vec![],
        },
    )
    .unwrap();

    let cloned = mgr.clone_branch(&src.id).unwrap();
    assert_ne!(cloned.id, src.id);
    assert_eq!(cloned.provider, "anthropic");
    assert_eq!(cloned.model, "sonnet");

    let new_branch = mgr.current_branch(&cloned.id);
    // 1 Meta + 5 replayed entries.
    assert_eq!(new_branch.len(), 6, "branch was {new_branch:?}");
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
            "assistant",
            "tool_call",
            "tool_result",
            "compaction"
        ]
    );
}

#[test]
fn clone_branch_for_unknown_source_errors() {
    let mgr = SessionManager::in_memory();
    let r = mgr.clone_branch("nope");
    assert!(r.is_err());
}

#[test]
fn clone_branch_inherits_provider_and_model_from_source() {
    let mgr = SessionManager::in_memory();
    let src = mgr.create("openai", "gpt-4o").unwrap();
    mgr.append(
        &src.id,
        SessionEntryKind::User {
            message: Message::user_text("hi"),
        },
    )
    .unwrap();
    let cloned = mgr.clone_branch(&src.id).unwrap();
    assert_eq!(cloned.provider, "openai");
    assert_eq!(cloned.model, "gpt-4o");
    // Distinct id.
    assert_ne!(cloned.id, src.id);
}
