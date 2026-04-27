//! Tests for `pi_coding_agent::share::render_session_html`.
//!
//! Verifies:
//! - output starts with `<!doctype html>` and contains the session id
//! - HTML escaping: a message containing `<script>` produces `&lt;script&gt;`
//! - each role gets its own div class
//! - empty session still produces a valid document
//! - tool_call div shows tool name in the header
//! - tool_result error flag adds `role-error` class
//! - compaction block is rendered

use pi_agent_core::{SessionEntry, SessionEntryKind};
use pi_ai::{Message, ToolCall, ToolResult};
use pi_coding_agent::share::{html_escape, render_session_html};

// ─── helpers ─────────────────────────────────────────────────────────────────

fn entry(id: &str, kind: SessionEntryKind) -> SessionEntry {
    SessionEntry {
        id: id.into(),
        parent_id: None,
        timestamp: 0,
        kind,
    }
}

// ─── basic structure ─────────────────────────────────────────────────────────

#[test]
fn output_starts_with_doctype_and_contains_session_id() {
    let html = render_session_html(&[], "abc-123-def", "anthropic", "sonnet");
    assert!(
        html.starts_with("<!doctype html>"),
        "must start with <!doctype html>, got: {:?}",
        &html[..50.min(html.len())]
    );
    assert!(
        html.contains("abc-123-def"),
        "must contain the session id"
    );
}

#[test]
fn title_contains_short_id() {
    let html = render_session_html(&[], "deadbeef1234", "openai", "gpt-4o");
    // Short id is first 8 chars.
    assert!(
        html.contains("pi-rs session deadbeef"),
        "title must contain short id"
    );
}

#[test]
fn empty_session_produces_valid_document() {
    let html = render_session_html(&[], "empty-sess", "p", "m");
    assert!(html.contains("<!doctype html>"));
    assert!(html.contains("<html"));
    assert!(html.contains("</html>"));
    assert!(html.contains("<body>"));
    assert!(html.contains("</body>"));
    // No block divs should be present.
    assert!(!html.contains("class=\"block"), "empty session must not have block divs");
}

// ─── HTML escaping ────────────────────────────────────────────────────────────

#[test]
fn html_escape_function_escapes_all_special_chars() {
    assert_eq!(html_escape("<script>"), "&lt;script&gt;");
    assert_eq!(html_escape("a & b"), "a &amp; b");
    assert_eq!(html_escape("\"quoted\""), "&quot;quoted&quot;");
    assert_eq!(html_escape("<>&\""), "&lt;&gt;&amp;&quot;");
    assert_eq!(html_escape("plain"), "plain");
}

#[test]
fn user_message_with_script_tag_is_escaped() {
    let branch = vec![entry(
        "1",
        SessionEntryKind::User {
            message: Message::user_text("<script>alert('xss')</script>"),
        },
    )];
    let html = render_session_html(&branch, "s1", "p", "m");
    assert!(
        html.contains("&lt;script&gt;"),
        "must escape <script>"
    );
    assert!(
        !html.contains("<script>"),
        "must not contain raw <script>"
    );
}

#[test]
fn assistant_message_escapes_angle_brackets_and_ampersand() {
    let branch = vec![entry(
        "2",
        SessionEntryKind::Assistant {
            message: Message::assistant_text("a < b && c > d"),
        },
    )];
    let html = render_session_html(&branch, "s2", "p", "m");
    assert!(html.contains("&lt;"), "must escape <");
    assert!(html.contains("&amp;"), "must escape &");
    assert!(html.contains("&gt;"), "must escape >");
}

// ─── role div classes ─────────────────────────────────────────────────────────

#[test]
fn user_block_has_role_user_class() {
    let branch = vec![entry(
        "1",
        SessionEntryKind::User {
            message: Message::user_text("hello"),
        },
    )];
    let html = render_session_html(&branch, "s", "p", "m");
    assert!(html.contains("role-user"), "must have role-user class");
    assert!(html.contains("<header>user</header>"), "header must say 'user'");
}

#[test]
fn assistant_block_has_role_assistant_class() {
    let branch = vec![entry(
        "1",
        SessionEntryKind::Assistant {
            message: Message::assistant_text("hi"),
        },
    )];
    let html = render_session_html(&branch, "s", "p", "m");
    assert!(html.contains("role-assistant"), "must have role-assistant class");
    assert!(html.contains("<header>assistant</header>"));
}

#[test]
fn tool_call_block_has_role_tool_call_class_and_shows_tool_name() {
    let branch = vec![entry(
        "1",
        SessionEntryKind::ToolCall {
            call: ToolCall {
                id: "tc1".into(),
                name: "shell".into(),
                input: serde_json::json!({"cmd": "ls"}),
            },
        },
    )];
    let html = render_session_html(&branch, "s", "p", "m");
    assert!(html.contains("role-tool_call"), "must have role-tool_call class");
    assert!(
        html.contains("tool_call: shell"),
        "header must show tool name"
    );
}

#[test]
fn tool_result_block_has_role_tool_result_class() {
    let branch = vec![entry(
        "1",
        SessionEntryKind::ToolResult {
            result: ToolResult {
                tool_use_id: "tc1".into(),
                model_output: "file.txt".into(),
                display: None,
                is_error: false,
            },
        },
    )];
    let html = render_session_html(&branch, "s", "p", "m");
    assert!(html.contains("role-tool_result"), "must have role-tool_result class");
    assert!(html.contains("file.txt"));
}

#[test]
fn tool_result_error_adds_role_error_class() {
    let branch = vec![entry(
        "1",
        SessionEntryKind::ToolResult {
            result: ToolResult {
                tool_use_id: "tc1".into(),
                model_output: "something went wrong".into(),
                display: None,
                is_error: true,
            },
        },
    )];
    let html = render_session_html(&branch, "s", "p", "m");
    assert!(html.contains("role-error"), "error result must have role-error class");
}

#[test]
fn compaction_block_has_role_compaction_class() {
    let branch = vec![entry(
        "1",
        SessionEntryKind::Compaction {
            summary: "older context summarised".into(),
            replaced_ids: vec!["a".into()],
        },
    )];
    let html = render_session_html(&branch, "s", "p", "m");
    assert!(html.contains("role-compaction"), "must have role-compaction class");
    assert!(html.contains("older context summarised"));
}

// ─── meta / usage / system-prompt are skipped ────────────────────────────────

#[test]
fn meta_and_usage_and_system_prompt_are_not_rendered() {
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
            "sp",
            SessionEntryKind::SystemPrompt {
                text: "secret system prompt".into(),
            },
        ),
        entry(
            "u",
            SessionEntryKind::Usage {
                usage: pi_ai::Usage::default(),
            },
        ),
        entry(
            "user",
            SessionEntryKind::User {
                message: Message::user_text("visible"),
            },
        ),
    ];
    let html = render_session_html(&branch, "s", "p", "m");
    assert!(!html.contains("secret system prompt"), "system prompt must be hidden");
    assert!(html.contains("visible"), "user message must be present");
    // Only one block div should be in the output (the user message).
    let count = html.matches("class=\"block").count();
    assert_eq!(count, 1, "expected exactly 1 block div, got {count}");
}

// ─── provider / model in document ────────────────────────────────────────────

#[test]
fn provider_and_model_appear_in_document() {
    let html = render_session_html(&[], "sess", "anthropic", "claude-sonnet");
    assert!(html.contains("anthropic"), "provider must appear");
    assert!(html.contains("claude-sonnet"), "model must appear");
}

// ─── ordering ────────────────────────────────────────────────────────────────

#[test]
fn blocks_appear_in_conversation_order() {
    let branch = vec![
        entry(
            "1",
            SessionEntryKind::User {
                message: Message::user_text("first"),
            },
        ),
        entry(
            "2",
            SessionEntryKind::Assistant {
                message: Message::assistant_text("second"),
            },
        ),
        entry(
            "3",
            SessionEntryKind::User {
                message: Message::user_text("third"),
            },
        ),
    ];
    let html = render_session_html(&branch, "s", "p", "m");
    let pos_first = html.find("first").expect("first");
    let pos_second = html.find("second").expect("second");
    let pos_third = html.find("third").expect("third");
    assert!(pos_first < pos_second, "first must appear before second");
    assert!(pos_second < pos_third, "second must appear before third");
}

// ─── tool_call input JSON is escaped ─────────────────────────────────────────

#[test]
fn tool_call_input_with_angle_brackets_is_escaped() {
    let branch = vec![entry(
        "1",
        SessionEntryKind::ToolCall {
            call: ToolCall {
                id: "t".into(),
                name: "read_file".into(),
                input: serde_json::json!({"path": "<root>/file.txt"}),
            },
        },
    )];
    let html = render_session_html(&branch, "s", "p", "m");
    assert!(html.contains("&lt;root&gt;"), "JSON value with < > must be escaped");
}

// ─── session_id shorter than 8 chars ─────────────────────────────────────────

#[test]
fn short_session_id_is_used_verbatim_as_short_id() {
    let html = render_session_html(&[], "abc", "p", "m");
    // Session id shorter than 8 chars — used as-is.
    assert!(html.contains("abc"), "short session id must appear");
}
