use pi_ai::auth::AuthStorage;
use pi_ai::provider::ProviderKind;
use pi_ai::registry::{ModelInfo, ModelRegistry, ProviderConfig};

#[test]
fn new_registers_anthropic_openai_and_fireworks() {
    let reg = ModelRegistry::new(AuthStorage::in_memory());
    let names: Vec<&str> = reg.providers().map(|p| p.name.as_str()).collect();
    assert!(names.contains(&"anthropic"));
    assert!(names.contains(&"openai"));
    assert!(names.contains(&"fireworks"));
    assert!(reg.get("anthropic").is_some());
    assert!(reg.get("openai").is_some());
    assert!(reg.get("fireworks").is_some());
}

#[test]
fn resolve_provider_slash_id() {
    let reg = ModelRegistry::new(AuthStorage::in_memory());
    let (p, m) = reg.resolve("anthropic/claude-opus-4-7").expect("resolve");
    assert_eq!(p.name, "anthropic");
    assert_eq!(m.id, "claude-opus-4-7");
}

#[test]
fn resolve_provider_slash_alias() {
    let reg = ModelRegistry::new(AuthStorage::in_memory());
    let (p, m) = reg.resolve("anthropic/sonnet").expect("resolve");
    assert_eq!(p.name, "anthropic");
    assert_eq!(m.alias.as_deref(), Some("sonnet"));
    assert_eq!(m.id, "claude-sonnet-4-6");
}

#[test]
fn resolve_bare_alias() {
    let reg = ModelRegistry::new(AuthStorage::in_memory());
    let (p, m) = reg.resolve("haiku").expect("resolve haiku");
    assert_eq!(p.name, "anthropic");
    assert_eq!(m.id, "claude-haiku-4-5-20251001");

    let (p, m) = reg.resolve("gpt-4o-mini").expect("resolve gpt-4o-mini");
    assert_eq!(p.name, "openai");
    assert_eq!(m.id, "gpt-4o-mini");
}

#[test]
fn resolve_full_id() {
    let reg = ModelRegistry::new(AuthStorage::in_memory());

    // Resolving the canonical Anthropic full id (no slash → bare alias-or-id
    // path) works.
    let (p, m) = reg
        .resolve("claude-haiku-4-5-20251001")
        .expect("resolve anthropic full id");
    assert_eq!(p.name, "anthropic");
    assert_eq!(m.id, "claude-haiku-4-5-20251001");

    // openai full id (no slash) resolves too.
    let (p, m) = reg.resolve("o1-mini").expect("resolve openai full id");
    assert_eq!(p.name, "openai");
    assert_eq!(m.id, "o1-mini");
}

#[test]
fn resolve_with_slash_short_circuits_when_first_segment_isnt_a_provider() {
    // resolve() splits on the first `/`, takes the LHS as the provider
    // name, and returns None outright if that provider doesn't exist.
    // Fireworks model ids contain slashes, so they cannot be resolved
    // by passing the bare id; callers must use "fireworks/<alias>".
    let reg = ModelRegistry::new(AuthStorage::in_memory());
    assert!(reg
        .resolve("accounts/fireworks/models/deepseek-r1")
        .is_none());
}

#[test]
fn resolve_unknown_returns_none() {
    let reg = ModelRegistry::new(AuthStorage::in_memory());
    assert!(reg.resolve("nope").is_none());
    assert!(reg.resolve("anthropic/does-not-exist").is_none());
    assert!(reg.resolve("nope/sonnet").is_none());
}

#[test]
fn install_adds_provider_retrievable_via_get() {
    let mut reg = ModelRegistry::new(AuthStorage::in_memory());
    let cfg = ProviderConfig {
        name: "myhost".into(),
        kind: ProviderKind::OpenAiCompat,
        base_url: "http://localhost:9999/v1".into(),
        auth_header: "Authorization".into(),
        auth_format: "Bearer {token}".into(),
        models: vec![ModelInfo {
            provider: "myhost".into(),
            id: "my-model".into(),
            alias: Some("mine".into()),
            context_window: 8192,
            max_output_tokens: 1024,
            supports_thinking: false,
            supports_tools: true,
            supports_vision: false,
            input_cost_per_mtok: 0.0,
            output_cost_per_mtok: 0.0,
        }],
    };
    reg.install(cfg);
    let got = reg.get("myhost").expect("get installed");
    assert_eq!(got.base_url, "http://localhost:9999/v1");
    let (p, m) = reg.resolve("mine").expect("resolve installed alias");
    assert_eq!(p.name, "myhost");
    assert_eq!(m.id, "my-model");
}

#[test]
fn auth_accessor_returns_storage() {
    let auth = AuthStorage::in_memory();
    let reg = ModelRegistry::new(auth);
    // Just ensure it compiles and is callable.
    assert!(reg.auth().provider_names().is_empty());
}

// ── E1: reasoning-family additions (o3, o3-pro, o4-mini, gpt-5*) ──────────

#[test]
fn registry_includes_o3_o3_pro_and_o4_mini() {
    let reg = pi_ai::ModelRegistry::new(pi_ai::AuthStorage::in_memory());
    for id in &["o3", "o3-pro", "o4-mini"] {
        let (provider, m) = reg
            .resolve(&format!("openai/{id}"))
            .unwrap_or_else(|| panic!("missing openai/{id}"));
        assert_eq!(provider.name, "openai");
        assert_eq!(m.id, *id);
        assert!(m.supports_thinking, "{id} should advertise supports_thinking");
    }
}

#[test]
fn registry_includes_gpt_5_family_with_thinking() {
    let reg = pi_ai::ModelRegistry::new(pi_ai::AuthStorage::in_memory());
    for id in &["gpt-5", "gpt-5-mini", "gpt-5-nano"] {
        let (_, m) = reg
            .resolve(&format!("openai/{id}"))
            .unwrap_or_else(|| panic!("missing openai/{id}"));
        assert!(m.supports_thinking, "{id} should advertise supports_thinking");
        assert!(m.supports_vision, "{id} should advertise vision");
    }
}

#[test]
fn anthropic_reasoning_models_carry_thinking_flag() {
    let reg = pi_ai::ModelRegistry::new(pi_ai::AuthStorage::in_memory());
    for alias in &["opus", "sonnet", "haiku"] {
        let (_, m) = reg
            .resolve(alias)
            .unwrap_or_else(|| panic!("missing alias {alias}"));
        assert!(
            m.supports_thinking,
            "anthropic {alias} should advertise supports_thinking"
        );
    }
}

#[test]
fn google_pro_reasoning_carries_thinking_flag() {
    let reg = pi_ai::ModelRegistry::new(pi_ai::AuthStorage::in_memory());
    let (_, m) = reg.resolve("gemini-pro").expect("gemini-pro");
    assert!(m.supports_thinking, "gemini-pro should advertise thinking");
}

#[test]
fn bedrock_anthropic_reasoning_models_carry_thinking_flag() {
    let reg = pi_ai::ModelRegistry::new(pi_ai::AuthStorage::in_memory());
    for alias in &["bedrock-opus", "bedrock-sonnet", "bedrock-haiku"] {
        let (_, m) = reg
            .resolve(alias)
            .unwrap_or_else(|| panic!("missing alias {alias}"));
        assert!(
            m.supports_thinking,
            "bedrock {alias} should advertise supports_thinking"
        );
    }
}

#[test]
fn gpt_4o_remains_non_thinking() {
    // Sanity check that we didn't accidentally flip the flag on
    // non-reasoning models.
    let reg = pi_ai::ModelRegistry::new(pi_ai::AuthStorage::in_memory());
    let (_, m) = reg.resolve("openai/gpt-4o").expect("gpt-4o");
    assert!(!m.supports_thinking);
}
