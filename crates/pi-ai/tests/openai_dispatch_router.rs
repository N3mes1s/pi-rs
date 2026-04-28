// TODO(rfd-0019): re-enable after responses-registry + responses-core merge.
//
// End-to-end check that the per-model `ApiKind` dispatch works:
// `gpt-5.4` must POST to `/v1/responses` and `gpt-4o` must POST to
// `/v1/chat/completions`, with `Content-Type: application/json` on
// both. Uses wiremock to record exactly one request per model.

#![cfg(rfd_0019_responses)]
#![allow(dead_code)]

use pi_ai::auth::AuthMethod;
use pi_ai::message::{Message, ThinkingLevel};
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

fn model(id: &str) -> ModelInfo {
    ModelInfo {
        provider: "openai".into(),
        id: id.into(),
        alias: Some(id.into()),
        context_window: 1024,
        max_output_tokens: 256,
        supports_thinking: id.starts_with("gpt-5"),
        supports_tools: true,
        supports_vision: false,
        input_cost_per_mtok: 0.0,
        output_cost_per_mtok: 0.0,
        cache_read_cost_per_mtok: None,
        cache_write_cost_per_mtok: None,
    }
}

fn responses_sse() -> String {
    // Minimal text-only Responses stream that the parser is expected to
    // tolerate without panicking.
    let mut s = String::new();
    s.push_str("event: response.created\n");
    s.push_str("data: {\"type\":\"response.created\",\"response\":{\"id\":\"r1\",\"status\":\"in_progress\",\"model\":\"gpt-5.4\"}}\n\n");
    s.push_str("event: response.completed\n");
    s.push_str("data: {\"type\":\"response.completed\",\"response\":{\"id\":\"r1\",\"status\":\"completed\",\"model\":\"gpt-5.4\",\"usage\":{\"input_tokens\":1,\"output_tokens\":1,\"total_tokens\":2}}}\n\n");
    s
}

fn chat_completions_sse() -> String {
    let mut s = String::new();
    s.push_str("data: {\"choices\":[{\"index\":0,\"delta\":{\"content\":\"ok\"}}]}\n\n");
    s.push_str("data: {\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"stop\"}]}\n\n");
    s.push_str("data: {\"choices\":[],\"usage\":{\"prompt_tokens\":1,\"completion_tokens\":1,\"total_tokens\":2}}\n\n");
    s.push_str("data: [DONE]\n\n");
    s
}

fn make_request(model_id: &str) -> GenerateRequest {
    GenerateRequest {
        model: model_id.into(),
        system: None,
        messages: vec![Message::user_text("hi")],
        tools: vec![],
        thinking: ThinkingLevel::Off,
        temperature: None,
        max_output_tokens: Some(64),
        extras: serde_json::Value::Null,
    }
}

#[tokio::test]
async fn gpt54_posts_to_v1_responses() {
    let server = MockServer::start().await;

    Mock::given(method("POST"))
        .and(path("/responses"))
        .and(header("content-type", "application/json"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_string(responses_sse()),
        )
        .expect(1)
        .mount(&server)
        .await;

    let provider = OpenAiProvider::new(
        provider_config(server.uri()),
        AuthMethod::ApiKey {
            value: "dummy".into(),
        },
    );

    let _ = provider
        .generate(make_request("gpt-5.4"), &model("gpt-5.4"))
        .await
        .expect("generate ok");
    // Mock `.expect(1)` enforces exactly one POST hit `/responses`.
}

#[tokio::test]
async fn gpt4o_posts_to_v1_chat_completions() {
    let server = MockServer::start().await;

    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .and(header("content-type", "application/json"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_string(chat_completions_sse()),
        )
        .expect(1)
        .mount(&server)
        .await;

    let provider = OpenAiProvider::new(
        provider_config(server.uri()),
        AuthMethod::ApiKey {
            value: "dummy".into(),
        },
    );

    let _ = provider
        .generate(make_request("gpt-4o"), &model("gpt-4o"))
        .await
        .expect("generate ok");
}
