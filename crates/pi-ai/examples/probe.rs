use futures::StreamExt;
use pi_ai::registry::ModelInfo;
use pi_ai::{
    AnthropicProvider, AuthMethod, GenerateRequest, Message, ProviderConfig, ProviderKind,
    ThinkingLevel,
};

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt::init();
    let key = std::env::var("ANTHROPIC_API_KEY").expect("ANTHROPIC_API_KEY");
    tracing::warn!("[probe] starting");
    let cfg = ProviderConfig {
        name: "anthropic".into(),
        kind: ProviderKind::Anthropic,
        base_url: "https://api.anthropic.com".into(),
        auth_header: "x-api-key".into(),
        auth_format: "{token}".into(),
        models: vec![],
    };
    let model = ModelInfo {
        provider: "anthropic".into(),
        id: "claude-haiku-4-5-20251001".into(),
        alias: Some("haiku".into()),
        context_window: 200_000,
        max_output_tokens: 1024,
        tier: 1,
        supports_thinking: false,
        supports_tools: true,
        supports_vision: true,
        input_cost_per_mtok: 0.0,
        output_cost_per_mtok: 0.0,
        cache_read_cost_per_mtok: None,
        cache_write_cost_per_mtok: None,
        api_kind: Default::default(),
    };
    let provider = AnthropicProvider::new(cfg, AuthMethod::ApiKey { value: key });
    let req = GenerateRequest {
        model: model.id.clone(),
        system: Some("be brief".into()),
        messages: vec![Message::user_text("hi")],
        tools: vec![],
        thinking: ThinkingLevel::Off,
        temperature: None,
        max_output_tokens: Some(64),
        extras: serde_json::Value::Null,
    };
    tracing::warn!("[probe] doing direct reqwest test");
    let client = reqwest::Client::new();
    match client
        .get("https://api.anthropic.com/v1/health")
        .send()
        .await
    {
        Ok(r) => tracing::warn!("[probe] reqwest GET ok: {}", r.status()),
        Err(e) => {
            tracing::warn!("[probe] reqwest GET error: {e:?}");
            if let Some(src) = std::error::Error::source(&e) {
                tracing::warn!("[probe]   source: {src:?}");
                if let Some(s2) = std::error::Error::source(src) {
                    tracing::warn!("[probe]   source2: {s2:?}");
                }
            }
        }
    }
    tracing::warn!("[probe] calling stream");
    let mut s = match pi_ai::Provider::stream(&provider, req, &model).await {
        Ok(s) => s,
        Err(e) => {
            tracing::warn!("[probe] stream error: {e}");
            return;
        }
    };
    tracing::warn!("[probe] got stream, polling");
    let mut count = 0;
    while let Some(ev) = s.next().await {
        count += 1;
        tracing::warn!("[probe] event #{count}: {:?}", ev.map(|e| e.kind));
    }
    tracing::warn!("[probe] stream ended after {count} events");
}
