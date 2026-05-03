use pi_ai::auth::{AuthMethod, AuthStorage};
use pi_ai::registry::ModelRegistry;

// ── helpers ──────────────────────────────────────────────────────────────────

fn provider_names(reg: &ModelRegistry) -> Vec<&str> {
    reg.providers().map(|c| c.name.as_str()).collect()
}

// ── provider presence ────────────────────────────────────────────────────────

#[test]
fn all_new_providers_are_registered() {
    let reg = ModelRegistry::new(AuthStorage::in_memory());
    let names = provider_names(&reg);

    for expected in &[
        "cerebras",
        "groq",
        "xai",
        "openrouter",
        "deepseek",
        "mistral",
        "zai",
        "huggingface",
        "ollama",
        "kimi",
        "minimax",
    ] {
        assert!(
            names.contains(expected),
            "provider '{}' missing from registry; found: {:?}",
            expected,
            names
        );
    }
}

// ── alias resolution ─────────────────────────────────────────────────────────

#[test]
fn resolve_deepseek_chat() {
    let reg = ModelRegistry::new(AuthStorage::in_memory());
    let (provider, model) = reg
        .resolve("deepseek-chat")
        .expect("deepseek-chat should resolve");
    assert_eq!(provider.name, "deepseek");
    assert_eq!(model.id, "deepseek-chat");
}

#[test]
fn resolve_grok_2_latest() {
    let reg = ModelRegistry::new(AuthStorage::in_memory());
    let (provider, model) = reg
        .resolve("grok-2-latest")
        .expect("grok-2-latest should resolve");
    assert_eq!(provider.name, "xai");
    assert_eq!(model.id, "grok-2-latest");
}

// ── env-key loading ───────────────────────────────────────────────────────────

#[test]
fn xai_api_key_from_env() {
    // Isolate: set only XAI_API_KEY and make sure unrelated keys are absent.
    std::env::set_var("XAI_API_KEY", "foo");

    let storage = AuthStorage::from_env_explicit(AuthStorage::ENV_KEYS.iter().copied()).unwrap();
    match storage.get("xai") {
        Some(AuthMethod::ApiKey { value }) => assert_eq!(value, "foo"),
        other => panic!("expected ApiKey {{ value: \"foo\" }}, got {:?}", other),
    }

    std::env::remove_var("XAI_API_KEY");
}

// ── ENV_KEYS completeness ─────────────────────────────────────────────────────

#[test]
fn env_keys_contains_all_new_providers() {
    let keys: Vec<&str> = AuthStorage::ENV_KEYS.iter().map(|(p, _)| *p).collect();

    for expected in &[
        "cerebras",
        "groq",
        "xai",
        "openrouter",
        "deepseek",
        "mistral",
        "zai",
        "huggingface",
        "ollama",
        "kimi",
        "minimax",
    ] {
        assert!(
            keys.contains(expected),
            "ENV_KEYS missing entry for '{}'; found: {:?}",
            expected,
            keys
        );
    }
}
