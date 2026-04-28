use pi_agent_core::{
    default_embedding_model_path, EmbeddingEngine, EmbeddingRouter, RouteMode, Router,
    RoutingContext, ToolSpec,
};
use pi_agent_core::router::RouteEntry;
use pi_ai::{AuthStorage, Message, ModelRegistry, ThinkingLevel};
use std::sync::Arc;

struct FakeEngine;

impl EmbeddingEngine for FakeEngine {
    fn embed(&self, text: &str) -> Result<Vec<f32>, pi_agent_core::RouterError> {
        let lower = text.to_ascii_lowercase();
        let v = if lower.contains("rename") || lower.contains("diff") {
            vec![1.0, 0.0, 0.0]
        } else if lower.contains("prove") || lower.contains("invariant") || lower.contains("sound") {
            vec![0.0, 0.0, 1.0]
        } else {
            vec![0.0, 1.0, 0.0]
        };
        Ok(v)
    }
}

fn registry() -> ModelRegistry {
    ModelRegistry::new(AuthStorage::in_memory())
}

fn ctx<'a>(registry: &'a ModelRegistry) -> RoutingContext<'a> {
    RoutingContext {
        registry,
        user_lambda: 1.0,
        force: None,
        session_id: "test",
        cache_read_tokens: 0,
        cache_write_tokens: 0,
    }
}

fn routes() -> Vec<RouteEntry> {
    vec![
        RouteEntry {
            id: "fast".into(),
            examples: vec!["rename foo to bar".into()],
            threshold: 0.0,
            provider: "anthropic".into(),
            model: "claude-haiku-4-5-20251001".into(),
            thinking: "off".into(),
        },
        RouteEntry {
            id: "default".into(),
            examples: vec!["run the test suite and fix what fails".into()],
            threshold: 0.0,
            provider: "anthropic".into(),
            model: "claude-sonnet-4-6".into(),
            thinking: "medium".into(),
        },
        RouteEntry {
            id: "hard".into(),
            examples: vec!["prove that this loop terminates".into()],
            threshold: 0.0,
            provider: "openai".into(),
            model: "gpt-5.4".into(),
            thinking: "xhigh".into(),
        },
    ]
}

#[test]
fn route_mode_parses_auto() {
    assert_eq!(RouteMode::parse("auto"), Some(RouteMode::Auto));
}

#[test]
fn embedding_router_uses_actual_prompt_semantics() {
    let registry = registry();
    let router = EmbeddingRouter::with_engine(routes(), Arc::new(FakeEngine));
    let tools = vec![ToolSpec {
        name: "bash".into(),
    }];

    let fast = router
        .route("rename foo to bar in src/lib.rs", &[], &tools, &ctx(&registry))
        .unwrap();
    assert_eq!(fast.route_id, "fast");
    assert_eq!(fast.model, "claude-haiku-4-5-20251001");

    let hard = router
        .route(
            "prove the invariant holds for this unsafe pointer pattern",
            &[],
            &tools,
            &ctx(&registry),
        )
        .unwrap();
    assert_eq!(hard.route_id, "hard");
    assert_eq!(hard.model, "gpt-5.4");
    assert_eq!(hard.thinking, ThinkingLevel::XHigh);
}

#[test]
fn embedding_router_consults_history_and_tools() {
    let registry = registry();
    let router = EmbeddingRouter::with_engine(routes(), Arc::new(FakeEngine));
    let history = vec![Message::user_text(
        "please prove this invariant over the borrow checker state".to_string(),
    )];
    let decision = router
        .route(
            "continue",
            &history,
            &[ToolSpec {
                name: "grep".into(),
            }],
            &ctx(&registry),
        )
        .unwrap();
    assert_eq!(decision.route_id, "hard");
}

#[test]
fn downloaded_onnx_path_is_loadable() {
    let path = default_embedding_model_path();
    assert!(path.exists(), "embedding model should be fetched before smoke tests");
}
