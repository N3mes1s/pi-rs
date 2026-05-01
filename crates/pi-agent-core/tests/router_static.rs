use pi_agent_core::router::{ForceOverride, Router, RoutingContext, RoutingDecision, StaticRouter};
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
    let router = StaticRouter::new(RoutingDecision {
        route_id: "static".to_string(),
        provider: "anthropic".to_string(),
        model: "claude-sonnet-4-6".to_string(),
        thinking: ThinkingLevel::Medium,
    });
    let decision = router
        .route("rename foo", &[], &[], &context(&registry, None))
        .unwrap();
    assert_eq!(decision.provider, "anthropic");
    assert_eq!(decision.model, "claude-sonnet-4-6");
    assert_eq!(decision.thinking, ThinkingLevel::Medium);
    assert_eq!(decision.route_id, "static");
}

// NOTE: tests `router_no_config_falls_back`, `router_resolve_failure`,
// and `router_force_override` were deleted as part of the RFD 0020
// StaticRouter simplification. They tested the old `from_paths` API
// which loaded `router.toml` from user/repo paths and resolved models
// against the registry. The new `StaticRouter::new(decision)` is a
// minimal wrapper that just emits a fixed decision; file-based routing
// is provided by other Router impls (e.g. `EmbeddingRouter`).
//
// The force-override behavior tested by `router_force_override` is now
// covered by the unit tests in `crates/pi-agent-core/src/router/mod.rs`
// (force.rs / mod.rs).
