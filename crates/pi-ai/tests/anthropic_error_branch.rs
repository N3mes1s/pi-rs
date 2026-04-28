//! Error / malformed-line branches for the Anthropic streaming parser.

use futures::StreamExt;
use pi_ai::auth::AuthMethod;
use pi_ai::message::{Message, ThinkingLevel};
use pi_ai::provider::{AnthropicProvider, GenerateRequest, Provider, ProviderKind};
use pi_ai::registry::{ModelInfo, ProviderConfig};
use pi_ai::stream::StreamEventKind;
use pi_ai::AiError;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn cfg(base_url: String) -> ProviderConfig {
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
        cache_read_cost_per_mtok: None,
        cache_write_cost_per_mtok: None,
        api_kind: Default::default(),
    }
}

fn req() -> GenerateRequest {
    GenerateRequest {
        model: "claude-test".into(),
        system: None,
        messages: vec![Message::user_text("hi")],
        tools: vec![],
        thinking: ThinkingLevel::Off,
        temperature: None,
        max_output_tokens: None,
        extras: serde_json::Value::Null,
    }
}

#[tokio::test]
async fn anthropic_5xx_yields_provider_error() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .respond_with(ResponseTemplate::new(500).set_body_string("boom"))
        .mount(&server)
        .await;

    let provider = AnthropicProvider::new(
        cfg(server.uri()),
        AuthMethod::ApiKey {
            value: "k".into(),
        },
    );

    let res = provider.stream(req(), &model()).await;
    let err = res.err().expect("expected error");
    match err {
        AiError::Provider { status, body } => {
            assert_eq!(status, 500);
            assert_eq!(body, "boom");
        }
        other => panic!("expected Provider error, got {other:?}"),
    }
}

#[tokio::test]
async fn anthropic_skips_malformed_line_and_finishes() {
    let server = MockServer::start().await;
    let mut body = String::new();
    // valid message_start
    body.push_str("event: message_start\n");
    body.push_str("data: {\"type\":\"message_start\",\"message\":{\"id\":\"m1\"}}\n\n");
    // garbage line — not JSON. The parser should skip it.
    body.push_str("event: message_delta\n");
    body.push_str("data: this-is-not-json\n\n");
    // valid message_stop
    body.push_str("event: message_stop\n");
    body.push_str("data: {\"type\":\"message_stop\"}\n\n");

    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_string(body),
        )
        .mount(&server)
        .await;

    let provider = AnthropicProvider::new(
        cfg(server.uri()),
        AuthMethod::ApiKey {
            value: "k".into(),
        },
    );

    let mut stream = provider.stream(req(), &model()).await.expect("ok");
    let mut kinds = Vec::new();
    while let Some(item) = stream.next().await {
        kinds.push(item.expect("ok").kind);
    }
    // Must contain MessageStart and a Finish at the end.
    assert!(
        kinds.iter().any(|k| matches!(k, StreamEventKind::MessageStart)),
        "expected MessageStart, got: {kinds:?}"
    );
    assert!(
        matches!(kinds.last(), Some(StreamEventKind::Finish { .. })),
        "expected Finish at end, got: {kinds:?}"
    );
}
