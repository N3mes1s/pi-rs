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
        cache_read_cost_per_mtok: None,
        cache_write_cost_per_mtok: None,
        api_kind: Default::default(),
    }
}

fn sse_body() -> String {
    // Mimic the Anthropic SSE wire format (a subset sufficient for our parser).
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
async fn anthropic_generate_collapses_text_deltas_to_response_message() {
    let server = MockServer::start().await;

    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .and(header("x-api-key", "dummy-key"))
        .and(header("anthropic-version", "2023-06-01"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_string(sse_body()),
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
            content: vec![ContentBlock::Text { text: "hi".into() }],
        }],
        tools: vec![],
        thinking: ThinkingLevel::Off,
        temperature: None,
        max_output_tokens: None,
        extras: serde_json::Value::Null,
    };

    let resp = provider.generate(req, &model()).await.expect("generate ok");
    assert_eq!(resp.message.role, Role::Assistant);
    assert_eq!(resp.message.text(), "Hello, world!");
    assert!(matches!(resp.finish_reason, FinishReason::Stop));
    assert!(resp.tool_calls.is_empty());
}

#[tokio::test]
async fn anthropic_stream_populates_every_usage_field_with_real_cost() {
    let server = MockServer::start().await;

    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .and(header("x-api-key", "dummy-key"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_string(sse_body()),
        )
        .mount(&server)
        .await;

    // Use a model with non-zero pricing so cost_usd > 0.
    let opus = ModelInfo {
        provider: "anthropic".into(),
        id: "claude-opus-4-7".into(),
        alias: Some("opus".into()),
        context_window: 200_000,
        max_output_tokens: 32_000,
        supports_thinking: true,
        supports_tools: true,
        supports_vision: true,
        input_cost_per_mtok: 15.0,
        output_cost_per_mtok: 75.0,
        cache_read_cost_per_mtok: None,
        cache_write_cost_per_mtok: None,
        api_kind: Default::default(),
    };

    let provider = AnthropicProvider::new(
        provider_config(server.uri()),
        AuthMethod::ApiKey {
            value: "dummy-key".into(),
        },
    );

    let req = GenerateRequest {
        model: "claude-opus-4-7".into(),
        system: None,
        messages: vec![Message {
            role: Role::User,
            content: vec![ContentBlock::Text { text: "hi".into() }],
        }],
        tools: vec![],
        thinking: ThinkingLevel::Off,
        temperature: None,
        max_output_tokens: None,
        extras: serde_json::Value::Null,
    };

    let resp = provider.generate(req, &opus).await.expect("generate ok");
    assert_eq!(resp.usage.input_tokens, 1200);
    assert_eq!(resp.usage.output_tokens, 42);
    assert_eq!(resp.usage.cache_read_tokens, 300);
    assert_eq!(resp.usage.cache_write_tokens, 50);
    assert!(
        resp.usage.cost_usd > 0.0,
        "expected non-zero cost, got {}",
        resp.usage.cost_usd
    );
}
