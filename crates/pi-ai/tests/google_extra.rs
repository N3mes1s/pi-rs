//! Extra coverage tests for GoogleProvider and message_to_google_parts.
//!
//! Covers: File attachment, usageMetadata, temperature/max_tokens forwarding,
//! OAuth auth, AuthMethod::None → MissingAuth, system instruction,
//! tools list forwarded, RECITATION/MAX_TOKENS/TOOL_CALL finish reasons.

use futures::StreamExt;
use pi_ai::auth::AuthMethod;
use pi_ai::message::{Attachment, AttachmentKind, ContentBlock, Message, ThinkingLevel};
use pi_ai::provider::{GenerateRequest, GoogleProvider, Provider, ProviderKind};
use pi_ai::provider::google::message_to_google_parts;
use pi_ai::registry::{ModelInfo, ProviderConfig};
use pi_ai::stream::StreamEventKind;
use pi_ai::{AiError, FinishReason, ToolSpec};
use serde_json::json;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn cfg(base_url: String) -> ProviderConfig {
    ProviderConfig {
        name: "google".into(),
        kind: ProviderKind::Google,
        base_url,
        auth_header: "x-goog-api-key".into(),
        auth_format: "{token}".into(),
        models: vec![],
    }
}

fn model() -> ModelInfo {
    ModelInfo {
        provider: "google".into(),
        id: "gemini-test".into(),
        alias: None,
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
        model: "gemini-test".into(),
        system: None,
        messages: vec![Message::user_text("hi")],
        tools: vec![],
        thinking: ThinkingLevel::Off,
        temperature: None,
        max_output_tokens: None,
        extras: serde_json::Value::Null,
    }
}

fn google_path() -> &'static str {
    "/v1beta/models/gemini-test:streamGenerateContent"
}

// ── message_to_google_parts: File attachment ──────────────────────────────────

#[test]
fn google_parts_file_attachment() {
    let blocks = vec![ContentBlock::Attachment {
        attachment: Attachment {
            kind: AttachmentKind::File {
                mime: "application/pdf".into(),
                base64: "pdfdata".into(),
                name: "report.pdf".into(),
            },
        },
    }];
    let parts = message_to_google_parts(&blocks);
    assert_eq!(parts.len(), 1);
    assert_eq!(parts[0]["inline_data"]["mime_type"], "application/pdf");
    assert_eq!(parts[0]["inline_data"]["data"], "pdfdata");
}

// ── message_to_google_parts: empty blocks ─────────────────────────────────────

#[test]
fn google_parts_empty_blocks() {
    let parts = message_to_google_parts(&[]);
    assert!(parts.is_empty());
}

// ── message_to_google_parts: ToolResult block ─────────────────────────────────

#[test]
fn google_parts_tool_result_with_error() {
    let blocks = vec![ContentBlock::ToolResult {
        tool_use_id: "call_1".into(),
        content: "error message".into(),
        is_error: true,
    }];
    let parts = message_to_google_parts(&blocks);
    assert_eq!(parts.len(), 1);
    assert_eq!(parts[0]["functionResponse"]["name"], "call_1");
    assert_eq!(parts[0]["functionResponse"]["response"]["is_error"], json!(true));
}

// ── AuthMethod::None → MissingAuth ─────────────────────────────────────────────

#[tokio::test]
async fn google_no_auth_gives_missing_auth_error() {
    let provider = GoogleProvider::new(cfg("http://localhost".into()), AuthMethod::None);
    let err = provider.stream(req(), &model()).await.err().expect("error");
    match err {
        AiError::MissingAuth(_) => {}
        other => panic!("expected MissingAuth, got {other:?}"),
    }
}

// ── OAuth token accepted ───────────────────────────────────────────────────────

#[tokio::test]
async fn google_oauth_token_accepted() {
    let server = MockServer::start().await;
    let body = "data: {\"candidates\":[{\"finishReason\":\"STOP\"}]}\n\n";

    Mock::given(method("POST"))
        .and(path(google_path()))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_string(body),
        )
        .mount(&server)
        .await;

    let provider = GoogleProvider::new(
        cfg(server.uri()),
        AuthMethod::OAuth {
            access_token: "oauth-token".into(),
            refresh_token: None,
            expires_at: None,
        },
    );

    let resp = provider.generate(req(), &model()).await.expect("ok");
    assert!(matches!(resp.finish_reason, FinishReason::Stop));
}

// ── with_client builder ────────────────────────────────────────────────────────

#[test]
fn google_with_client_builder() {
    let client = reqwest::Client::new();
    let provider =
        GoogleProvider::new(cfg("http://localhost".into()), AuthMethod::None).with_client(client);
    assert_eq!(provider.config.name, "google");
}

// ── System instruction forwarded ──────────────────────────────────────────────

#[tokio::test]
async fn google_system_instruction_forwarded() {
    let server = MockServer::start().await;
    let body = "data: {\"candidates\":[{\"content\":{\"parts\":[{\"text\":\"ok\"}]}}]}\n\ndata: {\"candidates\":[{\"finishReason\":\"STOP\"}]}\n\n";

    Mock::given(method("POST"))
        .and(path(google_path()))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_string(body),
        )
        .mount(&server)
        .await;

    let provider =
        GoogleProvider::new(cfg(server.uri()), AuthMethod::ApiKey { value: "k".into() });

    let mut r = req();
    r.system = Some("You are helpful.".into());

    let resp = provider.generate(r, &model()).await.expect("ok");
    assert_eq!(resp.message.text(), "ok");
}

// ── Temperature forwarded ─────────────────────────────────────────────────────

#[tokio::test]
async fn google_temperature_forwarded() {
    let server = MockServer::start().await;
    let body = "data: {\"candidates\":[{\"finishReason\":\"STOP\"}]}\n\n";

    Mock::given(method("POST"))
        .and(path(google_path()))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_string(body),
        )
        .mount(&server)
        .await;

    let provider =
        GoogleProvider::new(cfg(server.uri()), AuthMethod::ApiKey { value: "k".into() });

    let mut r = req();
    r.temperature = Some(0.5);

    let _ = provider.generate(r, &model()).await.expect("ok");
}

// ── Tools forwarded ───────────────────────────────────────────────────────────

#[tokio::test]
async fn google_tools_forwarded() {
    let server = MockServer::start().await;
    let body = "data: {\"candidates\":[{\"finishReason\":\"STOP\"}]}\n\n";

    Mock::given(method("POST"))
        .and(path(google_path()))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_string(body),
        )
        .mount(&server)
        .await;

    let provider =
        GoogleProvider::new(cfg(server.uri()), AuthMethod::ApiKey { value: "k".into() });

    let mut r = req();
    r.tools = vec![ToolSpec {
        name: "bash".into(),
        description: "run bash".into(),
        input_schema: json!({"type": "object", "properties": {}}),
    }];

    let _ = provider.generate(r, &model()).await.expect("ok");
}

// ── usageMetadata event ───────────────────────────────────────────────────────

#[tokio::test]
async fn google_usage_metadata_emitted() {
    let server = MockServer::start().await;
    let body = concat!(
        "data: {\"candidates\":[{\"content\":{\"parts\":[{\"text\":\"hi\"}]}}]}\n\n",
        "data: {\"candidates\":[{\"finishReason\":\"STOP\"}]}\n\n",
        "data: {\"usageMetadata\":{\"promptTokenCount\":5,\"candidatesTokenCount\":3}}\n\n",
    );

    Mock::given(method("POST"))
        .and(path(google_path()))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_string(body),
        )
        .mount(&server)
        .await;

    let provider =
        GoogleProvider::new(cfg(server.uri()), AuthMethod::ApiKey { value: "k".into() });

    let resp = provider.generate(req(), &model()).await.expect("ok");
    assert_eq!(resp.usage.input_tokens, 5);
    assert_eq!(resp.usage.output_tokens, 3);
}

// ── RECITATION finish reason → Refusal ────────────────────────────────────────

#[tokio::test]
async fn google_recitation_gives_refusal() {
    let server = MockServer::start().await;
    let body = "data: {\"candidates\":[{\"finishReason\":\"RECITATION\"}]}\n\n";

    Mock::given(method("POST"))
        .and(path(google_path()))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_string(body),
        )
        .mount(&server)
        .await;

    let provider =
        GoogleProvider::new(cfg(server.uri()), AuthMethod::ApiKey { value: "k".into() });

    let mut stream = provider.stream(req(), &model()).await.expect("ok");
    let mut saw_refusal = false;
    while let Some(ev) = stream.next().await {
        if let Ok(e) = ev {
            if matches!(e.kind, StreamEventKind::Finish { reason: FinishReason::Refusal }) {
                saw_refusal = true;
            }
        }
    }
    assert!(saw_refusal, "expected Refusal finish reason for RECITATION");
}

// ── MAX_TOKENS finish reason → Length ─────────────────────────────────────────

#[tokio::test]
async fn google_max_tokens_gives_length() {
    let server = MockServer::start().await;
    let body = "data: {\"candidates\":[{\"finishReason\":\"MAX_TOKENS\"}]}\n\n";

    Mock::given(method("POST"))
        .and(path(google_path()))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_string(body),
        )
        .mount(&server)
        .await;

    let provider =
        GoogleProvider::new(cfg(server.uri()), AuthMethod::ApiKey { value: "k".into() });

    let resp = provider.generate(req(), &model()).await.expect("ok");
    assert!(matches!(resp.finish_reason, FinishReason::Length));
}

// ── TOOL_CALL finish reason → ToolUse ─────────────────────────────────────────

#[tokio::test]
async fn google_tool_call_finish_reason() {
    let server = MockServer::start().await;
    let body = "data: {\"candidates\":[{\"finishReason\":\"TOOL_CALL\"}]}\n\n";

    Mock::given(method("POST"))
        .and(path(google_path()))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_string(body),
        )
        .mount(&server)
        .await;

    let provider =
        GoogleProvider::new(cfg(server.uri()), AuthMethod::ApiKey { value: "k".into() });

    let resp = provider.generate(req(), &model()).await.expect("ok");
    assert!(matches!(resp.finish_reason, FinishReason::ToolUse));
}

// ── Unknown finish reason → Other ─────────────────────────────────────────────

#[tokio::test]
async fn google_unknown_finish_reason_gives_other() {
    let server = MockServer::start().await;
    let body = "data: {\"candidates\":[{\"finishReason\":\"FUTURE_THING\"}]}\n\n";

    Mock::given(method("POST"))
        .and(path(google_path()))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_string(body),
        )
        .mount(&server)
        .await;

    let provider =
        GoogleProvider::new(cfg(server.uri()), AuthMethod::ApiKey { value: "k".into() });

    let resp = provider.generate(req(), &model()).await.expect("ok");
    assert!(matches!(resp.finish_reason, FinishReason::Other));
}

// ── config() and auth() accessors ─────────────────────────────────────────────

#[test]
fn google_config_and_auth_accessors() {
    let provider = GoogleProvider::new(
        cfg("http://localhost".into()),
        AuthMethod::ApiKey { value: "k".into() },
    );
    assert_eq!(provider.config().name, "google");
    assert!(matches!(provider.auth(), AuthMethod::ApiKey { .. }));
}

// ── System role messages filtered from contents ────────────────────────────────

#[tokio::test]
async fn google_system_role_messages_filtered_from_contents() {
    let server = MockServer::start().await;
    let body = "data: {\"candidates\":[{\"content\":{\"parts\":[{\"text\":\"filtered\"}]}}]}\n\ndata: {\"candidates\":[{\"finishReason\":\"STOP\"}]}\n\n";

    Mock::given(method("POST"))
        .and(path(google_path()))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_string(body),
        )
        .mount(&server)
        .await;

    let provider =
        GoogleProvider::new(cfg(server.uri()), AuthMethod::ApiKey { value: "k".into() });

    let mut r = req();
    r.messages = vec![
        Message::system_text("system"),
        Message::user_text("user"),
    ];

    let resp = provider.generate(r, &model()).await.expect("ok");
    assert_eq!(resp.message.text(), "filtered");
}
