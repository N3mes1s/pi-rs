use pi_agent_core::{EmbeddingRouter, RouteMode, Router, RoutingContext};
use pi_ai::{AuthStorage, ModelRegistry};

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

#[test]
fn embedding_router_routes() {
    let auth = AuthStorage::in_memory();
    let registry = ModelRegistry::new(auth);
    let router = match EmbeddingRouter::bundled() {
        Ok(router) => router,
        Err(_) => return,
    };
    let cases = vec![
        ("rename foo to bar in src/lib.rs", "fast"),
        ("rename foo to bar in src/lib.rs (just describe the diff)", "fast"),
        ("add a doc comment to this function", "fast"),
        ("remove the println at line 42", "fast"),
        ("fix this typo in README", "fast"),
        ("describe the diff for renaming a variable", "fast"),
        ("delete an unused import", "fast"),
        ("change this constant name everywhere", "fast"),
        ("add docs for this trait", "fast"),
        ("small mechanical edit only", "fast"),
        ("extract this trait into its own crate", "default"),
        ("audit OpenAI's Responses API and write an RFD", "default"),
        ("run the test suite and fix what fails", "default"),
        ("debug why this integration test flakes", "default"),
        ("refactor this module into smaller pieces", "default"),
        ("review this design and suggest improvements", "default"),
        ("investigate failing CI and propose a fix", "default"),
        ("trace through this bug and explain root cause", "default"),
        ("update the crate layout and explain tradeoffs", "default"),
        ("write an implementation plan for this feature", "default"),
        ("prove that this loop terminates", "hard"),
        ("find a counterexample to this invariant", "hard"),
        ("is the borrow checker sound for this pattern?", "hard"),
        ("give a formal argument that this recursion terminates", "hard"),
        ("reason about aliasing safety in this unsafe block", "hard"),
        ("prove memory safety for this ownership pattern", "hard"),
        ("find a soundness hole in this type system rule", "hard"),
        ("show whether the invariant can be violated", "hard"),
        ("formalize why this proof obligation holds", "hard"),
        ("counterexample for borrow checker assumption", "hard"),
    ];
    for (prompt, expected) in cases {
        let decision = router.route(prompt, &[], &[], &ctx(&registry)).expect(prompt);
        assert_eq!(decision.route_id, expected, "prompt: {prompt}");
    }
    assert_eq!(RouteMode::parse("auto"), Some(RouteMode::Auto));
}
