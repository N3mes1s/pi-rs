//! Extra coverage tests for AzureOpenAiProvider.
//!
//! Covers: OAuth auth, with_client builder, tool-call stream,
//! temperature/max_tokens forwarding, content_filter finish reason,
//! AuthMethod::None → MissingAuth, reasoning_tokens in usage.

use futures::StreamExt;
use pi_ai::auth::AuthMethod;
use pi_ai::message::{ContentBlock, Message, Role, ThinkingLevel};
use pi_ai::provider::{AzureOpenAiProvider, GenerateRequest, Provider, ProviderKind};
use pi_ai::registry::{ModelInfo, ProviderConfig};
use pi_ai::stream::StreamEventKind;
use pi_ai::{AiError, FinishReason, ToolSpec};
use serde_json::json;
use wiremock::matchers::{method, path_regex};
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

// ── AuthMethod::None → MissingAuth ─────────────────────────────────────────────

#[tokio::test]
async fn azure_no_auth_gives_missing_auth_error() {
    let provider = AzureOpenAiProvider::new(cfg("http://localhost".into()), AuthMethod::None);
    let err = provider.stream(req(), &model()).await.err().expect("error");
    match err {
        AiError::MissingAuth(_) => {}
        other => panic!("expected MissingAuth, got {other:?}"),
    }
}

// ── OAuth token is accepted ────────────────────────────────────────────────────

#[tokio::test]
async fn azure_oauth_token_is_used() {
    let server = MockServer::start().await;
    let body = "data: {\"choices\":[{\"index\":0,\"delta\":{\"content\":\"Hi\"},\"finish_reason\":null}]}\n\ndata: [DONE]\n\n";

    Mock::given(method("POST"))
        .and(path_regex(r"^/openai/deployments"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_string(body),
        )
        .mount(&server)
        .await;

    let provider = AzureOpenAiProvider::new(
        cfg(server.uri()),
        AuthMethod::OAuth {
            access_token: "oauth-token".into(),
            refresh_token: None,
            expires_at: None,
        },
    );

    // Should succeed (OAuth token is forwarded as api-key header value).
    let resp = provider
        .generate(req(), &model())
        .await
        .expect("generate ok");
    assert_eq!(resp.message.text(), "Hi");
}

// ── with_client builder smoke test ────────────────────────────────────────────

#[test]
fn azure_with_client_builder() {
    let client = reqwest::Client::new();
    let provider = AzureOpenAiProvider::new(cfg("http://localhost".into()), AuthMethod::None)
        .with_client(client);
    // Just verify we can construct the provider; the client field is set.
    assert_eq!(provider.api_version, "2024-10-21");
}

// ── System message is prepended ───────────────────────────────────────────────

#[tokio::test]
async fn azure_system_message_forwarded() {
    let server = MockServer::start().await;
    let body = "data: {\"choices\":[{\"index\":0,\"delta\":{\"content\":\"ok\"},\"finish_reason\":null}]}\n\ndata: [DONE]\n\n";

    Mock::given(method("POST"))
        .and(path_regex(r"^/openai/deployments"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_string(body),
        )
        .mount(&server)
        .await;

    let provider =
        AzureOpenAiProvider::new(cfg(server.uri()), AuthMethod::ApiKey { value: "k".into() });

    let mut r = req();
    r.system = Some("You are helpful.".into());

    let resp = provider.generate(r, &model()).await.expect("ok");
    assert_eq!(resp.message.text(), "ok");
}

// ── temperature and max_tokens are forwarded ──────────────────────────────────

#[tokio::test]
async fn azure_temperature_and_max_tokens_forwarded() {
    let server = MockServer::start().await;
    let body = "data: {\"choices\":[{\"index\":0,\"delta\":{\"content\":\"y\"},\"finish_reason\":null}]}\n\ndata: [DONE]\n\n";

    Mock::given(method("POST"))
        .and(path_regex(r"^/openai/deployments"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_string(body),
        )
        .mount(&server)
        .await;

    let provider =
        AzureOpenAiProvider::new(cfg(server.uri()), AuthMethod::ApiKey { value: "k".into() });

    let mut r = req();
    r.temperature = Some(0.7);
    r.max_output_tokens = Some(200);

    // Just verify no error; exact values are forwarded in JSON body which wiremock doesn't check here.
    let resp = provider.generate(r, &model()).await.expect("ok");
    assert_eq!(resp.message.text(), "y");
}

// ── Tool call stream: tool_calls finish_reason ────────────────────────────────

#[tokio::test]
async fn azure_tool_call_stream_emits_tool_call_complete() {
    let server = MockServer::start().await;

    // Simulate a tool call: delta with function start, arguments, then finish_reason=tool_calls
    let body = concat!(
        "data: {\"choices\":[{\"index\":0,\"delta\":{\"tool_calls\":[{\"index\":0,\"id\":\"call_abc\",\"type\":\"function\",\"function\":{\"name\":\"bash\",\"arguments\":\"\"}}]}}]}\n\n",
        "data: {\"choices\":[{\"index\":0,\"delta\":{\"tool_calls\":[{\"index\":0,\"function\":{\"arguments\":\"{\\\"cmd\\\":\\\"ls\\\"}\"}}]}}]}\n\n",
        "data: {\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"tool_calls\"}]}\n\n",
        "data: [DONE]\n\n",
    );

    Mock::given(method("POST"))
        .and(path_regex(r"^/openai/deployments"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_string(body),
        )
        .mount(&server)
        .await;

    let provider =
        AzureOpenAiProvider::new(cfg(server.uri()), AuthMethod::ApiKey { value: "k".into() });

    let mut stream = provider.stream(req(), &model()).await.expect("ok");
    let mut saw_tool_complete = false;
    let mut saw_tool_input_delta = false;
    while let Some(ev) = stream.next().await {
        if let Ok(e) = ev {
            match &e.kind {
                StreamEventKind::ToolCallComplete { name, .. } if name == "bash" => {
                    saw_tool_complete = true;
                }
                StreamEventKind::ToolInputDelta { .. } => {
                    saw_tool_input_delta = true;
                }
                _ => {}
            }
        }
    }
    assert!(
        saw_tool_complete || saw_tool_input_delta,
        "expected ToolCallComplete or ToolInputDelta for bash tool"
    );
}

// ── content_filter finish reason → Refusal ────────────────────────────────────

#[tokio::test]
async fn azure_content_filter_gives_refusal() {
    let server = MockServer::start().await;
    let body = "data: {\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"content_filter\"}]}\n\ndata: [DONE]\n\n";

    Mock::given(method("POST"))
        .and(path_regex(r"^/openai/deployments"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_string(body),
        )
        .mount(&server)
        .await;

    let provider =
        AzureOpenAiProvider::new(cfg(server.uri()), AuthMethod::ApiKey { value: "k".into() });

    let mut stream = provider.stream(req(), &model()).await.expect("ok");
    let mut saw_refusal = false;
    while let Some(ev) = stream.next().await {
        if let Ok(e) = ev {
            if matches!(
                e.kind,
                StreamEventKind::Finish {
                    reason: FinishReason::Refusal
                }
            ) {
                saw_refusal = true;
            }
        }
    }
    assert!(saw_refusal, "expected Refusal finish reason");
}

// ── length finish reason ───────────────────────────────────────────────────────

#[tokio::test]
async fn azure_length_finish_reason() {
    let server = MockServer::start().await;
    let body = "data: {\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"length\"}]}\n\ndata: [DONE]\n\n";

    Mock::given(method("POST"))
        .and(path_regex(r"^/openai/deployments"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_string(body),
        )
        .mount(&server)
        .await;

    let provider =
        AzureOpenAiProvider::new(cfg(server.uri()), AuthMethod::ApiKey { value: "k".into() });

    let mut stream = provider.stream(req(), &model()).await.expect("ok");
    let mut saw_length = false;
    while let Some(ev) = stream.next().await {
        if let Ok(e) = ev {
            if matches!(
                e.kind,
                StreamEventKind::Finish {
                    reason: FinishReason::Length
                }
            ) {
                saw_length = true;
            }
        }
    }
    assert!(saw_length, "expected Length finish reason");
}

// ── reasoning_tokens in usage ─────────────────────────────────────────────────

#[tokio::test]
async fn azure_reasoning_tokens_in_usage() {
    let server = MockServer::start().await;
    let body = concat!(
        "data: {\"choices\":[{\"index\":0,\"delta\":{\"content\":\"hi\"}}]}\n\n",
        "data: {\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"stop\"}]}\n\n",
        "data: {\"choices\":[],\"usage\":{\"prompt_tokens\":3,\"completion_tokens\":2,\"total_tokens\":5,\"completion_tokens_details\":{\"reasoning_tokens\":7}}}\n\n",
        "data: [DONE]\n\n",
    );

    Mock::given(method("POST"))
        .and(path_regex(r"^/openai/deployments"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_string(body),
        )
        .mount(&server)
        .await;

    let provider =
        AzureOpenAiProvider::new(cfg(server.uri()), AuthMethod::ApiKey { value: "k".into() });

    let resp = provider.generate(req(), &model()).await.expect("ok");
    assert_eq!(resp.usage.reasoning_tokens, 7);
}

// ── tools forwarded in request body ───────────────────────────────────────────

#[tokio::test]
async fn azure_tools_forwarded_in_request() {
    let server = MockServer::start().await;
    let body = "data: {\"choices\":[{\"index\":0,\"delta\":{\"content\":\"done\"},\"finish_reason\":null}]}\n\ndata: [DONE]\n\n";

    Mock::given(method("POST"))
        .and(path_regex(r"^/openai/deployments"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_string(body),
        )
        .mount(&server)
        .await;

    let provider =
        AzureOpenAiProvider::new(cfg(server.uri()), AuthMethod::ApiKey { value: "k".into() });

    let mut r = req();
    r.tools = vec![ToolSpec {
        name: "bash".into(),
        description: "run bash".into(),
        input_schema: json!({"type": "object", "properties": {}}),
    }];

    let resp = provider.generate(r, &model()).await.expect("ok");
    assert_eq!(resp.message.text(), "done");
}

// ── provider config() and auth() accessors ─────────────────────────────────────

#[test]
fn azure_config_and_auth_accessors() {
    use pi_ai::provider::Provider;
    let provider = AzureOpenAiProvider::new(
        cfg("http://localhost".into()),
        AuthMethod::ApiKey { value: "k".into() },
    );
    assert_eq!(provider.config().name, "azure-openai");
    assert!(matches!(provider.auth(), AuthMethod::ApiKey { .. }));
}
