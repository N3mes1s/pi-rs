use pi_agent_core::{AgentEvent, AgentEventKind};
use pi_ai::{ContentBlock, Message, Role, ToolCall, ToolResult, Usage};
use pi_coding_agent::renderer::{Block, Transcript};
use pi_tui::ThemeRegistry;
use serde_json::json;

fn ev(kind: AgentEventKind) -> AgentEvent {
    AgentEvent {
        session_id: "s".into(),
        entry_id: "e".into(),
        timestamp: 0,
        kind,
    }
}

fn theme() -> pi_tui::Theme {
    let reg = ThemeRegistry::new();
    reg.get("dark").cloned().expect("dark theme")
}

#[test]
fn empty_transcript_renders_only_trailing_separator_blank_line() {
    let t = Transcript::default();
    let frame = t.render(&theme(), 80);
    // single trailing separator
    assert_eq!(frame.lines.len(), 1);
    let line = &frame.lines[0];
    assert!(
        line.spans.is_empty(),
        "trailing separator should be blank, got {:?}",
        line
    );
}

#[test]
fn user_message_with_text_adds_user_block() {
    let mut t = Transcript::default();
    t.ingest(&ev(AgentEventKind::UserMessage {
        message: Message {
            role: Role::User,
            content: vec![ContentBlock::Text {
                text: "hello there".into(),
            }],
        },
    }));
    assert_eq!(t.blocks.len(), 1);
    match &t.blocks[0] {
        Block::User(s) => assert_eq!(s, "hello there"),
        other => panic!("expected User block, got {:?}", other),
    }
}

#[test]
fn user_message_without_text_is_ignored() {
    let mut t = Transcript::default();
    t.ingest(&ev(AgentEventKind::UserMessage {
        message: Message {
            role: Role::User,
            content: vec![],
        },
    }));
    assert!(t.blocks.is_empty());
}

#[test]
fn consecutive_assistant_text_deltas_coalesce_into_one_block() {
    let mut t = Transcript::default();
    for chunk in &["Hello, ", "world", "!"] {
        t.ingest(&ev(AgentEventKind::AssistantTextDelta {
            text: (*chunk).into(),
        }));
    }
    assert_eq!(t.blocks.len(), 1, "expected coalesced block");
    match &t.blocks[0] {
        Block::AssistantText(s) => assert_eq!(s, "Hello, world!"),
        other => panic!("expected AssistantText, got {:?}", other),
    }
}

#[test]
fn consecutive_thinking_deltas_coalesce_into_one_block() {
    let mut t = Transcript::default();
    for chunk in &["a", "b", "c"] {
        t.ingest(&ev(AgentEventKind::AssistantThinkingDelta {
            text: (*chunk).into(),
        }));
    }
    assert_eq!(t.blocks.len(), 1);
    match &t.blocks[0] {
        Block::Thinking(s) => assert_eq!(s, "abc"),
        other => panic!("expected Thinking block, got {:?}", other),
    }
}

#[test]
fn tool_call_event_adds_tool_call_block() {
    let mut t = Transcript::default();
    t.ingest(&ev(AgentEventKind::AssistantToolCall {
        call: ToolCall {
            id: "id-1".into(),
            name: "read".into(),
            input: json!({"path": "x"}),
        },
    }));
    assert_eq!(t.blocks.len(), 1);
    match &t.blocks[0] {
        Block::ToolCall { name, input_pretty } => {
            assert_eq!(name, "read");
            assert!(input_pretty.contains("\"path\""));
        }
        other => panic!("expected ToolCall, got {:?}", other),
    }
}

#[test]
fn tool_result_ok_flag_matches_is_error_inverse() {
    let mut t = Transcript::default();
    t.ingest(&ev(AgentEventKind::ToolResult {
        result: ToolResult {
            tool_use_id: "id-1".into(),
            model_output: "line1\nline2".into(),
            display: None,
            is_error: false,
        },
    }));
    t.ingest(&ev(AgentEventKind::ToolResult {
        result: ToolResult {
            tool_use_id: "id-2".into(),
            model_output: "boom".into(),
            display: None,
            is_error: true,
        },
    }));
    assert_eq!(t.blocks.len(), 2);
    match &t.blocks[0] {
        Block::ToolResult { ok, lines, .. } => {
            assert!(*ok);
            assert_eq!(*lines, 2);
        }
        other => panic!("expected ToolResult, got {:?}", other),
    }
    match &t.blocks[1] {
        Block::ToolResult { ok, .. } => assert!(!*ok),
        other => panic!("expected ToolResult, got {:?}", other),
    }
}

#[test]
fn usage_events_accumulate_into_usage_total() {
    let mut t = Transcript::default();
    t.ingest(&ev(AgentEventKind::Usage {
        usage: Usage {
            input_tokens: 10,
            output_tokens: 5,
            cost_usd: 0.01,
            ..Default::default()
        },
    }));
    t.ingest(&ev(AgentEventKind::Usage {
        usage: Usage {
            input_tokens: 3,
            output_tokens: 2,
            cost_usd: 0.02,
            ..Default::default()
        },
    }));
    assert_eq!(t.usage_total.input_tokens, 13);
    assert_eq!(t.usage_total.output_tokens, 7);
    assert!((t.usage_total.cost_usd - 0.03).abs() < 1e-9);
}

#[test]
fn error_event_adds_error_block() {
    let mut t = Transcript::default();
    t.ingest(&ev(AgentEventKind::Error {
        message: "oops".into(),
    }));
    assert_eq!(t.blocks.len(), 1);
    matches!(&t.blocks[0], Block::Error(s) if s == "oops");
}

#[test]
fn compaction_complete_adds_compact_block() {
    let mut t = Transcript::default();
    t.ingest(&ev(AgentEventKind::CompactionComplete {
        summary: "summarised".into(),
        freed_tokens: 2048,
    }));
    assert_eq!(t.blocks.len(), 1);
    match &t.blocks[0] {
        Block::Compact {
            summary,
            freed_tokens,
        } => {
            assert_eq!(summary, "summarised");
            assert_eq!(*freed_tokens, 2048);
        }
        other => panic!("expected Compact block, got {:?}", other),
    }
}

#[test]
fn thinking_collapsed_renders_placeholder_with_char_count() {
    let mut t = Transcript::default();
    t.ingest(&ev(AgentEventKind::AssistantThinkingDelta {
        text: "abcdef".into(),
    }));
    t.thinking_collapsed = true;
    let frame = t.render(&theme(), 80);
    let rendered: String = frame
        .lines
        .iter()
        .flat_map(|l| l.spans.iter())
        .map(|s| s.text.clone())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        rendered.contains("[thinking collapsed: 6 chars]"),
        "got: {rendered:?}"
    );
}

#[test]
fn tool_collapsed_renders_placeholder_with_line_count() {
    let mut t = Transcript::default();
    t.ingest(&ev(AgentEventKind::ToolResult {
        result: ToolResult {
            tool_use_id: "x".into(),
            model_output: "a\nb\nc".into(),
            display: None,
            is_error: false,
        },
    }));
    t.tool_collapsed = true;
    let frame = t.render(&theme(), 80);
    let rendered: String = frame
        .lines
        .iter()
        .flat_map(|l| l.spans.iter())
        .map(|s| s.text.clone())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        rendered.contains("[tool output: 3 lines]"),
        "got: {rendered:?}"
    );
}

#[test]
fn footer_includes_model_tokens_and_cwd() {
    let mut t = Transcript::default();
    t.usage_total.input_tokens = 42;
    t.usage_total.output_tokens = 7;
    let cwd = std::path::PathBuf::from("/tmp/here");
    let line = t.footer(&theme(), "claude-test", &cwd);
    let joined: String = line.spans.iter().map(|s| s.text.clone()).collect();
    assert!(joined.contains("claude-test"));
    assert!(joined.contains("in:42"));
    assert!(joined.contains("out:7"));
    assert!(joined.contains("/tmp/here"));
}

#[test]
fn tail_returns_last_n_blocks_or_everything_when_n_exceeds_len() {
    let mut t = Transcript::default();
    for i in 0..5 {
        t.ingest(&ev(AgentEventKind::AssistantToolCall {
            call: ToolCall {
                id: format!("id-{i}"),
                name: format!("tool-{i}"),
                input: json!({}),
            },
        }));
    }
    let last2 = t.tail(2);
    assert_eq!(last2.len(), 2);
    match &last2[0] {
        Block::ToolCall { name, .. } => assert_eq!(name, "tool-3"),
        other => panic!("got {:?}", other),
    }
    let all = t.tail(99);
    assert_eq!(all.len(), 5);
}

#[test]
fn renderer_wraps_long_lines_to_viewport_width() {
    let mut t = Transcript::default();
    let long = "x".repeat(200);
    t.ingest(&ev(AgentEventKind::AssistantTextDelta { text: long }));
    // Tiny viewport so wrapping is forced.
    let frame = t.render(&theme(), 30);
    // Excluding trailing blank separator.
    let body_lines: Vec<_> = frame.lines.iter().filter(|l| !l.spans.is_empty()).collect();
    assert!(
        body_lines.len() > 1,
        "expected multiple wrapped lines, got {}",
        body_lines.len()
    );
    // No rendered line's joined text should exceed the viewport width
    // by much (allow some leeway for the label prefix on the first line).
    for l in &body_lines {
        let total: usize = l.spans.iter().map(|s| s.text.chars().count()).sum();
        assert!(
            total <= 40,
            "line too wide ({}): {:?}",
            total,
            l.spans.iter().map(|s| &s.text).collect::<Vec<_>>()
        );
    }
}

#[test]
fn render_exercises_user_assistant_thinking_toolcall_toolresult_error_compact_paths() {
    let mut t = Transcript::default();
    t.ingest(&ev(AgentEventKind::UserMessage {
        message: Message {
            role: Role::User,
            content: vec![ContentBlock::Text {
                text: "hello\nlinetwo".into(),
            }],
        },
    }));
    t.ingest(&ev(AgentEventKind::AssistantTextDelta {
        text: "reply".into(),
    }));
    t.ingest(&ev(AgentEventKind::AssistantThinkingDelta {
        text: "thoughts".into(),
    }));
    t.ingest(&ev(AgentEventKind::AssistantToolCall {
        call: ToolCall {
            id: "id".into(),
            name: "read".into(),
            input: json!({"path": "x"}),
        },
    }));
    t.ingest(&ev(AgentEventKind::ToolResult {
        result: ToolResult {
            tool_use_id: "id".into(),
            model_output: "out1\nout2".into(),
            display: None,
            is_error: false,
        },
    }));
    t.ingest(&ev(AgentEventKind::ToolResult {
        result: ToolResult {
            tool_use_id: "id-bad".into(),
            model_output: "boom".into(),
            display: None,
            is_error: true,
        },
    }));
    t.ingest(&ev(AgentEventKind::Error {
        message: "fail".into(),
    }));
    t.ingest(&ev(AgentEventKind::CompactionComplete {
        summary: "compacted".into(),
        freed_tokens: 100,
    }));

    let frame = t.render(&theme(), 80);
    let rendered: String = frame
        .lines
        .iter()
        .flat_map(|l| l.spans.iter())
        .map(|s| s.text.clone())
        .collect::<Vec<_>>()
        .join(" ");
    assert!(rendered.contains("you>"));
    assert!(rendered.contains("pi>"));
    assert!(rendered.contains("thinking>"));
    assert!(rendered.contains("→ read"));
    assert!(rendered.contains("out1"));
    assert!(rendered.contains("[error] fail"));
    assert!(rendered.contains("[compacted ~100 tokens]"));
}

#[test]
fn tool_result_with_more_than_twenty_lines_emits_overflow_marker() {
    let mut t = Transcript::default();
    let body: String = (0..25).map(|i| format!("line-{i}")).collect::<Vec<_>>().join("\n");
    t.ingest(&ev(AgentEventKind::ToolResult {
        result: ToolResult {
            tool_use_id: "id".into(),
            model_output: body,
            display: None,
            is_error: false,
        },
    }));
    let frame = t.render(&theme(), 80);
    let rendered: String = frame
        .lines
        .iter()
        .flat_map(|l| l.spans.iter())
        .map(|s| s.text.clone())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(rendered.contains("(+5 lines)"), "got: {rendered:?}");
}

#[test]
fn assistant_message_records_unseen_tool_calls() {
    let mut t = Transcript::default();
    // No prior ToolCall block; AssistantMessage carries a ToolUse block.
    t.ingest(&ev(AgentEventKind::AssistantMessage {
        message: Message {
            role: Role::Assistant,
            content: vec![ContentBlock::ToolUse {
                id: "id-1".into(),
                name: "edit".into(),
                input: json!({"path": "x"}),
            }],
        },
    }));
    assert_eq!(t.blocks.len(), 1);
    matches!(&t.blocks[0], Block::ToolCall { name, .. } if name == "edit");
}

#[test]
fn assistant_message_does_not_duplicate_already_seen_tool_calls() {
    let mut t = Transcript::default();
    t.ingest(&ev(AgentEventKind::AssistantToolCall {
        call: ToolCall {
            id: "id-1".into(),
            name: "read".into(),
            input: json!({}),
        },
    }));
    t.ingest(&ev(AgentEventKind::AssistantMessage {
        message: Message {
            role: Role::Assistant,
            content: vec![ContentBlock::ToolUse {
                id: "id-1".into(),
                name: "read".into(),
                input: json!({}),
            }],
        },
    }));
    assert_eq!(t.blocks.len(), 1, "should not duplicate the ToolCall");
}
