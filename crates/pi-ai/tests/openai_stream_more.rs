//! Extra coverage for the OpenAI streaming tool-call accumulator.
//!
//! - tool call whose `function.name` *and* `function.arguments` arrive
//!   in a single delta chunk (no split-args reassembly path);
//! - usage block delivered without `completion_tokens_details` — only
//!   the prompt/completion totals.

use pi_ai::auth::AuthMethod;
use pi_ai::message::{Message, ThinkingLevel};
use pi_ai::provider::{GenerateRequest, OpenAiProvider, Provider, ProviderKind};
use pi_ai::registry::{ModelInfo, ProviderConfig};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn provider_config(base_url: String) -> ProviderConfig {
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

#[tokio::test]
async fn openai_streams_tool_call_with_name_and_args_in_one_chunk() {
    let server = MockServer::start().await;

    // Single chunk carrying id + name + complete arguments JSON, then a
    // finish_reason of tool_calls, then [DONE]. The accumulator must
    // emit ToolCallComplete with the full input.
    let mut body = String::new();
    body.push_str(
        "data: {\"choices\":[{\"index\":0,\"delta\":{\"tool_calls\":[{\"index\":0,\"id\":\"call_42\",\"function\":{\"name\":\"do_thing\",\"arguments\":\"{\\\"x\\\":7}\"}}]}}]}\n\n",
    );
    body.push_str(
        "data: {\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"tool_calls\"}]}\n\n",
    );
    body.push_str("data: [DONE]\n\n");

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
        provider_config(server.uri()),
        AuthMethod::ApiKey {
            value: "dummy".into(),
        },
    );

    let resp = provider.generate(req(), &model()).await.expect("ok");
    assert_eq!(resp.tool_calls.len(), 1);
    let c = &resp.tool_calls[0];
    assert_eq!(c.id, "call_42");
    assert_eq!(c.name, "do_thing");
    assert_eq!(c.input, serde_json::json!({"x": 7}));
}

#[tokio::test]
async fn openai_usage_without_completion_tokens_details_defaults_reasoning_to_zero() {
    let server = MockServer::start().await;

    let mut body = String::new();
    body.push_str("data: {\"choices\":[{\"index\":0,\"delta\":{\"content\":\"ok\"}}]}\n\n");
    body.push_str(
        "data: {\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"stop\"}]}\n\n",
    );
    // Usage block intentionally omits `completion_tokens_details`.
    body.push_str(
        "data: {\"choices\":[],\"usage\":{\"prompt_tokens\":11,\"completion_tokens\":13,\"total_tokens\":24}}\n\n",
    );
    body.push_str("data: [DONE]\n\n");

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
        provider_config(server.uri()),
        AuthMethod::ApiKey {
            value: "dummy".into(),
        },
    );

    let resp = provider.generate(req(), &model()).await.expect("ok");
    assert_eq!(resp.message.text(), "ok");
    assert_eq!(resp.usage.input_tokens, 11);
    assert_eq!(resp.usage.output_tokens, 13);
    // completion_tokens_details was absent → reasoning_tokens defaults to 0.
    assert_eq!(resp.usage.reasoning_tokens, 0);
}
