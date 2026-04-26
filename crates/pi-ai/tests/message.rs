use pi_ai::message::{ContentBlock, Message, Role, Usage};

#[test]
fn user_text_builds_a_user_role_with_text_block() {
    let m = Message::user_text("hello");
    assert_eq!(m.role, Role::User);
    assert_eq!(m.content.len(), 1);
    match &m.content[0] {
        ContentBlock::Text { text } => assert_eq!(text, "hello"),
        _ => panic!("expected Text"),
    }
    assert_eq!(m.text(), "hello");
}

#[test]
fn assistant_text_builds_an_assistant_role_with_text_block() {
    let m = Message::assistant_text("hi back");
    assert_eq!(m.role, Role::Assistant);
    assert_eq!(m.content.len(), 1);
    match &m.content[0] {
        ContentBlock::Text { text } => assert_eq!(text, "hi back"),
        _ => panic!("expected Text"),
    }
}

#[test]
fn system_text_builds_a_system_role_with_text_block() {
    let m = Message::system_text("be helpful");
    assert_eq!(m.role, Role::System);
    assert_eq!(m.content.len(), 1);
    match &m.content[0] {
        ContentBlock::Text { text } => assert_eq!(text, "be helpful"),
        _ => panic!("expected Text"),
    }
}

#[test]
fn usage_default_is_all_zeros() {
    let u = Usage::default();
    assert_eq!(u.input_tokens, 0);
    assert_eq!(u.output_tokens, 0);
    assert_eq!(u.cache_read_tokens, 0);
    assert_eq!(u.cache_write_tokens, 0);
    assert_eq!(u.reasoning_tokens, 0);
    assert_eq!(u.cost_usd, 0.0);
}

#[test]
fn text_concatenates_only_text_blocks() {
    let m = Message {
        role: Role::Assistant,
        content: vec![
            ContentBlock::Thinking {
                text: "ignored".into(),
                signature: None,
            },
            ContentBlock::Text { text: "hello ".into() },
            ContentBlock::ToolUse {
                id: "tu1".into(),
                name: "fs_read".into(),
                input: serde_json::json!({}),
            },
            ContentBlock::Text { text: "world".into() },
            ContentBlock::ToolResult {
                tool_use_id: "tu1".into(),
                content: "should-not-appear".into(),
                is_error: false,
            },
        ],
    };
    assert_eq!(m.text(), "hello world");
}

#[test]
fn text_on_message_without_text_blocks_returns_empty_string() {
    let m = Message {
        role: Role::User,
        content: vec![ContentBlock::ToolUse {
            id: "x".into(),
            name: "y".into(),
            input: serde_json::Value::Null,
        }],
    };
    assert_eq!(m.text(), "");
}
