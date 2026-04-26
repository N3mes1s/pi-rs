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
