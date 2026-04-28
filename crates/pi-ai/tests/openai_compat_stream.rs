//! RFD 0016 — pin that the RFD-0015 `UsageAcc` plumbing also fires
//! when a stream is driven through `OpenAiCompatProvider` (the umbrella
//! provider for Cohere, Mistral, Z.ai, Kimi, Cerebras, Groq, xAI,
//! DeepSeek).
//!
//! Today `OpenAiCompatProvider::stream` delegates verbatim to
//! `OpenAiProvider::stream`, so the fix is inherited. A future refactor
//! that forks the compat path must keep this test green.

use pi_ai::auth::AuthMethod;
use pi_ai::message::{Message, ThinkingLevel};
use pi_ai::provider::{GenerateRequest, OpenAiCompatProvider, Provider, ProviderKind};
use pi_ai::registry::{ModelInfo, ProviderConfig};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn provider_config(base_url: String) -> ProviderConfig {
    ProviderConfig {
        // Pretend to be one of the eight downstream compat providers.
        name: "deepseek".into(),
        kind: ProviderKind::OpenAiCompat,
        base_url,
        auth_header: "Authorization".into(),
        auth_format: "Bearer {token}".into(),
        models: vec![],
    }
}

fn priced_model() -> ModelInfo {
    // gpt-5-style pricing so we can assert cost_usd > 0 against any
    // non-zero token count.
    ModelInfo {
        provider: "openai".into(),
        id: "gpt-5".into(),
        alias: Some("gpt-5".into()),
        context_window: 1024,
        max_output_tokens: 256,
        supports_thinking: true,
        supports_tools: true,
        supports_vision: false,
        input_cost_per_mtok: 1.25,
        output_cost_per_mtok: 10.0,
        cache_read_cost_per_mtok: None,
        cache_write_cost_per_mtok: None,
        api_kind: Default::default(),
    }
}

fn sse_body_full_usage() -> String {
    let mut s = String::new();
    s.push_str(
        "data: {\"choices\":[{\"index\":0,\"delta\":{\"role\":\"assistant\",\"content\":\"ok\"}}]}\n\n",
    );
    s.push_str("data: {\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"stop\"}]}\n\n");
    s.push_str(
        "data: {\"choices\":[],\"usage\":{\
\"prompt_tokens\":1234,\
\"completion_tokens\":56,\
\"prompt_tokens_details\":{\"cached_tokens\":100},\
\"completion_tokens_details\":{\"reasoning_tokens\":20}\
}}\n\n",
    );
    s.push_str("data: [DONE]\n\n");
    s
}

#[tokio::test]
async fn openai_compat_closing_chunk_populates_every_usage_field() {
    let server = MockServer::start().await;

    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_string(sse_body_full_usage()),
        )
        .mount(&server)
        .await;

    let provider = OpenAiCompatProvider::new(
        provider_config(server.uri()),
        AuthMethod::ApiKey { value: "k".into() },
    );

    let req = GenerateRequest {
        model: "gpt-5".into(),
        system: None,
        messages: vec![Message::user_text("hi")],
        tools: vec![],
        thinking: ThinkingLevel::Off,
        temperature: None,
        max_output_tokens: None,
        extras: serde_json::Value::Null,
    };

    let resp = provider
        .generate(req, &priced_model())
        .await
        .expect("compat generate ok");

    assert_eq!(resp.usage.input_tokens, 1234);
    assert_eq!(resp.usage.output_tokens, 56);
    assert_eq!(resp.usage.cache_read_tokens, 100);
    assert_eq!(resp.usage.reasoning_tokens, 20);
    assert!(
        resp.usage.cost_usd > 0.0,
        "cost should be > 0 for OpenAiCompatProvider too, got {}",
        resp.usage.cost_usd
    );
}
