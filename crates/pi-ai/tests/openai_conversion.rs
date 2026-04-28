use pi_ai::message::{ContentBlock, Message, Role};
use pi_ai::provider::openai::message_to_openai;
use serde_json::json;

#[test]
fn assistant_message_with_only_text() {
    let m = Message::assistant_text("hello there");
    let out = message_to_openai(&m);
    assert_eq!(out.len(), 1);
    assert_eq!(
        out[0],
        json!({
            "role": "assistant",
            "content": "hello there",
        })
    );
}

#[test]
fn assistant_message_with_tool_use_only_has_null_content_and_tool_calls() {
    let m = Message {
        role: Role::Assistant,
        content: vec![ContentBlock::ToolUse {
            id: "call_1".into(),
            name: "fs_read".into(),
            input: json!({"path": "/etc/hosts"}),
        }],
    };
    let out = message_to_openai(&m);
    assert_eq!(out.len(), 1);
    assert_eq!(out[0]["role"], json!("assistant"));
    assert_eq!(out[0]["content"], serde_json::Value::Null);
    let tool_calls = out[0]["tool_calls"].as_array().expect("tool_calls array");
    assert_eq!(tool_calls.len(), 1);
    assert_eq!(tool_calls[0]["id"], json!("call_1"));
    assert_eq!(tool_calls[0]["type"], json!("function"));
    assert_eq!(tool_calls[0]["function"]["name"], json!("fs_read"));
    // arguments is the JSON-stringified input object.
    let args = tool_calls[0]["function"]["arguments"].as_str().unwrap();
    let parsed: serde_json::Value = serde_json::from_str(args).unwrap();
    assert_eq!(parsed, json!({"path": "/etc/hosts"}));
}

#[test]
fn user_message_with_tool_result_emits_tool_role_only() {
    let m = Message {
        role: Role::User,
        content: vec![ContentBlock::ToolResult {
            tool_use_id: "call_1".into(),
            content: "127.0.0.1 localhost".into(),
            is_error: false,
        }],
    };
    let out = message_to_openai(&m);
    assert_eq!(out.len(), 1);
    assert_eq!(
        out[0],
        json!({
            "role": "tool",
            "tool_call_id": "call_1",
            "content": "127.0.0.1 localhost",
        })
    );
}

#[test]
fn user_message_with_thinking_block_wraps_in_thinking_tags() {
    let m = Message {
        role: Role::User,
        content: vec![ContentBlock::Thinking {
            text: "hmm".into(),
            signature: None,
        }],
    };
    let out = message_to_openai(&m);
    assert_eq!(out.len(), 1);
    assert_eq!(
        out[0],
        json!({
            "role": "user",
            "content": "<thinking>hmm</thinking>\n",
        })
    );
}

#[test]
fn assistant_with_text_and_tool_use_produces_one_message_with_both() {
    let m = Message {
        role: Role::Assistant,
        content: vec![
            ContentBlock::Text {
                text: "looking up...".into(),
            },
            ContentBlock::ToolUse {
                id: "c2".into(),
                name: "search".into(),
                input: json!({"q": "rust"}),
            },
        ],
    };
    let out = message_to_openai(&m);
    assert_eq!(out.len(), 1);
    assert_eq!(out[0]["role"], json!("assistant"));
    assert_eq!(out[0]["content"], json!("looking up..."));
    assert_eq!(out[0]["tool_calls"][0]["function"]["name"], json!("search"));
}
