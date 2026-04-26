//! Streaming tool-call branch and `[DONE]` flush coverage for OpenAI.

use pi_ai::auth::AuthMethod;
use pi_ai::message::{FinishReason, Message, ThinkingLevel};
use pi_ai::provider::{GenerateRequest, OpenAiProvider, Provider, ProviderKind};
use pi_ai::registry::{ModelInfo, ProviderConfig};
use wiremock::matchers::{method, path};
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

fn req() -> GenerateRequest {
    GenerateRequest {
        model: "gpt-test".into(),
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
async fn openai_streams_tool_call_with_split_arguments() {
    let server = MockServer::start().await;

    let mut body = String::new();
    body.push_str(
        "data: {\"choices\":[{\"index\":0,\"delta\":{\"tool_calls\":[{\"index\":0,\"id\":\"call_1\",\"function\":{\"name\":\"foo\",\"arguments\":\"{\\\"a\\\":\"}}]}}]}\n\n",
    );
    body.push_str(
        "data: {\"choices\":[{\"index\":0,\"delta\":{\"tool_calls\":[{\"index\":0,\"function\":{\"arguments\":\"1}\"}}]}}]}\n\n",
    );
    body.push_str(
        "data: {\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"tool_calls\"}]}\n\n",
    );
    body.push_str("data: [DONE]\n\n");

    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_string(body),
        )
        .mount(&server)
        .await;

    let provider = OpenAiProvider::new(
        provider_config(server.uri()),
        AuthMethod::ApiKey {
            value: "dummy".into(),
        },
    );

    let resp = provider.generate(req(), &model()).await.expect("ok");
    assert_eq!(resp.tool_calls.len(), 1);
    let c = &resp.tool_calls[0];
    assert_eq!(c.id, "call_1");
    assert_eq!(c.name, "foo");
    assert_eq!(c.input, serde_json::json!({"a": 1}));
}

#[tokio::test]
async fn openai_done_tail_flushes_finish_stop() {
    let server = MockServer::start().await;
    let mut body = String::new();
    body.push_str(
        "data: {\"choices\":[{\"index\":0,\"delta\":{\"content\":\"hi\"}}]}\n\n",
    );
    body.push_str("data: [DONE]\n\n");

    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_string(body),
        )
        .mount(&server)
        .await;

    let provider = OpenAiProvider::new(
        provider_config(server.uri()),
        AuthMethod::ApiKey {
            value: "dummy".into(),
        },
    );

    let resp = provider.generate(req(), &model()).await.expect("ok");
    assert_eq!(resp.message.text(), "hi");
    assert!(matches!(resp.finish_reason, FinishReason::Stop));
}
