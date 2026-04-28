//! Integration tests for AzureOpenAiProvider streaming.
//!
//! Azure OpenAI uses the OpenAI Chat Completions wire format but:
//! - path: /openai/deployments/{deployment}/chat/completions?api-version=...
//! - auth header: `api-key: <value>` (not `Authorization: Bearer`)

use futures::StreamExt;
use pi_ai::auth::AuthMethod;
use pi_ai::message::{FinishReason, Message, ThinkingLevel};
use pi_ai::provider::{AzureOpenAiProvider, GenerateRequest, Provider, ProviderKind};
use pi_ai::registry::{ModelInfo, ProviderConfig};
use pi_ai::AiError;
use wiremock::matchers::{header, method, path_regex};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn cfg(base_url: String) -> ProviderConfig {
    ProviderConfig {
        name: "azure-openai".into(),
        kind: ProviderKind::Azure,
        base_url,
        auth_header: "api-key".into(),
        auth_format: "{token}".into(),
        models: vec![],
    }
}

fn model() -> ModelInfo {
    ModelInfo {
        provider: "azure-openai".into(),
        id: "my-gpt4o-deployment".into(),
        alias: None,
        context_window: 128_000,
        max_output_tokens: 4096,
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
        model: "my-gpt4o-deployment".into(),
        system: None,
        messages: vec![Message::user_text("hello")],
        tools: vec![],
        thinking: ThinkingLevel::Off,
        temperature: None,
        max_output_tokens: None,
        extras: serde_json::Value::Null,
    }
}

fn sse_body() -> String {
    let mut s = String::new();
    s.push_str("data: {\"choices\":[{\"index\":0,\"delta\":{\"role\":\"assistant\",\"content\":\"Hello from \"}}]}\n\n");
    s.push_str("data: {\"choices\":[{\"index\":0,\"delta\":{\"content\":\"Azure!\"}}]}\n\n");
    s.push_str("data: {\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"stop\"}]}\n\n");
    s.push_str("data: {\"choices\":[],\"usage\":{\"prompt_tokens\":5,\"completion_tokens\":4,\"total_tokens\":9}}\n\n");
    s.push_str("data: [DONE]\n\n");
    s
}

/// Happy path: text deltas are assembled and finish reason is Stop.
#[tokio::test]
async fn azure_stream_assembles_text_and_finish() {
    let server = MockServer::start().await;

    Mock::given(method("POST"))
        .and(path_regex(
            r"^/openai/deployments/my-gpt4o-deployment/chat/completions",
        ))
        .and(header("api-key", "azure-test-key"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_string(sse_body()),
        )
        .mount(&server)
        .await;

    let provider = AzureOpenAiProvider::new(
        cfg(server.uri()),
        AuthMethod::ApiKey {
            value: "azure-test-key".into(),
        },
    )
    .with_api_version("2024-10-21");

    let resp = provider
        .generate(req(), &model())
        .await
        .expect("generate ok");
    assert_eq!(resp.message.text(), "Hello from Azure!");
    assert!(matches!(resp.finish_reason, FinishReason::Stop));
    assert!(resp.tool_calls.is_empty());
}

/// Streaming variant: collect individual text deltas from the stream.
#[tokio::test]
async fn azure_stream_yields_text_deltas() {
    let server = MockServer::start().await;

    Mock::given(method("POST"))
        .and(path_regex(
            r"^/openai/deployments/my-gpt4o-deployment/chat/completions",
        ))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_string(sse_body()),
        )
        .mount(&server)
        .await;

    let provider = AzureOpenAiProvider::new(
        cfg(server.uri()),
        AuthMethod::ApiKey {
            value: "azure-test-key".into(),
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
    assert_eq!(text, "Hello from Azure!");
    assert!(saw_finish, "expected a Finish event");
}

/// Usage tokens are reported correctly.
#[tokio::test]
async fn azure_stream_reports_usage() {
    let server = MockServer::start().await;

    Mock::given(method("POST"))
        .and(path_regex(
            r"^/openai/deployments/my-gpt4o-deployment/chat/completions",
        ))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_string(sse_body()),
        )
        .mount(&server)
        .await;

    let provider = AzureOpenAiProvider::new(
        cfg(server.uri()),
        AuthMethod::ApiKey {
            value: "azure-test-key".into(),
        },
    );

    let resp = provider
        .generate(req(), &model())
        .await
        .expect("generate ok");
    assert_eq!(resp.usage.input_tokens, 5);
    assert_eq!(resp.usage.output_tokens, 4);
}

/// 5xx from Azure → AiError::Provider.
#[tokio::test]
async fn azure_5xx_yields_provider_error() {
    let server = MockServer::start().await;

    Mock::given(method("POST"))
        .and(path_regex(
            r"^/openai/deployments/my-gpt4o-deployment/chat/completions",
        ))
        .respond_with(ResponseTemplate::new(503).set_body_string("service unavailable"))
        .mount(&server)
        .await;

    let provider = AzureOpenAiProvider::new(
        cfg(server.uri()),
        AuthMethod::ApiKey {
            value: "azure-test-key".into(),
        },
    );

    let err = provider
        .stream(req(), &model())
        .await
        .err()
        .expect("expected error");
    match err {
        AiError::Provider { status, body } => {
            assert_eq!(status, 503);
            assert_eq!(body, "service unavailable");
        }
        other => panic!("expected AiError::Provider, got {other:?}"),
    }
}

/// with_api_version builder sets the version field (smoke test).
#[test]
fn azure_with_api_version_builder() {
    let provider = AzureOpenAiProvider::new(cfg("http://localhost".into()), AuthMethod::None)
        .with_api_version("2025-01-01");
    assert_eq!(provider.api_version, "2025-01-01");
}
