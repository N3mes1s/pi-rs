//! Integration tests for BedrockAnthropicProvider streaming.
//!
//! The Bedrock wire format is identical to Anthropic Messages SSE except:
//! - path: /model/{id}/invoke-with-response-stream
//! - body uses `anthropic_version: "bedrock-2023-05-31"` (no `model` field)
//! - auth: `Authorization: Bearer <token>` (pre-signed; real SigV4 not needed for tests)

use futures::StreamExt;
use pi_ai::auth::AuthMethod;
use pi_ai::message::{FinishReason, Message, ThinkingLevel};
use pi_ai::provider::{BedrockAnthropicProvider, GenerateRequest, Provider, ProviderKind};
use pi_ai::registry::{ModelInfo, ProviderConfig};
use pi_ai::AiError;
use wiremock::matchers::{header, method, path};
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
        tier: 1,
        supports_thinking: false,
        supports_tools: true,
        supports_vision: false,
        input_cost_per_mtok: 0.0,
        output_cost_per_mtok: 0.0,
        cache_read_cost_per_mtok: None,
        cache_write_cost_per_mtok: None,
        api_kind: Default::default(),
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

fn sse_body() -> String {
    let mut s = String::new();
    s.push_str("event: message_start\n");
    s.push_str("data: {\"type\":\"message_start\",\"message\":{\"id\":\"msg_1\"}}\n\n");

    s.push_str("event: content_block_delta\n");
    s.push_str("data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"Hello from \"}}\n\n");

    s.push_str("event: content_block_delta\n");
    s.push_str("data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"Bedrock!\"}}\n\n");

    s.push_str("event: message_delta\n");
    s.push_str("data: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"end_turn\"},\"usage\":{\"output_tokens\":5}}\n\n");

    s.push_str("event: message_stop\n");
    s.push_str("data: {\"type\":\"message_stop\"}\n\n");
    s
}

/// Happy path: text deltas are assembled and finish reason is Stop.
#[tokio::test]
async fn bedrock_stream_assembles_text_and_finish() {
    let server = MockServer::start().await;

    Mock::given(method("POST"))
        .and(path(
            "/model/anthropic.claude-test/invoke-with-response-stream",
        ))
        .and(header("Authorization", "Bearer test-token"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_string(sse_body()),
        )
        .mount(&server)
        .await;

    let provider = BedrockAnthropicProvider::new(
        cfg(server.uri()),
        AuthMethod::ApiKey {
            value: "test-token".into(),
        },
    );

    let resp = provider
        .generate(req(), &model())
        .await
        .expect("generate ok");
    assert_eq!(resp.message.text(), "Hello from Bedrock!");
    assert!(matches!(resp.finish_reason, FinishReason::Stop));
    assert!(resp.tool_calls.is_empty());
}

/// Streaming variant: collect individual text deltas from the stream.
#[tokio::test]
async fn bedrock_stream_yields_text_deltas() {
    let server = MockServer::start().await;

    Mock::given(method("POST"))
        .and(path(
            "/model/anthropic.claude-test/invoke-with-response-stream",
        ))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_string(sse_body()),
        )
        .mount(&server)
        .await;

    let provider = BedrockAnthropicProvider::new(
        cfg(server.uri()),
        AuthMethod::ApiKey {
            value: "test-token".into(),
        },
    );

    use pi_ai::stream::StreamEventKind;
    let mut stream = provider.stream(req(), &model()).await.expect("stream ok");
    let mut text = String::new();
    let mut saw_finish = false;
    while let Some(ev) = stream.next().await {
        match ev.expect("event ok").kind {
            StreamEventKind::TextDelta { text: t } => text.push_str(&t),
            StreamEventKind::Finish { .. } => saw_finish = true,
            _ => {}
        }
    }
    assert_eq!(text, "Hello from Bedrock!");
    assert!(saw_finish, "expected a Finish event");
}

/// 5xx from Bedrock → AiError::Provider.
#[tokio::test]
async fn bedrock_5xx_yields_provider_error() {
    let server = MockServer::start().await;

    Mock::given(method("POST"))
        .and(path(
            "/model/anthropic.claude-test/invoke-with-response-stream",
        ))
        .respond_with(ResponseTemplate::new(500).set_body_string("internal error"))
        .mount(&server)
        .await;

    let provider = BedrockAnthropicProvider::new(
        cfg(server.uri()),
        AuthMethod::ApiKey {
            value: "test-token".into(),
        },
    );

    let err = provider
        .stream(req(), &model())
        .await
        .err()
        .expect("expected error");
    match err {
        AiError::Provider { status, body } => {
            assert_eq!(status, 500);
            assert_eq!(body, "internal error");
        }
        other => panic!("expected AiError::Provider, got {other:?}"),
    }
}

/// with_region builder sets the region field (smoke test).
#[test]
fn bedrock_with_region_builder() {
    let provider = BedrockAnthropicProvider::new(cfg("http://localhost".into()), AuthMethod::None)
        .with_region("eu-west-1");
    assert_eq!(provider.region, "eu-west-1");
}
