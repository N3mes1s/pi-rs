use pi_agent_core::router::{ForceOverride, Router, RoutingContext, StaticRouter};
use pi_ai::{AuthStorage, ModelRegistry, ThinkingLevel};

fn context<'a>(registry: &'a ModelRegistry, force: Option<ForceOverride>) -> RoutingContext<'a> {
    RoutingContext {
        registry,
        user_lambda: 1.0,
        force,
        session_id: "test-session",
        cache_read_tokens: 0,
        cache_write_tokens: 0,
    }
}

#[test]
fn static_router_compat() {
    let registry = ModelRegistry::new(AuthStorage::in_memory());
    let router = StaticRouter::new("anthropic", "claude-sonnet-4-6", ThinkingLevel::Medium);
    let decision = router
        .route("rename foo", &[], &[], &context(&registry, None))
        .unwrap();
    assert_eq!(decision.provider, "anthropic");
    assert_eq!(decision.model, "claude-sonnet-4-6");
    assert_eq!(decision.thinking, ThinkingLevel::Medium);
    assert_eq!(decision.route_id, "static");
}

#[test]
fn router_no_config_falls_back() {
    let registry = ModelRegistry::new(AuthStorage::in_memory());
    let temp = tempfile::tempdir().unwrap();
    let user = temp.path().join("user-router.toml");
    let repo = temp.path().join("repo-router.toml");
    let router =
        StaticRouter::from_paths("openai", "gpt-4o", ThinkingLevel::Low, &user, &repo).unwrap();
    let decision = router
        .route("prompt", &[], &[], &context(&registry, None))
        .unwrap();
    assert_eq!(decision.provider, "openai");
    assert_eq!(decision.model, "gpt-4o");
    assert_eq!(decision.thinking, ThinkingLevel::Low);
}

#[test]
fn router_resolve_failure() {
    let registry = ModelRegistry::new(AuthStorage::in_memory());
    let temp = tempfile::tempdir().unwrap();
    let repo = temp.path().join("router.toml");
    std::fs::write(
        &repo,
        "[[route]]\nid = \"fast\"\nprovider = \"ollama\"\nmodel = \"missing\"\nthinking = \"off\"\n",
    )
    .unwrap();
    let router = StaticRouter::from_paths(
        "anthropic",
        "claude-sonnet-4-6",
        ThinkingLevel::Medium,
        temp.path().join("user.toml"),
        &repo,
    )
    .unwrap();
    let err = router
        .route("prompt", &[], &[], &context(&registry, None))
        .unwrap_err();
    assert!(err.to_string().contains("unknown model: ollama/missing"));
}

#[test]
fn router_force_override() {
    let registry = ModelRegistry::new(AuthStorage::in_memory());
    let temp = tempfile::tempdir().unwrap();
    let repo = temp.path().join("router.toml");
    std::fs::write(
        &repo,
        "[[route]]\nid = \"fast\"\nprovider = \"anthropic\"\nmodel = \"claude-haiku-4-5-20251001\"\nthinking = \"off\"\n",
    )
    .unwrap();
    let router = StaticRouter::from_paths(
        "anthropic",
        "claude-sonnet-4-6",
        ThinkingLevel::Medium,
        temp.path().join("user.toml"),
        &repo,
    )
    .unwrap();
    let decision = router
        .route(
            "prompt",
            &[],
            &[],
            &context(
                &registry,
                Some(ForceOverride {
                    provider: "openai".to_string(),
                    model: "gpt-5.4".to_string(),
                    thinking: ThinkingLevel::XHigh,
                }),
            ),
        )
        .unwrap();
    assert_eq!(decision.provider, "openai");
    assert_eq!(decision.model, "gpt-5.4");
    assert_eq!(decision.thinking, ThinkingLevel::XHigh);
    assert_eq!(decision.route_id, "forced");
}
