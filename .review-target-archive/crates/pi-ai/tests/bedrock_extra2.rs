//! Extra coverage for BedrockAnthropicProvider — second round.
//!
//! Covers: unknown stop_reason → Other, content_block_stop for non-tool block,
//! multiple thinking levels in one run, message_delta without stop_reason.

use futures::StreamExt;
use pi_ai::auth::AuthMethod;
use pi_ai::message::{Message, ThinkingLevel};
use pi_ai::provider::{BedrockAnthropicProvider, GenerateRequest, Provider, ProviderKind};
use pi_ai::registry::{ModelInfo, ProviderConfig};
use pi_ai::stream::StreamEventKind;
use pi_ai::FinishReason;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn cfg(base_url: String) -> ProviderConfig {
    ProviderConfig {
        name: "bedrock".into(),
        kind: ProviderKind::Bedrock,
        base_url,
        auth_header: "Authorization".into(),
        auth_format: "Bearer {token}".into(),
        models: vec![],
    }
}

fn model() -> ModelInfo {
    ModelInfo {
        provider: "bedrock".into(),
        id: "anthropic.claude-test".into(),
        alias: None,
        context_window: 1024,
        max_output_tokens: 256,
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
        model: "anthropic.claude-test".into(),
        system: None,
        messages: vec![Message::user_text("hi")],
        tools: vec![],
        thinking: ThinkingLevel::Off,
        temperature: None,
        max_output_tokens: None,
        extras: serde_json::Value::Null,
    }
}

fn bedrock_path() -> &'static str {
    "/model/anthropic.claude-test/invoke-with-response-stream"
}

// ── Unknown stop_reason → Other ───────────────────────────────────────────────

#[tokio::test]
async fn bedrock_unknown_stop_reason_gives_other() {
    let server = MockServer::start().await;
    let body = concat!(
        "event: message_start\n",
        "data: {\"type\":\"message_start\"}\n\n",
        // Usage in separate event first.
        "event: message_delta\n",
        "data: {\"type\":\"message_delta\",\"usage\":{\"output_tokens\":1}}\n\n",
        // Stop reason in separate event (no usage field).
        "event: message_delta\n",
        "data: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"future_reason\"}}\n\n",
        "event: message_stop\n",
        "data: {\"type\":\"message_stop\"}\n\n",
    );

    Mock::given(method("POST"))
        .and(path(bedrock_path()))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_string(body),
        )
        .mount(&server)
        .await;

    let provider = BedrockAnthropicProvider::new(
        cfg(server.uri()),
        AuthMethod::ApiKey {
            value: "tok".into(),
        },
    );

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
    assert!(
        saw_other,
        "expected Other finish reason for unknown stop_reason"
    );
}

// ── refusal stop reason ───────────────────────────────────────────────────────

#[tokio::test]
async fn bedrock_refusal_stop_reason() {
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
        .and(path(bedrock_path()))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_string(body),
        )
        .mount(&server)
        .await;

    let provider = BedrockAnthropicProvider::new(
        cfg(server.uri()),
        AuthMethod::ApiKey {
            value: "tok".into(),
        },
    );

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

// ── content_block_stop for non-tool (empty acc) → no ToolCallComplete ─────────

#[tokio::test]
async fn bedrock_content_block_stop_without_tool_noop() {
    let server = MockServer::start().await;
    let body = concat!(
        "event: message_start\n",
        "data: {\"type\":\"message_start\"}\n\n",
        // content_block_stop for index 0, but no tool_use was started for index 0.
        "event: content_block_stop\n",
        "data: {\"type\":\"content_block_stop\",\"index\":0}\n\n",
        "event: content_block_delta\n",
        "data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"hi\"}}\n\n",
        "event: message_stop\n",
        "data: {\"type\":\"message_stop\"}\n\n",
    );

    Mock::given(method("POST"))
        .and(path(bedrock_path()))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_string(body),
        )
        .mount(&server)
        .await;

    let provider = BedrockAnthropicProvider::new(
        cfg(server.uri()),
        AuthMethod::ApiKey {
            value: "tok".into(),
        },
    );

    let resp = provider.generate(req(), &model()).await.expect("ok");
    // Should get text, no tool calls.
    assert_eq!(resp.message.text(), "hi");
    assert!(resp.tool_calls.is_empty());
}

// ── message_delta without stop_reason (only usage) ───────────────────────────

#[tokio::test]
async fn bedrock_message_delta_only_usage_no_finish() {
    let server = MockServer::start().await;
    let body = concat!(
        "event: message_start\n",
        "data: {\"type\":\"message_start\"}\n\n",
        "event: content_block_delta\n",
        "data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"text\"}}\n\n",
        // Usage without stop_reason.
        "event: message_delta\n",
        "data: {\"type\":\"message_delta\",\"usage\":{\"output_tokens\":5}}\n\n",
        "event: message_stop\n",
        "data: {\"type\":\"message_stop\"}\n\n",
    );

    Mock::given(method("POST"))
        .and(path(bedrock_path()))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_string(body),
        )
        .mount(&server)
        .await;

    let provider = BedrockAnthropicProvider::new(
        cfg(server.uri()),
        AuthMethod::ApiKey {
            value: "tok".into(),
        },
    );

    let resp = provider.generate(req(), &model()).await.expect("ok");
    assert_eq!(resp.message.text(), "text");
    assert_eq!(resp.usage.output_tokens, 5);
}

// ── max_tokens stop reason → Length ──────────────────────────────────────────

#[tokio::test]
async fn bedrock_max_tokens_stop_reason() {
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
        .and(path(bedrock_path()))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_string(body),
        )
        .mount(&server)
        .await;

    let provider = BedrockAnthropicProvider::new(
        cfg(server.uri()),
        AuthMethod::ApiKey {
            value: "tok".into(),
        },
    );

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
    assert!(saw_length, "expected Length finish reason for max_tokens");
}

// ── tool call with empty buf → empty object input ────────────────────────────

#[tokio::test]
async fn bedrock_tool_call_empty_buf_gives_empty_object() {
    let server = MockServer::start().await;
    let body = concat!(
        "event: message_start\n",
        "data: {\"type\":\"message_start\"}\n\n",
        "event: content_block_start\n",
        "data: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"tool_use\",\"id\":\"tu_empty\",\"name\":\"no_args\"}}\n\n",
        // content_block_stop immediately (no delta → empty buf).
        "event: content_block_stop\n",
        "data: {\"type\":\"content_block_stop\",\"index\":0}\n\n",
        "event: message_stop\n",
        "data: {\"type\":\"message_stop\"}\n\n",
    );

    Mock::given(method("POST"))
        .and(path(bedrock_path()))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_string(body),
        )
        .mount(&server)
        .await;

    let provider = BedrockAnthropicProvider::new(
        cfg(server.uri()),
        AuthMethod::ApiKey {
            value: "tok".into(),
        },
    );

    let resp = provider.generate(req(), &model()).await.expect("ok");
    assert_eq!(resp.tool_calls.len(), 1);
    assert_eq!(resp.tool_calls[0].name, "no_args");
    // Empty buf → Value::Object({}).
    assert!(resp.tool_calls[0].input.is_object());
}

// ── input_json_delta without matching acc entry is a no-op ───────────────────

#[tokio::test]
async fn bedrock_input_json_delta_without_acc_entry_noop() {
    let server = MockServer::start().await;
    let body = concat!(
        "event: message_start\n",
        "data: {\"type\":\"message_start\"}\n\n",
        // input_json_delta for index 99 (no matching tool start) → noop.
        "event: content_block_delta\n",
        "data: {\"type\":\"content_block_delta\",\"index\":99,\"delta\":{\"type\":\"input_json_delta\",\"partial_json\":\"{}\"}}\n\n",
        "event: content_block_delta\n",
        "data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"done\"}}\n\n",
        "event: message_stop\n",
        "data: {\"type\":\"message_stop\"}\n\n",
    );

    Mock::given(method("POST"))
        .and(path(bedrock_path()))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_string(body),
        )
        .mount(&server)
        .await;

    let provider = BedrockAnthropicProvider::new(
        cfg(server.uri()),
        AuthMethod::ApiKey {
            value: "tok".into(),
        },
    );

    let resp = provider.generate(req(), &model()).await.expect("ok");
    assert_eq!(resp.message.text(), "done");
}
