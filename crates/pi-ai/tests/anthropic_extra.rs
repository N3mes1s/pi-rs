//! Extra coverage for AnthropicProvider.
//!
//! Covers: AuthMethod::None → MissingAuth, OAuth token, with_client builder,
//! system message, temperature/max_tokens, tools, thinking levels,
//! content_blocks_to_anthropic all block types, thinking delta, usage event,
//! message_delta stop reason variants (max_tokens, refusal, other).

use futures::StreamExt;
use pi_ai::auth::AuthMethod;
use pi_ai::message::{ContentBlock, Message, Role, ThinkingLevel};
use pi_ai::provider::{AnthropicProvider, GenerateRequest, Provider, ProviderKind};
use pi_ai::provider::anthropic::content_blocks_to_anthropic;
use pi_ai::registry::{ModelInfo, ProviderConfig};
use pi_ai::stream::StreamEventKind;
use pi_ai::{AiError, FinishReason, ToolSpec};
use serde_json::json;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn cfg(base_url: String) -> ProviderConfig {
    ProviderConfig {
        name: "anthropic".into(),
        kind: ProviderKind::Anthropic,
        base_url,
        auth_header: "x-api-key".into(),
        auth_format: "{token}".into(),
        models: vec![],
    }
}

fn model() -> ModelInfo {
    ModelInfo {
        provider: "anthropic".into(),
        id: "claude-test".into(),
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
    }
}

fn req() -> GenerateRequest {
    GenerateRequest {
        model: "claude-test".into(),
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
async fn anthropic_no_auth_gives_missing_auth_error() {
    let provider = AnthropicProvider::new(cfg("http://localhost".into()), AuthMethod::None);
    let err = provider.stream(req(), &model()).await.err().expect("error");
    match err {
        AiError::MissingAuth(_) => {}
        other => panic!("expected MissingAuth, got {other:?}"),
    }
}

// ── OAuth token accepted ───────────────────────────────────────────────────────

#[tokio::test]
async fn anthropic_oauth_token_accepted() {
    let server = MockServer::start().await;
    let body = concat!(
        "event: message_start\n",
        "data: {\"type\":\"message_start\"}\n\n",
        "event: content_block_delta\n",
        "data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"ok\"}}\n\n",
        "event: message_stop\n",
        "data: {\"type\":\"message_stop\"}\n\n",
    );

    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_string(body),
        )
        .mount(&server)
        .await;

    let provider = AnthropicProvider::new(
        cfg(server.uri()),
        AuthMethod::OAuth {
            access_token: "oauth-tok".into(),
            refresh_token: None,
            expires_at: None,
        },
    );

    let resp = provider.generate(req(), &model()).await.expect("ok");
    assert_eq!(resp.message.text(), "ok");
}

// ── with_client builder ────────────────────────────────────────────────────────

#[test]
fn anthropic_with_client_builder() {
    let client = reqwest::Client::new();
    let provider =
        AnthropicProvider::new(cfg("http://localhost".into()), AuthMethod::None).with_client(client);
    assert_eq!(provider.config.name, "anthropic");
}

// ── System message forwarded ──────────────────────────────────────────────────

#[tokio::test]
async fn anthropic_system_message_forwarded() {
    let server = MockServer::start().await;
    let body = concat!(
        "event: message_start\n",
        "data: {\"type\":\"message_start\"}\n\n",
        "event: content_block_delta\n",
        "data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"hi\"}}\n\n",
        "event: message_stop\n",
        "data: {\"type\":\"message_stop\"}\n\n",
    );

    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_string(body),
        )
        .mount(&server)
        .await;

    let provider = AnthropicProvider::new(cfg(server.uri()), AuthMethod::ApiKey { value: "k".into() });
    let mut r = req();
    r.system = Some("You are helpful.".into());

    let resp = provider.generate(r, &model()).await.expect("ok");
    assert_eq!(resp.message.text(), "hi");
}

// ── Temperature and max_tokens forwarded ──────────────────────────────────────

#[tokio::test]
async fn anthropic_temperature_and_max_tokens_forwarded() {
    let server = MockServer::start().await;
    let body = concat!(
        "event: message_start\n",
        "data: {\"type\":\"message_start\"}\n\n",
        "event: message_stop\n",
        "data: {\"type\":\"message_stop\"}\n\n",
    );

    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_string(body),
        )
        .mount(&server)
        .await;

    let provider = AnthropicProvider::new(cfg(server.uri()), AuthMethod::ApiKey { value: "k".into() });
    let mut r = req();
    r.temperature = Some(0.5);
    r.max_output_tokens = Some(512);

    let _ = provider.generate(r, &model()).await.expect("ok");
}

// ── Tools forwarded ───────────────────────────────────────────────────────────

#[tokio::test]
async fn anthropic_tools_forwarded() {
    let server = MockServer::start().await;
    let body = concat!(
        "event: message_start\n",
        "data: {\"type\":\"message_start\"}\n\n",
        "event: message_stop\n",
        "data: {\"type\":\"message_stop\"}\n\n",
    );

    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_string(body),
        )
        .mount(&server)
        .await;

    let provider = AnthropicProvider::new(cfg(server.uri()), AuthMethod::ApiKey { value: "k".into() });
    let mut r = req();
    r.tools = vec![ToolSpec {
        name: "bash".into(),
        description: "run bash".into(),
        input_schema: json!({"type": "object", "properties": {}}),
    }];

    let _ = provider.generate(r, &model()).await.expect("ok");
}

// ── Thinking level Low ────────────────────────────────────────────────────────

#[tokio::test]
async fn anthropic_thinking_level_low_forwarded() {
    let server = MockServer::start().await;
    let body = concat!(
        "event: message_start\n",
        "data: {\"type\":\"message_start\"}\n\n",
        "event: message_stop\n",
        "data: {\"type\":\"message_stop\"}\n\n",
    );

    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_string(body),
        )
        .mount(&server)
        .await;

    let provider = AnthropicProvider::new(cfg(server.uri()), AuthMethod::ApiKey { value: "k".into() });
    let mut r = req();
    r.thinking = ThinkingLevel::Low;

    let _ = provider.generate(r, &model()).await.expect("ok");
}

// ── Thinking level Medium ─────────────────────────────────────────────────────

#[tokio::test]
async fn anthropic_thinking_level_medium_forwarded() {
    let server = MockServer::start().await;
    let body = concat!(
        "event: message_start\n",
        "data: {\"type\":\"message_start\"}\n\n",
        "event: message_stop\n",
        "data: {\"type\":\"message_stop\"}\n\n",
    );

    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_string(body),
        )
        .mount(&server)
        .await;

    let provider = AnthropicProvider::new(cfg(server.uri()), AuthMethod::ApiKey { value: "k".into() });
    let mut r = req();
    r.thinking = ThinkingLevel::Medium;

    let _ = provider.generate(r, &model()).await.expect("ok");
}

// ── Thinking level High ───────────────────────────────────────────────────────

#[tokio::test]
async fn anthropic_thinking_level_high_forwarded() {
    let server = MockServer::start().await;
    let body = concat!(
        "event: message_start\n",
        "data: {\"type\":\"message_start\"}\n\n",
        "event: message_stop\n",
        "data: {\"type\":\"message_stop\"}\n\n",
    );

    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_string(body),
        )
        .mount(&server)
        .await;

    let provider = AnthropicProvider::new(cfg(server.uri()), AuthMethod::ApiKey { value: "k".into() });
    let mut r = req();
    r.thinking = ThinkingLevel::High;

    let _ = provider.generate(r, &model()).await.expect("ok");
}

// ── ThinkingDelta event ───────────────────────────────────────────────────────

#[tokio::test]
async fn anthropic_thinking_delta_emitted() {
    let server = MockServer::start().await;
    let body = concat!(
        "event: message_start\n",
        "data: {\"type\":\"message_start\"}\n\n",
        "event: content_block_delta\n",
        "data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"thinking_delta\",\"thinking\":\"pondering\"}}\n\n",
        "event: message_stop\n",
        "data: {\"type\":\"message_stop\"}\n\n",
    );

    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_string(body),
        )
        .mount(&server)
        .await;

    let provider = AnthropicProvider::new(cfg(server.uri()), AuthMethod::ApiKey { value: "k".into() });
    let mut stream = provider.stream(req(), &model()).await.expect("ok");
    let mut saw_thinking = false;
    while let Some(ev) = stream.next().await {
        if let Ok(e) = ev {
            if matches!(&e.kind, StreamEventKind::ThinkingDelta { text } if text == "pondering") {
                saw_thinking = true;
            }
        }
    }
    assert!(saw_thinking, "expected ThinkingDelta event");
}

// ── Usage from message_delta ──────────────────────────────────────────────────

#[tokio::test]
async fn anthropic_usage_from_message_delta() {
    let server = MockServer::start().await;
    let body = concat!(
        "event: message_start\n",
        "data: {\"type\":\"message_start\"}\n\n",
        "event: message_delta\n",
        "data: {\"type\":\"message_delta\",\"usage\":{\"output_tokens\":77}}\n\n",
        "event: message_stop\n",
        "data: {\"type\":\"message_stop\"}\n\n",
    );

    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_string(body),
        )
        .mount(&server)
        .await;

    let provider = AnthropicProvider::new(cfg(server.uri()), AuthMethod::ApiKey { value: "k".into() });
    let resp = provider.generate(req(), &model()).await.expect("ok");
    assert_eq!(resp.usage.output_tokens, 77);
}

// ── stop_reason max_tokens → Length ───────────────────────────────────────────

#[tokio::test]
async fn anthropic_stop_reason_max_tokens_gives_length() {
    let server = MockServer::start().await;
    let body = concat!(
        "event: message_start\n",
        "data: {\"type\":\"message_start\"}\n\n",
        "event: message_delta\n",
        "data: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"max_tokens\"}}\n\n",
        "event: message_stop\n",
        "data: {\"type\":\"message_stop\"}\n\n",
    );

    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_string(body),
        )
        .mount(&server)
        .await;

    let provider = AnthropicProvider::new(cfg(server.uri()), AuthMethod::ApiKey { value: "k".into() });
    let mut stream = provider.stream(req(), &model()).await.expect("ok");
    let mut saw_length = false;
    while let Some(ev) = stream.next().await {
        if let Ok(e) = ev {
            if matches!(e.kind, StreamEventKind::Finish { reason: FinishReason::Length }) {
                saw_length = true;
            }
        }
    }
    assert!(saw_length, "expected Length finish reason for max_tokens");
}

// ── stop_reason refusal → Refusal ─────────────────────────────────────────────

#[tokio::test]
async fn anthropic_stop_reason_refusal() {
    let server = MockServer::start().await;
    let body = concat!(
        "event: message_start\n",
        "data: {\"type\":\"message_start\"}\n\n",
        "event: message_delta\n",
        "data: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"refusal\"}}\n\n",
        "event: message_stop\n",
        "data: {\"type\":\"message_stop\"}\n\n",
    );

    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_string(body),
        )
        .mount(&server)
        .await;

    let provider = AnthropicProvider::new(cfg(server.uri()), AuthMethod::ApiKey { value: "k".into() });
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

// ── stop_reason other → Other ─────────────────────────────────────────────────

#[tokio::test]
async fn anthropic_stop_reason_other() {
    let server = MockServer::start().await;
    let body = concat!(
        "event: message_start\n",
        "data: {\"type\":\"message_start\"}\n\n",
        "event: message_delta\n",
        "data: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"future_reason\"}}\n\n",
        "event: message_stop\n",
        "data: {\"type\":\"message_stop\"}\n\n",
    );

    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_string(body),
        )
        .mount(&server)
        .await;

    let provider = AnthropicProvider::new(cfg(server.uri()), AuthMethod::ApiKey { value: "k".into() });
    let mut stream = provider.stream(req(), &model()).await.expect("ok");
    let mut saw_other = false;
    while let Some(ev) = stream.next().await {
        if let Ok(e) = ev {
            if matches!(e.kind, StreamEventKind::Finish { reason: FinishReason::Other }) {
                saw_other = true;
            }
        }
    }
    assert!(saw_other, "expected Other finish reason for unknown stop_reason");
}

// ── content_block_start for text (non-tool) → no tool accumulation ────────────

#[tokio::test]
async fn anthropic_text_content_block_start_is_ignored() {
    let server = MockServer::start().await;
    let body = concat!(
        "event: message_start\n",
        "data: {\"type\":\"message_start\"}\n\n",
        "event: content_block_start\n",
        "data: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"text\"}}\n\n",
        "event: content_block_delta\n",
        "data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"hello\"}}\n\n",
        "event: message_stop\n",
        "data: {\"type\":\"message_stop\"}\n\n",
    );

    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_string(body),
        )
        .mount(&server)
        .await;

    let provider = AnthropicProvider::new(cfg(server.uri()), AuthMethod::ApiKey { value: "k".into() });
    let resp = provider.generate(req(), &model()).await.expect("ok");
    assert_eq!(resp.message.text(), "hello");
    assert!(resp.tool_calls.is_empty());
}

// ── content_blocks_to_anthropic: all block types ─────────────────────────────

#[test]
fn content_blocks_to_anthropic_text() {
    let blocks = vec![ContentBlock::Text { text: "hello".into() }];
    let val = content_blocks_to_anthropic(&blocks);
    let arr = val.as_array().unwrap();
    assert_eq!(arr[0]["type"], "text");
    assert_eq!(arr[0]["text"], "hello");
}

#[test]
fn content_blocks_to_anthropic_thinking_with_signature() {
    let blocks = vec![ContentBlock::Thinking {
        text: "think".into(),
        signature: Some("sig123".into()),
    }];
    let val = content_blocks_to_anthropic(&blocks);
    let arr = val.as_array().unwrap();
    assert_eq!(arr[0]["type"], "thinking");
    assert_eq!(arr[0]["signature"], "sig123");
}

#[test]
fn content_blocks_to_anthropic_tool_use() {
    let blocks = vec![ContentBlock::ToolUse {
        id: "id1".into(),
        name: "bash".into(),
        input: json!({"cmd": "ls"}),
    }];
    let val = content_blocks_to_anthropic(&blocks);
    let arr = val.as_array().unwrap();
    assert_eq!(arr[0]["type"], "tool_use");
    assert_eq!(arr[0]["name"], "bash");
}

#[test]
fn content_blocks_to_anthropic_tool_result() {
    let blocks = vec![ContentBlock::ToolResult {
        tool_use_id: "id1".into(),
        content: "result".into(),
        is_error: true,
    }];
    let val = content_blocks_to_anthropic(&blocks);
    let arr = val.as_array().unwrap();
    assert_eq!(arr[0]["type"], "tool_result");
    assert_eq!(arr[0]["is_error"], json!(true));
}

#[test]
fn content_blocks_to_anthropic_image_attachment() {
    use pi_ai::message::{Attachment, AttachmentKind};
    let blocks = vec![ContentBlock::Attachment {
        attachment: Attachment {
            kind: AttachmentKind::Image {
                mime: "image/png".into(),
                base64: "abc".into(),
            },
        },
    }];
    let val = content_blocks_to_anthropic(&blocks);
    let arr = val.as_array().unwrap();
    assert_eq!(arr[0]["type"], "image");
    assert_eq!(arr[0]["source"]["type"], "base64");
}

#[test]
fn content_blocks_to_anthropic_file_attachment() {
    use pi_ai::message::{Attachment, AttachmentKind};
    let blocks = vec![ContentBlock::Attachment {
        attachment: Attachment {
            kind: AttachmentKind::File {
                mime: "application/pdf".into(),
                base64: "xyz".into(),
                name: "doc.pdf".into(),
            },
        },
    }];
    let val = content_blocks_to_anthropic(&blocks);
    let arr = val.as_array().unwrap();
    assert_eq!(arr[0]["type"], "document");
    assert_eq!(arr[0]["name"], "doc.pdf");
}

// ── System-role messages are filtered from content ────────────────────────────

#[tokio::test]
async fn anthropic_system_role_message_filtered() {
    let server = MockServer::start().await;
    let body = concat!(
        "event: message_start\n",
        "data: {\"type\":\"message_start\"}\n\n",
        "event: content_block_delta\n",
        "data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"filtered\"}}\n\n",
        "event: message_stop\n",
        "data: {\"type\":\"message_stop\"}\n\n",
    );

    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_string(body),
        )
        .mount(&server)
        .await;

    let provider = AnthropicProvider::new(cfg(server.uri()), AuthMethod::ApiKey { value: "k".into() });
    let mut r = req();
    r.messages = vec![
        Message::system_text("system"),
        Message::user_text("user"),
    ];

    let resp = provider.generate(r, &model()).await.expect("ok");
    assert_eq!(resp.message.text(), "filtered");
}

// ── config() and auth() accessors ─────────────────────────────────────────────

#[test]
fn anthropic_config_and_auth_accessors() {
    let provider = AnthropicProvider::new(
        cfg("http://localhost".into()),
        AuthMethod::ApiKey { value: "k".into() },
    );
    assert_eq!(provider.config().name, "anthropic");
    assert!(matches!(provider.auth(), AuthMethod::ApiKey { .. }));
}
