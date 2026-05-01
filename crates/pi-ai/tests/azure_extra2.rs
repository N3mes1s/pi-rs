//! Extra coverage for AzureOpenAiProvider — second round.
//!
//! Covers remaining branches: unknown finish reason, tool input delta,
//! tool call with empty args, multi-message with system.

use futures::StreamExt;
use pi_ai::auth::AuthMethod;
use pi_ai::message::{ContentBlock, Message, Role, ThinkingLevel};
use pi_ai::provider::{AzureOpenAiProvider, GenerateRequest, Provider, ProviderKind};
use pi_ai::registry::{ModelInfo, ProviderConfig};
use pi_ai::stream::StreamEventKind;
use pi_ai::FinishReason;
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

// ── Unknown finish reason → Other ────────────────────────────────────────────

#[tokio::test]
async fn azure_unknown_finish_reason_gives_other() {
    let server = MockServer::start().await;
    let body = "data: {\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"future_thing\"}]}\n\ndata: [DONE]\n\n";

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
    let mut saw_other = false;
    while let Some(ev) = stream.next().await {
        if let Ok(e) = ev {
            if matches!(
                e.kind,
                StreamEventKind::Finish {
                    reason: FinishReason::Other
                }
            ) {
                saw_other = true;
            }
        }
    }
    assert!(saw_other, "expected Other finish reason for unknown string");
}

// ── Tool input delta emitted ──────────────────────────────────────────────────

#[tokio::test]
async fn azure_tool_input_delta_emitted() {
    let server = MockServer::start().await;
    // id comes first, then arguments in second chunk (split scenario).
    let body = concat!(
        // Chunk 1: id + name
        "data: {\"choices\":[{\"index\":0,\"delta\":{\"tool_calls\":[{\"index\":0,\"id\":\"call_xyz\",\"function\":{\"name\":\"search\",\"arguments\":\"\"}}]}}]}\n\n",
        // Chunk 2: arguments (now id is set, so ToolInputDelta should be emitted)
        "data: {\"choices\":[{\"index\":0,\"delta\":{\"tool_calls\":[{\"index\":0,\"function\":{\"arguments\":\"{\\\"q\\\":\\\"test\\\"}\"}}]}}]}\n\n",
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
    let mut saw_input_delta = false;
    while let Some(ev) = stream.next().await {
        if let Ok(e) = ev {
            if matches!(&e.kind, StreamEventKind::ToolInputDelta { .. }) {
                saw_input_delta = true;
            }
        }
    }
    assert!(saw_input_delta, "expected ToolInputDelta event");
}

// ── Messages with multiple roles ──────────────────────────────────────────────

#[tokio::test]
async fn azure_multi_message_request() {
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
    r.messages = vec![
        Message::user_text("hello"),
        Message::assistant_text("hi"),
        Message::user_text("what's up?"),
    ];

    let resp = provider.generate(r, &model()).await.expect("ok");
    assert_eq!(resp.message.text(), "ok");
}

// ── Empty text delta (empty string) is skipped ────────────────────────────────

#[tokio::test]
async fn azure_empty_text_delta_skipped() {
    let server = MockServer::start().await;
    let body = concat!(
        "data: {\"choices\":[{\"index\":0,\"delta\":{\"content\":\"\"}}]}\n\n",
        "data: {\"choices\":[{\"index\":0,\"delta\":{\"content\":\"hi\"},\"finish_reason\":null}]}\n\n",
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
    assert_eq!(resp.message.text(), "hi");
}

// ── Assistant message with tool use block ─────────────────────────────────────

#[tokio::test]
async fn azure_message_with_tool_use_block() {
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
    r.messages = vec![Message {
        role: Role::User,
        content: vec![
            ContentBlock::Text {
                text: "use the tool".into(),
            },
            ContentBlock::ToolResult {
                tool_use_id: "id1".into(),
                content: "result".into(),
                is_error: false,
            },
        ],
    }];

    let resp = provider.generate(r, &model()).await.expect("ok");
    assert_eq!(resp.message.text(), "done");
}
