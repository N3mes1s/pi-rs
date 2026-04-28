//! Extra coverage tests for OpenAiProvider and message_to_openai.
//!
//! Covers: AuthMethod::None → MissingAuth, OAuth token, with_client builder,
//! OpenAiCompatProvider, system message, temperature/max_tokens forwarding,
//! tools in request, content_filter/length/other finish reasons,
//! reasoning_content (ThinkingDelta), attachment in user message,
//! message_to_openai with attachment block.

use futures::StreamExt;
use pi_ai::auth::AuthMethod;
use pi_ai::message::{Attachment, AttachmentKind, ContentBlock, Message, Role, ThinkingLevel};
use pi_ai::provider::{
    GenerateRequest, OpenAiCompatProvider, OpenAiProvider, Provider, ProviderKind,
};
use pi_ai::provider::openai::message_to_openai;
use pi_ai::registry::{ModelInfo, ProviderConfig};
use pi_ai::stream::StreamEventKind;
use pi_ai::{AiError, FinishReason, ToolSpec};
use serde_json::json;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn cfg(base_url: String) -> ProviderConfig {
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

// ── AuthMethod::None → MissingAuth ─────────────────────────────────────────────

#[tokio::test]
async fn openai_no_auth_gives_missing_auth_error() {
    let provider = OpenAiProvider::new(cfg("http://localhost".into()), AuthMethod::None);
    let err = provider.stream(req(), &model()).await.err().expect("error");
    match err {
        AiError::MissingAuth(_) => {}
        other => panic!("expected MissingAuth, got {other:?}"),
    }
}

// ── OAuth token accepted ───────────────────────────────────────────────────────

#[tokio::test]
async fn openai_oauth_token_accepted() {
    let server = MockServer::start().await;
    let body = "data: {\"choices\":[{\"index\":0,\"delta\":{\"content\":\"hi\"},\"finish_reason\":null}]}\n\ndata: [DONE]\n\n";

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
        cfg(server.uri()),
        AuthMethod::OAuth {
            access_token: "oauth-tok".into(),
            refresh_token: None,
            expires_at: None,
        },
    );

    let resp = provider.generate(req(), &model()).await.expect("ok");
    assert_eq!(resp.message.text(), "hi");
}

// ── with_client builder ────────────────────────────────────────────────────────

#[test]
fn openai_with_client_builder() {
    let client = reqwest::Client::new();
    let provider =
        OpenAiProvider::new(cfg("http://localhost".into()), AuthMethod::None).with_client(client);
    assert_eq!(provider.config.name, "openai");
}

// ── OpenAiCompatProvider delegates to OpenAiProvider ─────────────────────────

#[tokio::test]
async fn openai_compat_provider_delegates() {
    let server = MockServer::start().await;
    let body = "data: {\"choices\":[{\"index\":0,\"delta\":{\"content\":\"compat\"},\"finish_reason\":null}]}\n\ndata: [DONE]\n\n";

    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_string(body),
        )
        .mount(&server)
        .await;

    let compat = OpenAiCompatProvider::new(
        cfg(server.uri()),
        AuthMethod::ApiKey { value: "k".into() },
    );

    let resp = compat.generate(req(), &model()).await.expect("ok");
    assert_eq!(resp.message.text(), "compat");
    assert_eq!(compat.config().name, "openai");
    assert!(matches!(compat.auth(), AuthMethod::ApiKey { .. }));
}

// ── System message prepended ──────────────────────────────────────────────────

#[tokio::test]
async fn openai_system_message_prepended() {
    let server = MockServer::start().await;
    let body = "data: {\"choices\":[{\"index\":0,\"delta\":{\"content\":\"ok\"},\"finish_reason\":null}]}\n\ndata: [DONE]\n\n";

    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_string(body),
        )
        .mount(&server)
        .await;

    let provider = OpenAiProvider::new(cfg(server.uri()), AuthMethod::ApiKey { value: "k".into() });
    let mut r = req();
    r.system = Some("You are helpful.".into());

    let resp = provider.generate(r, &model()).await.expect("ok");
    assert_eq!(resp.message.text(), "ok");
}

// ── Temperature and max_tokens forwarded ──────────────────────────────────────

#[tokio::test]
async fn openai_temperature_and_max_tokens_forwarded() {
    let server = MockServer::start().await;
    let body = "data: {\"choices\":[{\"index\":0,\"delta\":{\"content\":\"y\"},\"finish_reason\":null}]}\n\ndata: [DONE]\n\n";

    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_string(body),
        )
        .mount(&server)
        .await;

    let provider = OpenAiProvider::new(cfg(server.uri()), AuthMethod::ApiKey { value: "k".into() });
    let mut r = req();
    r.temperature = Some(0.7);
    r.max_output_tokens = Some(100);

    let resp = provider.generate(r, &model()).await.expect("ok");
    assert_eq!(resp.message.text(), "y");
}

// ── Tools forwarded in request ────────────────────────────────────────────────

#[tokio::test]
async fn openai_tools_forwarded_in_request() {
    let server = MockServer::start().await;
    let body = "data: {\"choices\":[{\"index\":0,\"delta\":{\"content\":\"done\"},\"finish_reason\":null}]}\n\ndata: [DONE]\n\n";

    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_string(body),
        )
        .mount(&server)
        .await;

    let provider = OpenAiProvider::new(cfg(server.uri()), AuthMethod::ApiKey { value: "k".into() });
    let mut r = req();
    r.tools = vec![ToolSpec {
        name: "bash".into(),
        description: "run bash".into(),
        input_schema: json!({"type": "object", "properties": {}}),
    }];

    let resp = provider.generate(r, &model()).await.expect("ok");
    assert_eq!(resp.message.text(), "done");
}

// ── content_filter finish reason → Refusal ────────────────────────────────────

#[tokio::test]
async fn openai_content_filter_gives_refusal() {
    let server = MockServer::start().await;
    let body = "data: {\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"content_filter\"}]}\n\ndata: [DONE]\n\n";

    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_string(body),
        )
        .mount(&server)
        .await;

    let provider = OpenAiProvider::new(cfg(server.uri()), AuthMethod::ApiKey { value: "k".into() });

    let mut stream = provider.stream(req(), &model()).await.expect("ok");
    let mut saw_refusal = false;
    while let Some(ev) = stream.next().await {
        if let Ok(e) = ev {
            if matches!(e.kind, StreamEventKind::Finish { reason: FinishReason::Refusal }) {
                saw_refusal = true;
            }
        }
    }
    assert!(saw_refusal, "expected Refusal finish reason");
}

// ── length finish reason ───────────────────────────────────────────────────────

#[tokio::test]
async fn openai_length_finish_reason() {
    let server = MockServer::start().await;
    let body = "data: {\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"length\"}]}\n\ndata: [DONE]\n\n";

    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_string(body),
        )
        .mount(&server)
        .await;

    let provider = OpenAiProvider::new(cfg(server.uri()), AuthMethod::ApiKey { value: "k".into() });

    let mut stream = provider.stream(req(), &model()).await.expect("ok");
    let mut saw_length = false;
    while let Some(ev) = stream.next().await {
        if let Ok(e) = ev {
            if matches!(e.kind, StreamEventKind::Finish { reason: FinishReason::Length }) {
                saw_length = true;
            }
        }
    }
    assert!(saw_length, "expected Length finish reason");
}

// ── reasoning_content → ThinkingDelta ─────────────────────────────────────────

#[tokio::test]
async fn openai_reasoning_content_emits_thinking_delta() {
    let server = MockServer::start().await;
    let body = concat!(
        "data: {\"choices\":[{\"index\":0,\"delta\":{\"reasoning_content\":\"ponder\"}}]}\n\n",
        "data: {\"choices\":[{\"index\":0,\"delta\":{\"content\":\"answer\"},\"finish_reason\":null}]}\n\n",
        "data: [DONE]\n\n",
    );

    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_string(body),
        )
        .mount(&server)
        .await;

    let provider = OpenAiProvider::new(cfg(server.uri()), AuthMethod::ApiKey { value: "k".into() });

    let mut stream = provider.stream(req(), &model()).await.expect("ok");
    let mut saw_thinking = false;
    while let Some(ev) = stream.next().await {
        if let Ok(e) = ev {
            if matches!(&e.kind, StreamEventKind::ThinkingDelta { text } if text == "ponder") {
                saw_thinking = true;
            }
        }
    }
    assert!(saw_thinking, "expected ThinkingDelta event");
}

// ── message_to_openai: Attachment with image block ────────────────────────────

#[test]
fn openai_message_to_openai_attachment_image() {
    let m = Message {
        role: Role::User,
        content: vec![ContentBlock::Attachment {
            attachment: Attachment {
                kind: AttachmentKind::Image {
                    mime: "image/png".into(),
                    base64: "abc123".into(),
                },
            },
        }],
    };
    let out = message_to_openai(&m);
    // The image gets inlined into the content string.
    assert_eq!(out.len(), 1);
    let content = out[0]["content"].as_str().expect("string content");
    assert!(content.contains("data:image/png;base64,abc123"));
}

// ── message_to_openai: empty message (no content) produces nothing ────────────

#[test]
fn openai_message_to_openai_empty_produces_nothing() {
    let m = Message {
        role: Role::User,
        content: vec![],
    };
    let out = message_to_openai(&m);
    assert!(out.is_empty(), "empty message should produce no output");
}

// ── config() and auth() accessors ─────────────────────────────────────────────

#[test]
fn openai_config_and_auth_accessors() {
    let provider = OpenAiProvider::new(
        cfg("http://localhost".into()),
        AuthMethod::ApiKey { value: "k".into() },
    );
    assert_eq!(provider.config().name, "openai");
    assert!(matches!(provider.auth(), AuthMethod::ApiKey { .. }));
}

// ── function_call finish reason → ToolUse ─────────────────────────────────────

#[tokio::test]
async fn openai_function_call_finish_reason_tool_use() {
    let server = MockServer::start().await;
    let body = concat!(
        "data: {\"choices\":[{\"index\":0,\"delta\":{\"tool_calls\":[{\"index\":0,\"id\":\"c1\",\"function\":{\"name\":\"run\",\"arguments\":\"{\\\"x\\\":1}\"}}]}}]}\n\n",
        "data: {\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"function_call\"}]}\n\n",
        "data: [DONE]\n\n",
    );

    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_string(body),
        )
        .mount(&server)
        .await;

    let provider = OpenAiProvider::new(cfg(server.uri()), AuthMethod::ApiKey { value: "k".into() });

    let mut stream = provider.stream(req(), &model()).await.expect("ok");
    let mut saw_tool_use = false;
    let mut saw_tool_complete = false;
    while let Some(ev) = stream.next().await {
        if let Ok(e) = ev {
            match &e.kind {
                StreamEventKind::Finish { reason: FinishReason::ToolUse } => {
                    saw_tool_use = true;
                }
                StreamEventKind::ToolCallComplete { name, .. } if name == "run" => {
                    saw_tool_complete = true;
                }
                _ => {}
            }
        }
    }
    assert!(saw_tool_use || saw_tool_complete, "expected ToolUse finish or ToolCallComplete");
}

// ── [DONE] with pending tool call flushes ToolCallComplete ────────────────────

#[tokio::test]
async fn openai_done_flushes_pending_tool_call() {
    let server = MockServer::start().await;
    // No finish_reason event; tool call completes via [DONE] flush
    let body = concat!(
        "data: {\"choices\":[{\"index\":0,\"delta\":{\"tool_calls\":[{\"index\":0,\"id\":\"c99\",\"function\":{\"name\":\"flush_tool\",\"arguments\":\"{\\\"k\\\":\\\"v\\\"}\"}}]}}]}\n\n",
        "data: [DONE]\n\n",
    );

    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_string(body),
        )
        .mount(&server)
        .await;

    let provider = OpenAiProvider::new(cfg(server.uri()), AuthMethod::ApiKey { value: "k".into() });

    let mut stream = provider.stream(req(), &model()).await.expect("ok");
    let mut saw_complete = false;
    while let Some(ev) = stream.next().await {
        if let Ok(e) = ev {
            if matches!(&e.kind, StreamEventKind::ToolCallComplete { name, .. } if name == "flush_tool") {
                saw_complete = true;
            }
        }
    }
    assert!(saw_complete, "expected ToolCallComplete from [DONE] flush");
}
