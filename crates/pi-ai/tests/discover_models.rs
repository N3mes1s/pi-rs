//! Tests for live model discovery against `/v1/models` (and equivalents).

use pi_ai::auth::AuthMethod;
use pi_ai::provider::{
    AnthropicProvider, GoogleProvider, OpenAiCompatProvider, OpenAiProvider, Provider, ProviderKind,
};
use pi_ai::registry::{ModelRegistry, ProviderConfig};
use pi_ai::{discovered_cache_path, DiscoveredCache};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn cfg(name: &str, kind: ProviderKind, base_url: String) -> ProviderConfig {
    ProviderConfig {
        name: name.into(),
        kind,
        base_url,
        auth_header: "Authorization".into(),
        auth_format: "Bearer {token}".into(),
        models: vec![],
    }
}

#[tokio::test]
async fn openai_discover_returns_data_array_models() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/models"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "object": "list",
            "data": [
                {"id": "gpt-4o", "context_length": 128000, "max_tokens": 16384},
                {"id": "gpt-5o", "context_window": 200000},
                {"id": "o1-pro"}
            ]
        })))
        .mount(&server)
        .await;

    let provider = OpenAiProvider::new(
        cfg("openai", ProviderKind::OpenAi, server.uri()),
        AuthMethod::ApiKey {
            value: "sk-test".into(),
        },
    );
    let models = provider.discover_models().await.expect("discover ok");
    assert_eq!(models.len(), 3);
    assert!(models.iter().any(|m| m.id == "gpt-4o"));
    assert!(models
        .iter()
        .any(|m| m.id == "gpt-5o" && m.context_window == 200000));
    let o1 = models.iter().find(|m| m.id == "o1-pro").unwrap();
    assert_eq!(o1.context_window, 8192); // default fallback
    assert_eq!(o1.max_output_tokens, 4096);
    for m in &models {
        assert_eq!(m.provider, "openai");
        assert!(m.alias.is_none());
    }
}

#[tokio::test]
async fn openai_compat_uses_same_endpoint() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/models"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "data": [{"id": "fireworks-only-model"}]
        })))
        .mount(&server)
        .await;
    let provider = OpenAiCompatProvider::new(
        cfg("fireworks", ProviderKind::OpenAiCompat, server.uri()),
        AuthMethod::ApiKey {
            value: "fw-test".into(),
        },
    );
    let models = provider.discover_models().await.unwrap();
    assert_eq!(models.len(), 1);
    assert_eq!(models[0].provider, "fireworks");
    assert_eq!(models[0].id, "fireworks-only-model");
}

#[tokio::test]
async fn anthropic_discover_uses_x_api_key_header() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/v1/models"))
        .and(wiremock::matchers::header("x-api-key", "sk-ant-test"))
        .and(wiremock::matchers::header(
            "anthropic-version",
            "2023-06-01",
        ))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "data": [
                {"id": "claude-opus-4-7", "type": "model", "display_name": "Claude Opus 4.7"},
                {"id": "claude-sonnet-4-6", "type": "model", "display_name": "Claude Sonnet 4.6"}
            ]
        })))
        .mount(&server)
        .await;
    let provider = AnthropicProvider::new(
        cfg("anthropic", ProviderKind::Anthropic, server.uri()),
        AuthMethod::ApiKey {
            value: "sk-ant-test".into(),
        },
    );
    let models = provider.discover_models().await.unwrap();
    assert_eq!(models.len(), 2);
    assert_eq!(models[0].context_window, 200_000);
    assert!(models.iter().any(|m| m.id == "claude-opus-4-7"));
}

#[tokio::test]
async fn google_strips_models_prefix_and_filters_non_generators() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/v1beta/models"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "models": [
                {
                    "name": "models/gemini-2.5-pro",
                    "inputTokenLimit": 1000000,
                    "outputTokenLimit": 8192,
                    "supportedGenerationMethods": ["generateContent", "streamGenerateContent"]
                },
                {
                    "name": "models/embedding-001",
                    "inputTokenLimit": 2048,
                    "outputTokenLimit": 1,
                    "supportedGenerationMethods": ["embedContent"]
                }
            ]
        })))
        .mount(&server)
        .await;
    let provider = GoogleProvider::new(
        cfg("google", ProviderKind::Google, server.uri()),
        AuthMethod::ApiKey {
            value: "g-test".into(),
        },
    );
    let models = provider.discover_models().await.unwrap();
    assert_eq!(models.len(), 1, "embedding-only models filtered out");
    assert_eq!(models[0].id, "gemini-2.5-pro"); // models/ prefix stripped
    assert_eq!(models[0].context_window, 1_000_000);
    assert_eq!(models[0].max_output_tokens, 8192);
}

#[tokio::test]
async fn discover_5xx_returns_provider_error() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/models"))
        .respond_with(ResponseTemplate::new(502).set_body_string("upstream borked"))
        .mount(&server)
        .await;
    let provider = OpenAiProvider::new(
        cfg("openai", ProviderKind::OpenAi, server.uri()),
        AuthMethod::ApiKey { value: "x".into() },
    );
    let err = provider.discover_models().await.expect_err("must err");
    assert!(format!("{err}").contains("502"), "got: {err}");
}

#[tokio::test]
async fn discover_with_no_auth_returns_missing_auth_error() {
    let provider = OpenAiProvider::new(
        cfg(
            "openai",
            ProviderKind::OpenAi,
            "https://nope.invalid".into(),
        ),
        AuthMethod::None,
    );
    let err = provider.discover_models().await.unwrap_err();
    assert!(format!("{err}").contains("missing credentials"));
}

#[test]
fn registry_merge_discovered_appends_new_only_keeps_static_on_conflict() {
    use pi_ai::registry::ModelInfo;
    use pi_ai::AuthStorage;

    let mut reg = ModelRegistry::new(AuthStorage::in_memory());
    // 'opus' is in the static catalogue. Try to merge a discovered entry
    // for the SAME id with different cost — static must win.
    let conflict = ModelInfo {
        provider: "anthropic".into(),
        id: "claude-opus-4-7".into(),
        alias: None,
        context_window: 100,
        max_output_tokens: 100,
        supports_thinking: false,
        supports_tools: false,
        supports_vision: false,
        input_cost_per_mtok: 999.0,
        output_cost_per_mtok: 999.0,
        cache_read_cost_per_mtok: None,
        cache_write_cost_per_mtok: None,
        api_kind: Default::default(),
    };
    let novel = ModelInfo {
        provider: "anthropic".into(),
        id: "claude-future-7-0".into(),
        alias: None,
        context_window: 200_000,
        max_output_tokens: 16_384,
        supports_thinking: true,
        supports_tools: true,
        supports_vision: true,
        input_cost_per_mtok: 0.0,
        output_cost_per_mtok: 0.0,
        cache_read_cost_per_mtok: None,
        cache_write_cost_per_mtok: None,
        api_kind: Default::default(),
    };
    let before_count = reg.total_models();
    reg.merge_discovered(vec![conflict, novel.clone()]);
    let after_count = reg.total_models();
    assert_eq!(
        after_count,
        before_count + 1,
        "only the novel id is appended"
    );
    let (_p, m) = reg.resolve("anthropic/claude-opus-4-7").unwrap();
    assert_eq!(m.input_cost_per_mtok, 5.0, "static cost preserved");
    let (_p, m) = reg.resolve("anthropic/claude-future-7-0").unwrap();
    assert_eq!(m.id, novel.id);
}

#[test]
fn discovered_cache_round_trip_to_disk() {
    let dir = tempfile::tempdir().unwrap();
    let path = discovered_cache_path(dir.path());

    use pi_ai::registry::ModelInfo;
    let mut cache = DiscoveredCache::default();
    cache.generated_at = 12345;
    cache.providers.insert(
        "openai".into(),
        vec![ModelInfo {
            provider: "openai".into(),
            id: "gpt-future".into(),
            alias: None,
            context_window: 50_000,
            max_output_tokens: 8192,
            supports_thinking: false,
            supports_tools: true,
            supports_vision: false,
            input_cost_per_mtok: 0.0,
            output_cost_per_mtok: 0.0,
            cache_read_cost_per_mtok: None,
            cache_write_cost_per_mtok: None,
            api_kind: Default::default(),
        }],
    );
    cache.save(&path).unwrap();
    let loaded = DiscoveredCache::load(&path);
    assert_eq!(loaded.generated_at, 12345);
    let flat = loaded.flatten();
    assert_eq!(flat.len(), 1);
    assert_eq!(flat[0].id, "gpt-future");
}

#[test]
fn discovered_cache_load_missing_file_returns_default() {
    let cache = DiscoveredCache::load(std::path::Path::new("/nonexistent/path"));
    assert_eq!(cache.generated_at, 0);
    assert!(cache.providers.is_empty());
}
