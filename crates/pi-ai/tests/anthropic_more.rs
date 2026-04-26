//! End-to-end test of an Anthropic streaming `tool_use` flow.
//!
//! We hand-roll the SSE wire format for a single assistant tool_use
//! block and assert the provider surfaces it as one `ToolCall`.

use pi_ai::auth::AuthMethod;
use pi_ai::message::{ContentBlock, FinishReason, Message, Role, ThinkingLevel};
use pi_ai::provider::{AnthropicProvider, GenerateRequest, Provider, ProviderKind};
use pi_ai::registry::{ModelInfo, ProviderConfig};
use wiremock::matchers::{header, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn provider_config(base_url: String) -> ProviderConfig {
    ProviderConfig {
        name: "anthropic".into(),
        kind: ProviderKind::Anthropic,
        base_url,
        auth_header: "x-api-key".into(),
        auth_format: "{token}".into(),
        models: vec![],
    }
}

fn model() -> ModelInfo {
    ModelInfo {
        provider: "anthropic".into(),
        id: "claude-test".into(),
        alias: Some("test".into()),
        context_window: 1024,
        max_output_tokens: 256,
        supports_thinking: false,
        supports_tools: true,
        supports_vision: false,
        input_cost_per_mtok: 0.0,
        output_cost_per_mtok: 0.0,
    }
}

fn tool_use_sse_body() -> String {
    let mut s = String::new();
    s.push_str("event: message_start\n");
    s.push_str("data: {\"type\":\"message_start\",\"message\":{\"id\":\"msg_1\"}}\n\n");

    // tool_use block start
    s.push_str("event: content_block_start\n");
    s.push_str("data: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"tool_use\",\"id\":\"tu_1\",\"name\":\"read\",\"input\":{}}}\n\n");

    // partial input json arriving in deltas
    s.push_str("event: content_block_delta\n");
    s.push_str("data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"input_json_delta\",\"partial_json\":\"{\\\"path\\\":\"}}\n\n");

    s.push_str("event: content_block_delta\n");
    s.push_str("data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"input_json_delta\",\"partial_json\":\"\\\"a.txt\\\"}\"}}\n\n");

    s.push_str("event: content_block_stop\n");
    s.push_str("data: {\"type\":\"content_block_stop\",\"index\":0}\n\n");

    s.push_str("event: message_delta\n");
    s.push_str("data: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"tool_use\"}}\n\n");

    s.push_str("event: message_stop\n");
    s.push_str("data: {\"type\":\"message_stop\"}\n\n");
    s
}

#[tokio::test]
async fn anthropic_generate_surfaces_single_tool_call_from_streaming_tool_use() {
    let server = MockServer::start().await;

    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .and(header("x-api-key", "dummy-key"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_string(tool_use_sse_body()),
        )
        .mount(&server)
        .await;

    let provider = AnthropicProvider::new(
        provider_config(server.uri()),
        AuthMethod::ApiKey {
            value: "dummy-key".into(),
        },
    );

    let req = GenerateRequest {
        model: "claude-test".into(),
        system: None,
        messages: vec![Message {
            role: Role::User,
            content: vec![ContentBlock::Text { text: "go".into() }],
        }],
        tools: vec![],
        thinking: ThinkingLevel::Off,
        temperature: None,
        max_output_tokens: None,
        extras: serde_json::Value::Null,
    };

    let resp = provider.generate(req, &model()).await.expect("generate ok");
    assert_eq!(resp.message.role, Role::Assistant);
    assert_eq!(resp.tool_calls.len(), 1, "expected one tool call");
    let call = &resp.tool_calls[0];
    assert_eq!(call.id, "tu_1");
    assert_eq!(call.name, "read");
    assert_eq!(
        call.input.get("path").and_then(|v| v.as_str()),
        Some("a.txt")
    );
    assert!(matches!(
        resp.finish_reason,
        FinishReason::ToolUse | FinishReason::Stop
    ));
}
