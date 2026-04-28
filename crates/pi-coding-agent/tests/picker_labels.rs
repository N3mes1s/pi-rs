//! Tests for `format_session_label` and `format_tree_entry` in picker.rs.

use pi_agent_core::{SessionEntry, SessionEntryKind, SessionMeta};
use pi_coding_agent::picker::{format_session_label, format_tree_entry};
use std::path::PathBuf;

// ─── helpers ─────────────────────────────────────────────────────────────────

fn make_meta(
    id: &str,
    provider: &str,
    model: &str,
    title: Option<&str>,
    updated_at: i64,
) -> SessionMeta {
    SessionMeta {
        id: id.to_string(),
        path: PathBuf::from("/tmp/test.jsonl"),
        cwd: "/tmp".to_string(),
        provider: provider.to_string(),
        model: model.to_string(),
        title: title.map(|s| s.to_string()),
        created_at: 0,
        updated_at,
    }
}

fn make_entry(id: &str, kind: SessionEntryKind) -> SessionEntry {
    SessionEntry {
        id: id.to_string(),
        parent_id: None,
        timestamp: 0,
        kind,
    }
}

fn user_entry(text: &str) -> SessionEntry {
    make_entry(
        "u1",
        SessionEntryKind::User {
            message: pi_ai::Message::user_text(text),
        },
    )
}

fn assistant_entry(text: &str) -> SessionEntry {
    make_entry(
        "a1",
        SessionEntryKind::Assistant {
            message: pi_ai::Message::assistant_text(text),
        },
    )
}

// ─── format_session_label ────────────────────────────────────────────────────

#[test]
fn session_label_full_case() {
    let meta = make_meta(
        "abcdef1234567890",
        "anthropic",
        "claude-3-5-sonnet",
        Some("My conversation"),
        1_700_000_000_000,
    );
    let label = format_session_label(&meta);
    // short_id is first 8 chars
    assert!(label.starts_with("abcdef12"), "label = {label:?}");
    // provider/model present
    assert!(
        label.contains("anthropic/claude-3-5-sonnet"),
        "label = {label:?}"
    );
    // title present
    assert!(label.contains("My conversation"), "label = {label:?}");
}

#[test]
fn session_label_missing_title_falls_back() {
    let meta = make_meta(
        "abcdef1234567890",
        "openai",
        "gpt-4o",
        None,
        1_700_000_000_000,
    );
    let label = format_session_label(&meta);
    assert!(label.contains("(no title)"), "label = {label:?}");
}

#[test]
fn session_label_empty_title_falls_back() {
    let meta = make_meta(
        "abcdef1234567890",
        "openai",
        "gpt-4o",
        Some(""),
        1_700_000_000_000,
    );
    let label = format_session_label(&meta);
    assert!(label.contains("(no title)"), "label = {label:?}");
}

#[test]
fn session_label_short_id_is_exactly_8_chars() {
    // ID longer than 8 chars — short_id must be truncated to 8.
    let meta = make_meta(
        "0123456789abcdef",
        "anthropic",
        "haiku",
        Some("t"),
        1_700_000_000_000,
    );
    let label = format_session_label(&meta);
    // The very first token (before the first double-space) is the short_id.
    let short_id = label.split("  ").next().unwrap();
    assert_eq!(short_id.len(), 8, "short_id = {short_id:?}");
    assert_eq!(short_id, "01234567");
}

#[test]
fn session_label_short_id_is_8_chars_when_id_is_short() {
    // ID shorter than 8 chars — short_id is the whole id.
    let meta = make_meta("abc", "anthropic", "haiku", Some("t"), 1_700_000_000_000);
    let label = format_session_label(&meta);
    let short_id = label.split("  ").next().unwrap();
    assert_eq!(short_id, "abc");
}

#[test]
fn session_label_timestamp_formatted_correctly() {
    // 1_700_000_000_000 ms  → 1_700_000_000 s
    // chrono: 2023-11-14 22:13 UTC  (verify with: `date -u -d @1700000000`)
    use chrono::{TimeZone, Utc};
    let epoch_ms: i64 = 1_700_000_000_000;
    let expected = Utc
        .timestamp_opt(epoch_ms / 1_000, 0)
        .single()
        .unwrap()
        .format("%Y-%m-%d %H:%M")
        .to_string();

    let meta = make_meta("id12345678", "p", "m", Some("t"), epoch_ms);
    let label = format_session_label(&meta);
    assert!(
        label.contains(&expected),
        "expected timestamp {expected:?} in label {label:?}"
    );
}

// ─── format_tree_entry ───────────────────────────────────────────────────────

#[test]
fn tree_entry_user_kind() {
    let entry = user_entry("hello world");
    let label = format_tree_entry(&entry);
    assert!(label.starts_with("user  "), "label = {label:?}");
    assert!(label.contains("hello world"), "label = {label:?}");
}

#[test]
fn tree_entry_assistant_kind() {
    let entry = assistant_entry("sure, here is the answer");
    let label = format_tree_entry(&entry);
    assert!(label.starts_with("assistant  "), "label = {label:?}");
    assert!(
        label.contains("sure, here is the answer"),
        "label = {label:?}"
    );
}

#[test]
fn tree_entry_tool_call_kind() {
    let call = pi_ai::ToolCall {
        id: "tc1".into(),
        name: "read_file".into(),
        input: serde_json::json!({"path": "/foo"}),
    };
    let entry = make_entry("tc1", SessionEntryKind::ToolCall { call });
    let label = format_tree_entry(&entry);
    assert!(
        label.starts_with("tool_call: read_file"),
        "label = {label:?}"
    );
}

#[test]
fn tree_entry_tool_result_kind() {
    let result = pi_ai::ToolResult {
        tool_use_id: "tc1".into(),
        model_output: "file contents here".into(),
        display: None,
        is_error: false,
    };
    let entry = make_entry("tr1", SessionEntryKind::ToolResult { result });
    let label = format_tree_entry(&entry);
    assert!(label.starts_with("tool_result  "), "label = {label:?}");
    assert!(label.contains("file contents here"), "label = {label:?}");
}

#[test]
fn tree_entry_compaction_kind() {
    let entry = make_entry(
        "comp1",
        SessionEntryKind::Compaction {
            summary: "long conversation compacted".into(),
            replaced_ids: vec!["x".into()],
        },
    );
    let label = format_tree_entry(&entry);
    assert!(label.starts_with("compaction  "), "label = {label:?}");
    assert!(
        label.contains("long conversation compacted"),
        "label = {label:?}"
    );
}

#[test]
fn tree_entry_meta_kind() {
    let entry = make_entry(
        "m1",
        SessionEntryKind::Meta {
            cwd: "/home/user".into(),
            provider: "anthropic".into(),
            model: "sonnet".into(),
            title: None,
        },
    );
    let label = format_tree_entry(&entry);
    // Meta has no text — label is just the kind string.
    assert_eq!(label, "meta", "label = {label:?}");
}

#[test]
fn tree_entry_system_kind() {
    let entry = make_entry(
        "sys1",
        SessionEntryKind::SystemPrompt {
            text: "you are a helpful assistant".into(),
        },
    );
    let label = format_tree_entry(&entry);
    assert!(label.starts_with("system  "), "label = {label:?}");
    assert!(
        label.contains("you are a helpful assistant"),
        "label = {label:?}"
    );
}

#[test]
fn tree_entry_long_message_truncated_to_60_chars() {
    // Build a message text that is definitely longer than 60 characters.
    let long_text: String = "a".repeat(120);
    let entry = user_entry(&long_text);
    let label = format_tree_entry(&entry);
    // "user  " prefix (6 chars) + at most 60 text chars = at most 66 chars total.
    let prefix = "user  ";
    assert!(label.starts_with(prefix), "label = {label:?}");
    let text_part = &label[prefix.len()..];
    assert_eq!(text_part.chars().count(), 60, "text part = {text_part:?}");
}

#[test]
fn tree_entry_newlines_replaced_with_spaces() {
    let entry = user_entry("first line\nsecond line\nthird line");
    let label = format_tree_entry(&entry);
    assert!(
        !label.contains('\n'),
        "label must not contain newlines, got {label:?}"
    );
    assert!(
        label.contains("first line second line"),
        "label = {label:?}"
    );
}

#[test]
fn tree_entry_exactly_60_chars_not_truncated() {
    let text: String = "b".repeat(60);
    let entry = user_entry(&text);
    let label = format_tree_entry(&entry);
    let text_part = label.strip_prefix("user  ").unwrap();
    assert_eq!(text_part.chars().count(), 60);
}
