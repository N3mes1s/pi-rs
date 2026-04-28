use futures::StreamExt;
use pi_ai::auth::AuthMethod;
use pi_ai::message::{Message, ThinkingLevel};
use pi_ai::provider::{GenerateRequest, GoogleProvider, Provider, ProviderKind};
use pi_ai::registry::{ModelInfo, ProviderConfig};
use pi_ai::stream::StreamEventKind;
use pi_ai::AiError;
use pi_ai::FinishReason;
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
        alias: Some("test".into()),
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

#[tokio::test]
async fn google_text_stream_captures_text() {
    let server = MockServer::start().await;
    let mut body = String::new();
    body.push_str("data: {\"candidates\":[{\"content\":{\"parts\":[{\"text\":\"Hello \"}]}}]}\n\n");
    body.push_str("data: {\"candidates\":[{\"content\":{\"parts\":[{\"text\":\"world\"}]}}]}\n\n");
    body.push_str("data: {\"candidates\":[{\"finishReason\":\"STOP\"}]}\n\n");
    Mock::given(method("POST"))
        .and(path("/v1beta/models/gemini-test:streamGenerateContent"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_string(body),
        )
        .mount(&server)
        .await;

    let provider = GoogleProvider::new(cfg(server.uri()), AuthMethod::ApiKey { value: "k".into() });
    let resp = provider.generate(req(), &model()).await.expect("ok");
    assert_eq!(resp.message.text(), "Hello world");
    assert!(matches!(resp.finish_reason, FinishReason::Stop));
}

#[tokio::test]
async fn google_function_call_emits_tool_call() {
    let server = MockServer::start().await;
    let mut body = String::new();
    body.push_str(
        "data: {\"candidates\":[{\"content\":{\"parts\":[{\"functionCall\":{\"name\":\"deploy\",\"args\":{\"target\":\"prod\"}}}]}}]}\n\n",
    );
    body.push_str("data: {\"candidates\":[{\"finishReason\":\"TOOL_USE\"}]}\n\n");
    Mock::given(method("POST"))
        .and(path("/v1beta/models/gemini-test:streamGenerateContent"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_string(body),
        )
        .mount(&server)
        .await;

    let provider = GoogleProvider::new(cfg(server.uri()), AuthMethod::ApiKey { value: "k".into() });
    let resp = provider.generate(req(), &model()).await.expect("ok");
    assert_eq!(resp.tool_calls.len(), 1);
    let c = &resp.tool_calls[0];
    assert_eq!(c.name, "deploy");
    assert_eq!(c.input, serde_json::json!({"target": "prod"}));
    assert!(matches!(resp.finish_reason, FinishReason::ToolUse));
}

#[tokio::test]
async fn google_5xx_yields_provider_error() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1beta/models/gemini-test:streamGenerateContent"))
        .respond_with(ResponseTemplate::new(500).set_body_string("boom"))
        .mount(&server)
        .await;
    let provider = GoogleProvider::new(cfg(server.uri()), AuthMethod::ApiKey { value: "k".into() });
    let err = provider.stream(req(), &model()).await.err().expect("error");
    match err {
        AiError::Provider { status, body } => {
            assert_eq!(status, 500);
            assert_eq!(body, "boom");
        }
        other => panic!("expected Provider error, got {other:?}"),
    }
}

#[tokio::test]
async fn google_safety_finish_reason_is_refusal() {
    let server = MockServer::start().await;
    let body = "data: {\"candidates\":[{\"finishReason\":\"SAFETY\"}]}\n\n".to_string();
    Mock::given(method("POST"))
        .and(path("/v1beta/models/gemini-test:streamGenerateContent"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_string(body),
        )
        .mount(&server)
        .await;
    let provider = GoogleProvider::new(cfg(server.uri()), AuthMethod::ApiKey { value: "k".into() });
    let mut s = provider.stream(req(), &model()).await.expect("ok");
    let mut found_refusal = false;
    while let Some(ev) = s.next().await {
        if let Ok(e) = ev {
            if matches!(
                e.kind,
                StreamEventKind::Finish {
                    reason: FinishReason::Refusal
                }
            ) {
                found_refusal = true;
                break;
            }
        }
    }
    assert!(found_refusal, "expected Refusal finish");
}

#[test]
fn google_message_to_parts_handles_all_block_types() {
    use pi_ai::message::{Attachment, AttachmentKind, ContentBlock};
    use pi_ai::provider::google::message_to_google_parts;
    let blocks = vec![
        ContentBlock::Text { text: "hi".into() },
        ContentBlock::Thinking {
            text: "ponder".into(),
            signature: None,
        },
        ContentBlock::ToolUse {
            id: "c1".into(),
            name: "ls".into(),
            input: serde_json::json!({"path": "/tmp"}),
        },
        ContentBlock::ToolResult {
            tool_use_id: "c1".into(),
            content: "ok".into(),
            is_error: false,
        },
        ContentBlock::Attachment {
            attachment: Attachment {
                kind: AttachmentKind::Image {
                    mime: "image/png".into(),
                    base64: "abc".into(),
                },
            },
        },
    ];
    let parts = message_to_google_parts(&blocks);
    assert_eq!(parts.len(), 5);
    assert_eq!(parts[0]["text"], "hi");
    assert!(parts[1]["text"].as_str().unwrap().contains("<thinking>"));
    assert_eq!(parts[2]["functionCall"]["name"], "ls");
    assert_eq!(parts[3]["functionResponse"]["name"], "c1");
    assert_eq!(parts[4]["inline_data"]["mime_type"], "image/png");
}

// --- RFD 0015: cumulative usageMetadata + single terminal Usage event ─

#[tokio::test]
async fn google_emits_single_cumulative_usage_at_terminal_chunk() {
    let server = MockServer::start().await;
    let mut body = String::new();
    // Chunk 1: text + partial usage.
    body.push_str(
        "data: {\"candidates\":[{\"content\":{\"parts\":[{\"text\":\"hi \"}]}}],\
\"usageMetadata\":{\"promptTokenCount\":10,\"candidatesTokenCount\":1}}\n\n",
    );
    // Chunk 2: more text + larger usage (cumulative).
    body.push_str(
        "data: {\"candidates\":[{\"content\":{\"parts\":[{\"text\":\"there\"}]}}],\
\"usageMetadata\":{\"promptTokenCount\":10,\"candidatesTokenCount\":3,\"cachedContentTokenCount\":4}}\n\n",
    );
    // Chunk 3: terminal — finishReason set, final cumulative usage.
    body.push_str(
        "data: {\"candidates\":[{\"finishReason\":\"STOP\"}],\
\"usageMetadata\":{\"promptTokenCount\":10,\"candidatesTokenCount\":7,\"cachedContentTokenCount\":4,\"thoughtsTokenCount\":2}}\n\n",
    );

    Mock::given(method("POST"))
        .and(path("/v1beta/models/gemini-test:streamGenerateContent"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_string(body),
        )
        .mount(&server)
        .await;

    let provider = GoogleProvider::new(cfg(server.uri()), AuthMethod::ApiKey { value: "k".into() });
    let mut s = provider.stream(req(), &model()).await.expect("ok");
    let mut usages = Vec::new();
    while let Some(ev) = s.next().await {
        if let Ok(e) = ev {
            if let StreamEventKind::Usage { usage } = e.kind {
                usages.push(usage);
            }
        }
    }
    assert_eq!(usages.len(), 1, "expected exactly one Usage event");
    let u = &usages[0];
    assert_eq!(u.input_tokens, 10);
    assert_eq!(u.output_tokens, 7);
    assert_eq!(u.cache_read_tokens, 4);
    assert_eq!(u.reasoning_tokens, 2);
}
