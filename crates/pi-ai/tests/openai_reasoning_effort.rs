//! A1: OpenAI reasoning models (o1/o1-mini/o3-mini) require `reasoning_effort`
//! in the request body. We only set it when `model.supports_thinking == true`,
//! and we map `ThinkingLevel::{Low, Medium, High}` → `"low"|"medium"|"high"`.
//! `ThinkingLevel::Off` omits the field entirely.

use pi_ai::auth::AuthMethod;
use pi_ai::message::{Message, ThinkingLevel};
use pi_ai::provider::{GenerateRequest, OpenAiProvider, Provider, ProviderKind};
use pi_ai::registry::{ModelInfo, ProviderConfig};
use serde_json::Value;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, Request, ResponseTemplate};

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

fn thinking_model() -> ModelInfo {
    ModelInfo {
        provider: "openai".into(),
        id: "o3-mini".into(),
        alias: Some("o3-mini".into()),
        context_window: 1024,
        max_output_tokens: 256,
        supports_thinking: true,
        supports_tools: true,
        supports_vision: false,
        input_cost_per_mtok: 0.0,
        output_cost_per_mtok: 0.0,
        cache_read_cost_per_mtok: None,
        cache_write_cost_per_mtok: None,
        api_kind: Default::default(),
    }
}

fn non_thinking_model() -> ModelInfo {
    ModelInfo {
        provider: "openai".into(),
        id: "gpt-4o".into(),
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
        api_kind: Default::default(),
    }
}

fn sse_done() -> String {
    let mut s = String::new();
    s.push_str("data: {\"choices\":[{\"index\":0,\"delta\":{\"role\":\"assistant\",\"content\":\"ok\"}}]}\n\n");
    s.push_str("data: {\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"stop\"}]}\n\n");
    s.push_str("data: [DONE]\n\n");
    s
}

async fn capture_body(level: ThinkingLevel, model: ModelInfo) -> Value {
    let server = MockServer::start().await;

    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_string(sse_done()),
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
        model: model.id.clone(),
        system: None,
        messages: vec![Message::user_text("hi")],
        tools: vec![],
        thinking: level,
        temperature: None,
        max_output_tokens: None,
        extras: Value::Null,
    };

    let _ = provider.generate(req, &model).await.expect("ok");

    let received: Vec<Request> = server.received_requests().await.expect("requests");
    assert_eq!(received.len(), 1);
    serde_json::from_slice(&received[0].body).expect("json body")
}

#[tokio::test]
async fn thinking_model_low_sets_reasoning_effort_low() {
    let body = capture_body(ThinkingLevel::Low, thinking_model()).await;
    assert_eq!(body["reasoning_effort"], "low");
}

#[tokio::test]
async fn thinking_model_medium_sets_reasoning_effort_medium() {
    let body = capture_body(ThinkingLevel::Medium, thinking_model()).await;
    assert_eq!(body["reasoning_effort"], "medium");
}

#[tokio::test]
async fn thinking_model_high_sets_reasoning_effort_high() {
    let body = capture_body(ThinkingLevel::High, thinking_model()).await;
    assert_eq!(body["reasoning_effort"], "high");
}

#[tokio::test]
async fn thinking_model_off_omits_field() {
    let body = capture_body(ThinkingLevel::Off, thinking_model()).await;
    assert!(body.get("reasoning_effort").is_none());
}

#[tokio::test]
async fn non_thinking_model_never_sets_reasoning_effort() {
    let body = capture_body(ThinkingLevel::High, non_thinking_model()).await;
    assert!(body.get("reasoning_effort").is_none());
}
