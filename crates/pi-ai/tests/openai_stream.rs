use pi_ai::auth::AuthMethod;
use pi_ai::message::{FinishReason, Message, Role, ThinkingLevel};
use pi_ai::provider::{GenerateRequest, OpenAiProvider, Provider, ProviderKind};
use pi_ai::registry::{ModelInfo, ProviderConfig};
use wiremock::matchers::{header, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn provider_config(base_url: String) -> ProviderConfig {
    ProviderConfig {
        name: "openai".into(),
        kind: ProviderKind::OpenAi,
        base_url,
        auth_header: "Authorization".into(),
        auth_format: "Bearer {token}".into(),
        models: vec![],
    }
}

fn model() -> ModelInfo {
    ModelInfo {
        provider: "openai".into(),
        id: "gpt-test".into(),
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
    }
}

fn sse_body() -> String {
    // OpenAI Chat Completions SSE wire format, abbreviated.
    let mut s = String::new();
    s.push_str("data: {\"choices\":[{\"index\":0,\"delta\":{\"role\":\"assistant\",\"content\":\"Hello, \"}}]}\n\n");
    s.push_str("data: {\"choices\":[{\"index\":0,\"delta\":{\"content\":\"world!\"}}]}\n\n");
    s.push_str("data: {\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"stop\"}]}\n\n");
    s.push_str("data: {\"choices\":[],\"usage\":{\"prompt_tokens\":4,\"completion_tokens\":3,\"total_tokens\":7}}\n\n");
    s.push_str("data: [DONE]\n\n");
    s
}

#[tokio::test]
async fn openai_generate_assembles_text_and_usage() {
    let server = MockServer::start().await;

    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .and(header("Authorization", "Bearer dummy-key"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_string(sse_body()),
        )
        .mount(&server)
        .await;

    let provider = OpenAiProvider::new(
        provider_config(server.uri()),
        AuthMethod::ApiKey {
            value: "dummy-key".into(),
        },
    );

    let req = GenerateRequest {
        model: "gpt-test".into(),
        system: None,
        messages: vec![Message::user_text("hi")],
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
    assert_eq!(resp.usage.input_tokens, 4);
    assert_eq!(resp.usage.output_tokens, 3);
    assert!(resp.tool_calls.is_empty());
}

// --- RFD 0015: closing-chunk usage populates every field + cost ------

fn gpt5_model() -> ModelInfo {
    ModelInfo {
        provider: "openai".into(),
        id: "gpt-5".into(),
        alias: Some("gpt-5".into()),
        context_window: 1024,
        max_output_tokens: 256,
        supports_thinking: true,
        supports_tools: true,
        supports_vision: false,
        input_cost_per_mtok: 1.25,
        output_cost_per_mtok: 10.0,
        cache_read_cost_per_mtok: None,
        cache_write_cost_per_mtok: None,
    }
}

fn sse_body_full_usage() -> String {
    let mut s = String::new();
    s.push_str("data: {\"choices\":[{\"index\":0,\"delta\":{\"role\":\"assistant\",\"content\":\"ok\"}}]}\n\n");
    s.push_str("data: {\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"stop\"}]}\n\n");
    s.push_str(
        "data: {\"choices\":[],\"usage\":{\
\"prompt_tokens\":1234,\
\"completion_tokens\":56,\
\"prompt_tokens_details\":{\"cached_tokens\":100},\
\"completion_tokens_details\":{\"reasoning_tokens\":20}\
}}\n\n",
    );
    s.push_str("data: [DONE]\n\n");
    s
}

#[tokio::test]
async fn openai_closing_chunk_populates_every_usage_field() {
    let server = MockServer::start().await;

    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_string(sse_body_full_usage()),
        )
        .mount(&server)
        .await;

    let provider = OpenAiProvider::new(
        provider_config(server.uri()),
        AuthMethod::ApiKey { value: "k".into() },
    );

    let req = GenerateRequest {
        model: "gpt-5".into(),
        system: None,
        messages: vec![Message::user_text("hi")],
        tools: vec![],
        thinking: ThinkingLevel::Off,
        temperature: None,
        max_output_tokens: None,
        extras: serde_json::Value::Null,
    };

    let resp = provider.generate(req, &gpt5_model()).await.expect("ok");
    assert_eq!(resp.usage.input_tokens, 1234);
    assert_eq!(resp.usage.output_tokens, 56);
    assert_eq!(resp.usage.cache_read_tokens, 100);
    assert_eq!(resp.usage.reasoning_tokens, 20);
    assert!(resp.usage.cost_usd > 0.0, "cost should be > 0, got {}", resp.usage.cost_usd);
}
