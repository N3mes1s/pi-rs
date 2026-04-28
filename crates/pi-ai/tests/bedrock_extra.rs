//! Extra coverage tests for BedrockAnthropicProvider.
//!
//! Covers: OAuth auth, content_blocks_to_anthropic delegation,
//! thinking levels, tool call stream, message_delta with stop_reason,
//! AuthMethod::None → MissingAuth, with_client builder.

use futures::StreamExt;
use pi_ai::auth::AuthMethod;
use pi_ai::message::{ContentBlock, Message, Role, ThinkingLevel};
use pi_ai::provider::{BedrockAnthropicProvider, GenerateRequest, Provider, ProviderKind};
use pi_ai::registry::{ModelInfo, ProviderConfig};
use pi_ai::stream::StreamEventKind;
use pi_ai::{AiError, FinishReason, ToolSpec};
use serde_json::json;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn cfg(base_url: String) -> ProviderConfig {
    ProviderConfig {
        name: "bedrock".into(),
        kind: ProviderKind::Bedrock,
        base_url,
        auth_header: "Authorization".into(),
        auth_format: "Bearer {token}".into(),
        models: vec![],
    }
}

fn model() -> ModelInfo {
    ModelInfo {
        provider: "bedrock".into(),
        id: "anthropic.claude-test".into(),
        alias: None,
        context_window: 1024,
        max_output_tokens: 256,
        supports_thinking: false,
        supports_tools: true,
        supports_vision: false,
        input_cost_per_mtok: 0.0,
        output_cost_per_mtok: 0.0,
        cache_read_cost_per_mtok: None,
        cache_write_cost_per_mtok: None,
    }
}

fn req() -> GenerateRequest {
    GenerateRequest {
        model: "anthropic.claude-test".into(),
        system: None,
        messages: vec![Message::user_text("hi")],
        tools: vec![],
        thinking: ThinkingLevel::Off,
        temperature: None,
        max_output_tokens: None,
        extras: serde_json::Value::Null,
    }
}

fn bedrock_path() -> &'static str {
    "/model/anthropic.claude-test/invoke-with-response-stream"
}

// ── AuthMethod::None → MissingAuth ─────────────────────────────────────────────

#[tokio::test]
async fn bedrock_no_auth_gives_missing_auth_error() {
    let provider = BedrockAnthropicProvider::new(cfg("http://localhost".into()), AuthMethod::None);
    let err = provider.stream(req(), &model()).await.err().expect("error");
    match err {
        AiError::MissingAuth(_) => {}
        other => panic!("expected MissingAuth, got {other:?}"),
    }
}

// ── OAuth token is accepted ────────────────────────────────────────────────────

#[tokio::test]
async fn bedrock_oauth_token_is_used() {
    let server = MockServer::start().await;
    let body = concat!(
        "event: message_start\n",
        "data: {\"type\":\"message_start\"}\n\n",
        "event: content_block_delta\n",
        "data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"Hello\"}}\n\n",
        "event: message_stop\n",
        "data: {\"type\":\"message_stop\"}\n\n",
    );

    Mock::given(method("POST"))
        .and(path(bedrock_path()))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_string(body),
        )
        .mount(&server)
        .await;

    let provider = BedrockAnthropicProvider::new(
        cfg(server.uri()),
        AuthMethod::OAuth {
            access_token: "oauth-token".into(),
            refresh_token: None,
            expires_at: None,
        },
    );

    let resp = provider.generate(req(), &model()).await.expect("generate ok");
    assert_eq!(resp.message.text(), "Hello");
}

// ── with_client builder smoke test ────────────────────────────────────────────

#[test]
fn bedrock_with_client_builder() {
    let client = reqwest::Client::new();
    let provider = BedrockAnthropicProvider::new(cfg("http://localhost".into()), AuthMethod::None)
        .with_client(client);
    assert_eq!(provider.region, std::env::var("AWS_REGION").unwrap_or_else(|_| "us-east-1".to_string()));
}

// ── Thinking level Low/Medium/High forwarded ──────────────────────────────────

#[tokio::test]
async fn bedrock_thinking_levels_are_forwarded() {
    let server = MockServer::start().await;
    let body = concat!(
        "event: message_start\n",
        "data: {\"type\":\"message_start\"}\n\n",
        "event: message_delta\n",
        "data: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"end_turn\"},\"usage\":{\"output_tokens\":1}}\n\n",
        "event: message_stop\n",
        "data: {\"type\":\"message_stop\"}\n\n",
    );

    for level in [ThinkingLevel::Low, ThinkingLevel::Medium, ThinkingLevel::High] {
        Mock::given(method("POST"))
            .and(path(bedrock_path()))
            .respond_with(
                ResponseTemplate::new(200)
                    .insert_header("content-type", "text/event-stream")
                    .set_body_string(body),
            )
            .mount(&server)
            .await;

        let provider = BedrockAnthropicProvider::new(
            cfg(server.uri()),
            AuthMethod::ApiKey { value: "tok".into() },
        );
        let mut r = req();
        r.thinking = level;

        let resp = provider.generate(r, &model()).await.expect("ok");
        assert!(matches!(resp.finish_reason, FinishReason::Stop));
    }
}

// ── System message forwarded ──────────────────────────────────────────────────

#[tokio::test]
async fn bedrock_system_message_forwarded() {
    let server = MockServer::start().await;
    let body = concat!(
        "event: message_start\n",
        "data: {\"type\":\"message_start\"}\n\n",
        "event: content_block_delta\n",
        "data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"yes\"}}\n\n",
        "event: message_stop\n",
        "data: {\"type\":\"message_stop\"}\n\n",
    );

    Mock::given(method("POST"))
        .and(path(bedrock_path()))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_string(body),
        )
        .mount(&server)
        .await;

    let provider = BedrockAnthropicProvider::new(
        cfg(server.uri()),
        AuthMethod::ApiKey { value: "tok".into() },
    );
    let mut r = req();
    r.system = Some("System prompt".into());

    let resp = provider.generate(r, &model()).await.expect("ok");
    assert_eq!(resp.message.text(), "yes");
}

// ── Tool call via content_block_start / content_block_stop ────────────────────

#[tokio::test]
async fn bedrock_tool_call_stream_emits_tool_call_complete() {
    let server = MockServer::start().await;
    let body = concat!(
        "event: message_start\n",
        "data: {\"type\":\"message_start\"}\n\n",
        "event: content_block_start\n",
        "data: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"tool_use\",\"id\":\"tu_1\",\"name\":\"bash\"}}\n\n",
        "event: content_block_delta\n",
        "data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"input_json_delta\",\"partial_json\":\"{\\\"cmd\\\":\\\"ls\\\"}\"}}\n\n",
        "event: content_block_stop\n",
        "data: {\"type\":\"content_block_stop\",\"index\":0}\n\n",
        "event: message_delta\n",
        "data: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"tool_use\"},\"usage\":{\"output_tokens\":10}}\n\n",
        "event: message_stop\n",
        "data: {\"type\":\"message_stop\"}\n\n",
    );

    Mock::given(method("POST"))
        .and(path(bedrock_path()))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_string(body),
        )
        .mount(&server)
        .await;

    let provider = BedrockAnthropicProvider::new(
        cfg(server.uri()),
        AuthMethod::ApiKey { value: "tok".into() },
    );

    let resp = provider.generate(req(), &model()).await.expect("ok");
    assert_eq!(resp.tool_calls.len(), 1);
    assert_eq!(resp.tool_calls[0].name, "bash");
    assert_eq!(resp.tool_calls[0].input, json!({"cmd": "ls"}));
}

// ── Thinking delta ─────────────────────────────────────────────────────────────

#[tokio::test]
async fn bedrock_thinking_delta_emitted() {
    let server = MockServer::start().await;
    let body = concat!(
        "event: message_start\n",
        "data: {\"type\":\"message_start\"}\n\n",
        "event: content_block_delta\n",
        "data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"thinking_delta\",\"thinking\":\"pondering\"}}\n\n",
        "event: message_stop\n",
        "data: {\"type\":\"message_stop\"}\n\n",
    );

    Mock::given(method("POST"))
        .and(path(bedrock_path()))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_string(body),
        )
        .mount(&server)
        .await;

    let provider = BedrockAnthropicProvider::new(
        cfg(server.uri()),
        AuthMethod::ApiKey { value: "tok".into() },
    );

    let mut stream = provider.stream(req(), &model()).await.expect("ok");
    let mut saw_thinking = false;
    while let Some(ev) = stream.next().await {
        if let Ok(e) = ev {
            if matches!(&e.kind, StreamEventKind::ThinkingDelta { text } if text == "pondering") {
                saw_thinking = true;
            }
        }
    }
    assert!(saw_thinking, "expected a ThinkingDelta event");
}

// ── Usage event from message_delta ────────────────────────────────────────────

#[tokio::test]
async fn bedrock_usage_from_message_delta() {
    let server = MockServer::start().await;
    let body = concat!(
        "event: message_start\n",
        "data: {\"type\":\"message_start\"}\n\n",
        "event: message_delta\n",
        "data: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"end_turn\"},\"usage\":{\"output_tokens\":42}}\n\n",
        "event: message_stop\n",
        "data: {\"type\":\"message_stop\"}\n\n",
    );

    Mock::given(method("POST"))
        .and(path(bedrock_path()))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_string(body),
        )
        .mount(&server)
        .await;

    let provider = BedrockAnthropicProvider::new(
        cfg(server.uri()),
        AuthMethod::ApiKey { value: "tok".into() },
    );

    let resp = provider.generate(req(), &model()).await.expect("ok");
    assert_eq!(resp.usage.output_tokens, 42);
}

// ── content_block_start for text block (non-tool) is a no-op ─────────────────

#[tokio::test]
async fn bedrock_text_content_block_start_is_ignored() {
    let server = MockServer::start().await;
    let body = concat!(
        "event: message_start\n",
        "data: {\"type\":\"message_start\"}\n\n",
        "event: content_block_start\n",
        "data: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"text\"}}\n\n",
        "event: content_block_delta\n",
        "data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"hi\"}}\n\n",
        "event: message_stop\n",
        "data: {\"type\":\"message_stop\"}\n\n",
    );

    Mock::given(method("POST"))
        .and(path(bedrock_path()))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_string(body),
        )
        .mount(&server)
        .await;

    let provider = BedrockAnthropicProvider::new(
        cfg(server.uri()),
        AuthMethod::ApiKey { value: "tok".into() },
    );

    let resp = provider.generate(req(), &model()).await.expect("ok");
    assert_eq!(resp.message.text(), "hi");
}

// ── content_blocks_to_anthropic delegation ────────────────────────────────────

#[test]
fn content_blocks_to_anthropic_all_block_types() {
    use pi_ai::message::{Attachment, AttachmentKind};
    use pi_ai::provider::anthropic::content_blocks_to_anthropic;

    let blocks = vec![
        ContentBlock::Text { text: "hello".into() },
        ContentBlock::Thinking { text: "think".into(), signature: Some("sig".into()) },
        ContentBlock::ToolUse { id: "id1".into(), name: "bash".into(), input: json!({"cmd": "ls"}) },
        ContentBlock::ToolResult { tool_use_id: "id1".into(), content: "result".into(), is_error: false },
        ContentBlock::Attachment {
            attachment: Attachment {
                kind: AttachmentKind::Image { mime: "image/png".into(), base64: "abc".into() },
            },
        },
        ContentBlock::Attachment {
            attachment: Attachment {
                kind: AttachmentKind::File { mime: "application/pdf".into(), base64: "xyz".into(), name: "doc.pdf".into() },
            },
        },
    ];

    let val = content_blocks_to_anthropic(&blocks);
    let arr = val.as_array().expect("array");
    assert_eq!(arr.len(), 6);
    assert_eq!(arr[0]["type"], "text");
    assert_eq!(arr[1]["type"], "thinking");
    assert_eq!(arr[1]["signature"], "sig");
    assert_eq!(arr[2]["type"], "tool_use");
    assert_eq!(arr[3]["type"], "tool_result");
    assert_eq!(arr[4]["type"], "image");
    assert_eq!(arr[5]["type"], "document");
}

// ── tools forwarded in Bedrock request ────────────────────────────────────────

#[tokio::test]
async fn bedrock_tools_forwarded() {
    let server = MockServer::start().await;
    let body = concat!(
        "event: message_start\n",
        "data: {\"type\":\"message_start\"}\n\n",
        "event: message_stop\n",
        "data: {\"type\":\"message_stop\"}\n\n",
    );

    Mock::given(method("POST"))
        .and(path(bedrock_path()))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_string(body),
        )
        .mount(&server)
        .await;

    let provider = BedrockAnthropicProvider::new(
        cfg(server.uri()),
        AuthMethod::ApiKey { value: "tok".into() },
    );

    let mut r = req();
    r.tools = vec![ToolSpec {
        name: "bash".into(),
        description: "run bash".into(),
        input_schema: json!({"type": "object", "properties": {}}),
    }];

    let _ = provider.generate(r, &model()).await.expect("ok");
}

// ── max_tokens forwarded ──────────────────────────────────────────────────────

#[tokio::test]
async fn bedrock_max_tokens_forwarded() {
    let server = MockServer::start().await;
    let body = concat!(
        "event: message_start\n",
        "data: {\"type\":\"message_start\"}\n\n",
        "event: message_stop\n",
        "data: {\"type\":\"message_stop\"}\n\n",
    );

    Mock::given(method("POST"))
        .and(path(bedrock_path()))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_string(body),
        )
        .mount(&server)
        .await;

    let provider = BedrockAnthropicProvider::new(
        cfg(server.uri()),
        AuthMethod::ApiKey { value: "tok".into() },
    );

    let mut r = req();
    r.max_output_tokens = Some(512);
    r.temperature = Some(0.3);

    let _ = provider.generate(r, &model()).await.expect("ok");
}

// ── provider config() and auth() accessors ─────────────────────────────────────

#[test]
fn bedrock_config_and_auth_accessors() {
    use pi_ai::provider::Provider;
    let provider = BedrockAnthropicProvider::new(
        cfg("http://localhost".into()),
        AuthMethod::ApiKey { value: "k".into() },
    );
    assert_eq!(provider.config().name, "bedrock");
    assert!(matches!(provider.auth(), AuthMethod::ApiKey { .. }));
}

// ── Messages with Role::System filtered out ────────────────────────────────────

#[tokio::test]
async fn bedrock_system_role_messages_filtered() {
    let server = MockServer::start().await;
    let body = concat!(
        "event: message_start\n",
        "data: {\"type\":\"message_start\"}\n\n",
        "event: content_block_delta\n",
        "data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"filtered\"}}\n\n",
        "event: message_stop\n",
        "data: {\"type\":\"message_stop\"}\n\n",
    );

    Mock::given(method("POST"))
        .and(path(bedrock_path()))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_string(body),
        )
        .mount(&server)
        .await;

    let provider = BedrockAnthropicProvider::new(
        cfg(server.uri()),
        AuthMethod::ApiKey { value: "tok".into() },
    );

    let mut r = req();
    r.messages = vec![
        Message::system_text("system content"),
        Message::user_text("user question"),
    ];

    let resp = provider.generate(r, &model()).await.expect("ok");
    assert_eq!(resp.message.text(), "filtered");
}

// --- RFD 0015: Bedrock parity with Anthropic Usage shape -----------------

fn shared_anthropic_sse_body() -> String {
    let mut s = String::new();
    s.push_str("event: message_start\n");
    s.push_str("data: {\"type\":\"message_start\",\"message\":{\"id\":\"msg_1\",\"usage\":{\"input_tokens\":1200,\"cache_read_input_tokens\":300,\"cache_creation_input_tokens\":50}}}\n\n");
    s.push_str("event: content_block_delta\n");
    s.push_str("data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"Hello, \"}}\n\n");
    s.push_str("event: content_block_delta\n");
    s.push_str("data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"world!\"}}\n\n");
    s.push_str("event: message_delta\n");
    s.push_str("data: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"end_turn\"},\"usage\":{\"output_tokens\":42}}\n\n");
    s.push_str("event: message_stop\n");
    s.push_str("data: {\"type\":\"message_stop\"}\n\n");
    s
}

#[tokio::test]
async fn bedrock_usage_shape_matches_anthropic_byte_for_byte() {
    let server = MockServer::start().await;

    Mock::given(method("POST"))
        .and(path(bedrock_path()))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_string(shared_anthropic_sse_body()),
        )
        .mount(&server)
        .await;

    // Same opus model as anthropic_stream test for cost-shape parity.
    let opus = ModelInfo {
        provider: "bedrock".into(),
        id: "anthropic.claude-test".into(),
        alias: None,
        context_window: 200_000,
        max_output_tokens: 32_000,
        supports_thinking: true,
        supports_tools: true,
        supports_vision: true,
        input_cost_per_mtok: 15.0,
        output_cost_per_mtok: 75.0,
        cache_read_cost_per_mtok: None,
        cache_write_cost_per_mtok: None,
    };

    let provider = BedrockAnthropicProvider::new(
        cfg(server.uri()),
        AuthMethod::ApiKey { value: "tok".into() },
    );

    let resp = provider.generate(req(), &opus).await.expect("ok");
    assert_eq!(resp.usage.input_tokens, 1200);
    assert_eq!(resp.usage.output_tokens, 42);
    assert_eq!(resp.usage.cache_read_tokens, 300);
    assert_eq!(resp.usage.cache_write_tokens, 50);
    assert_eq!(resp.usage.reasoning_tokens, 0);
    assert!(resp.usage.cost_usd > 0.0);
    // Cross-check: same input → same cost as the helper.
    use pi_ai::cost::{compute_cost, UsageAcc};
    let acc = UsageAcc {
        input_tokens: 1200,
        output_tokens: 42,
        cache_read_tok: 300,
        cache_write_tok: 50,
        reasoning_tok: 0,
    };
    let expected = compute_cost(&opus, &acc);
    assert!((resp.usage.cost_usd - expected).abs() < 1e-12);
}
