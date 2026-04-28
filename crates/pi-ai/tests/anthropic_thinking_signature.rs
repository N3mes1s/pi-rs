//! Pin the Anthropic `thinking` block signature round-trip.
//!
//! Bug: pi-rs was streaming `signature_delta` events but discarding the
//! signature, so the resulting `ContentBlock::Thinking` had
//! `signature: None`. On the next turn, `content_blocks_to_anthropic`
//! serialised the block with `"signature": null`, which the Anthropic
//! API rejects with
//! `messages.*.content.*.thinking.signature.str: Input should be a
//! valid string`. Surface area: every multi-turn `--route auto` session
//! that landed on the `default` route (claude-sonnet-4-6 with medium
//! thinking) blew up on the second turn.
//!
//! Fix:
//!   1. anthropic_stream: emit `StreamEventKind::ThinkingSignature`
//!      when `signature_delta` arrives.
//!   2. provider::run_generate: stash the signature on the produced
//!      `ContentBlock::Thinking`.
//!   3. anthropic::content_blocks_to_anthropic: skip thinking blocks
//!      whose signature is None — better to lose one block than to
//!      poison the entire request with a null signature.

use pi_ai::auth::AuthMethod;
use pi_ai::message::{ContentBlock, Message, Role, ThinkingLevel};
use pi_ai::provider::{AnthropicProvider, GenerateRequest, Provider, ProviderKind};
use pi_ai::registry::{ModelInfo, ProviderConfig};
use wiremock::matchers::{header, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn provider_config(base_url: String) -> ProviderConfig {
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
        id: "claude-sonnet-4-6".into(),
        alias: Some("sonnet".into()),
        context_window: 200_000,
        max_output_tokens: 32_000,
        supports_thinking: true,
        supports_tools: true,
        supports_vision: true,
        input_cost_per_mtok: 3.0,
        output_cost_per_mtok: 15.0,
        cache_read_cost_per_mtok: None,
        cache_write_cost_per_mtok: None,
        api_kind: Default::default(),
        tier: 2,
    }
}

fn sse_body_with_signature() -> String {
    // Faithful sample of an Anthropic thinking-mode SSE: thinking_delta
    // chunks for the reasoning prose, then a signature_delta carrying
    // the opaque signature the API expects to see echoed back.
    let mut s = String::new();
    s.push_str("event: message_start\n");
    s.push_str("data: {\"type\":\"message_start\",\"message\":{\"id\":\"msg_1\",\"usage\":{\"input_tokens\":12}}}\n\n");

    s.push_str("event: content_block_start\n");
    s.push_str("data: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"thinking\",\"thinking\":\"\"}}\n\n");

    s.push_str("event: content_block_delta\n");
    s.push_str("data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"thinking_delta\",\"thinking\":\"plotting an answer\"}}\n\n");

    s.push_str("event: content_block_delta\n");
    s.push_str("data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"signature_delta\",\"signature\":\"OPAQUE_SIG_BLOB_001\"}}\n\n");

    s.push_str("event: content_block_stop\n");
    s.push_str("data: {\"type\":\"content_block_stop\",\"index\":0}\n\n");

    s.push_str("event: content_block_delta\n");
    s.push_str("data: {\"type\":\"content_block_delta\",\"index\":1,\"delta\":{\"type\":\"text_delta\",\"text\":\"Done.\"}}\n\n");

    s.push_str("event: message_delta\n");
    s.push_str("data: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"end_turn\"},\"usage\":{\"output_tokens\":7}}\n\n");

    s.push_str("event: message_stop\n");
    s.push_str("data: {\"type\":\"message_stop\"}\n\n");
    s
}

#[tokio::test]
async fn anthropic_stream_captures_thinking_signature_onto_content_block() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .and(header("x-api-key", "dummy-key"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_string(sse_body_with_signature()),
        )
        .mount(&server)
        .await;

    let provider = AnthropicProvider::new(
        provider_config(server.uri()),
        AuthMethod::ApiKey {
            value: "dummy-key".into(),
        },
    );

    let req = GenerateRequest {
        model: "claude-sonnet-4-6".into(),
        system: None,
        messages: vec![Message {
            role: Role::User,
            content: vec![ContentBlock::Text { text: "think hard".into() }],
        }],
        tools: vec![],
        thinking: ThinkingLevel::Medium,
        temperature: None,
        max_output_tokens: None,
        extras: serde_json::Value::Null,
    };

    let resp = provider.generate(req, &model()).await.expect("generate ok");
    let thinking_block = resp
        .message
        .content
        .iter()
        .find_map(|b| match b {
            ContentBlock::Thinking { text, signature } => Some((text.clone(), signature.clone())),
            _ => None,
        })
        .expect("response must contain a Thinking block");
    assert_eq!(thinking_block.0, "plotting an answer");
    assert_eq!(
        thinking_block.1.as_deref(),
        Some("OPAQUE_SIG_BLOB_001"),
        "signature_delta payload must round-trip onto ContentBlock::Thinking"
    );
}

#[test]
fn content_blocks_to_anthropic_skips_thinking_with_null_signature() {
    // When a thinking block lacks a signature (older session JSONLs,
    // synthesised content, or any code path that didn't capture
    // `signature_delta`), it must NOT be sent on the wire. Anthropic
    // rejects null signatures with a 400. The plain text + tool blocks
    // around it must still serialise.
    let blocks = vec![
        ContentBlock::Thinking {
            text: "unsigned reasoning".into(),
            signature: None,
        },
        ContentBlock::Text { text: "hello".into() },
    ];
    let v = pi_ai::provider::anthropic::content_blocks_to_anthropic(&blocks);
    let arr = v.as_array().expect("array");
    let types: Vec<&str> = arr
        .iter()
        .filter_map(|b| b.get("type").and_then(|t| t.as_str()))
        .collect();
    assert_eq!(types, vec!["text"], "thinking-without-signature must be dropped");
    assert_eq!(arr[0].get("text").and_then(|v| v.as_str()), Some("hello"));
}

#[test]
fn content_blocks_to_anthropic_keeps_thinking_with_real_signature() {
    let blocks = vec![ContentBlock::Thinking {
        text: "signed reasoning".into(),
        signature: Some("SIG_X".into()),
    }];
    let v = pi_ai::provider::anthropic::content_blocks_to_anthropic(&blocks);
    let arr = v.as_array().expect("array");
    assert_eq!(arr.len(), 1);
    assert_eq!(arr[0].get("type").and_then(|v| v.as_str()), Some("thinking"));
    assert_eq!(arr[0].get("thinking").and_then(|v| v.as_str()), Some("signed reasoning"));
    assert_eq!(arr[0].get("signature").and_then(|v| v.as_str()), Some("SIG_X"));
}
