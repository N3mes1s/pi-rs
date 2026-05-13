// TODO(rfd-0019): re-enable after responses-core merges.
//
// Asserts the JSON request body produced for a Responses-API call
// matches the golden fixture in
// `tests/data/openai_responses/request_gpt54_basic.json`. Depends on
// `pi_ai::provider::openai_responses::build_request_body` (or
// equivalent) which lands with claude/responses-core.

#![cfg(rfd_0019_responses)]
#![allow(dead_code)]

use pi_ai::message::{Message, ThinkingLevel};
use pi_ai::provider::openai_responses::build_request_body;
use pi_ai::provider::GenerateRequest;
use pi_ai::tool::ToolSpec;

const GOLDEN: &str = include_str!("data/openai_responses/request_gpt54_basic.json");

fn fixture_request() -> GenerateRequest {
    GenerateRequest {
        model: "gpt-5.4".into(),
        system: None,
        messages: vec![Message::user_text("hi")],
        tools: vec![ToolSpec {
            name: "echo_tool".into(),
            description: "Echo a message back.".into(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "message": { "type": "string" }
                },
                "required": ["message"]
            }),
        }],
        thinking: ThinkingLevel::Medium,
        temperature: None,
        max_output_tokens: Some(256),
        extras: serde_json::Value::Null,
    }
}

#[test]
fn request_body_matches_golden() {
    let body = build_request_body(&fixture_request());
    let expected: serde_json::Value = serde_json::from_str(GOLDEN).expect("golden parse ok");
    assert_eq!(body, expected, "request body diverged from golden");
}

#[test]
fn input_items_use_input_text_not_text() {
    let body = build_request_body(&fixture_request());
    let input = body
        .get("input")
        .and_then(|v| v.as_array())
        .expect("input array");
    assert_eq!(input.len(), 1);
    let user = &input[0];
    assert_eq!(user["role"], "user");
    let content = user["content"].as_array().expect("content array");
    assert_eq!(content[0]["type"], "input_text");
    assert_eq!(content[0]["text"], "hi");
}

#[test]
fn tool_is_flat_no_function_wrapper() {
    let body = build_request_body(&fixture_request());
    let tool = &body["tools"][0];

    assert_eq!(tool["type"], "function");
    assert_eq!(tool["name"], "echo_tool");
    assert_eq!(tool["description"], "Echo a message back.");
    assert!(
        tool.get("parameters").is_some(),
        "expected flat `parameters` field"
    );
    assert_eq!(tool["strict"], serde_json::Value::Bool(true));
    assert!(
        tool.get("function").is_none(),
        "Responses tools must be flat: no nested `function: {{…}}` wrapper"
    );
}

#[test]
fn reasoning_block_is_present_with_effort_and_summary() {
    let body = build_request_body(&fixture_request());
    let reasoning = body.get("reasoning").expect("reasoning block present");
    assert_eq!(reasoning["effort"], "medium");
    assert_eq!(reasoning["summary"], "auto");
}

#[test]
fn include_carries_encrypted_reasoning() {
    let body = build_request_body(&fixture_request());
    let include = body
        .get("include")
        .and_then(|v| v.as_array())
        .expect("include is array");
    assert!(
        include.iter().any(|v| v == "reasoning.encrypted_content"),
        "expected `reasoning.encrypted_content` in include"
    );
}

#[test]
fn uses_max_output_tokens_not_max_tokens() {
    let body = build_request_body(&fixture_request());
    assert_eq!(body["max_output_tokens"], 256);
    assert!(body.get("max_tokens").is_none(), "no legacy max_tokens");
    assert!(
        body.get("max_completion_tokens").is_none(),
        "max_completion_tokens belongs to Chat Completions, not Responses"
    );
}

#[test]
fn store_is_false_by_default() {
    let body = build_request_body(&fixture_request());
    assert_eq!(body["store"], serde_json::Value::Bool(false));
}
