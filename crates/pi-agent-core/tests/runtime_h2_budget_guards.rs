//! Hardening §4.5 #2 + #3 (RFD 0027 Commit H2): per-session token
//! budget + per-turn tool-invocation cap + `Finish::ToolUse` requires
//! ≥1 tool call.

use async_trait::async_trait;
use pi_agent_core::{
    create_agent_session, ProviderFactory, RuntimeConfig, RuntimeError, SessionManager, Settings,
};
use pi_ai::provider::EventStream;
use pi_ai::{
    AuthMethod, AuthStorage, FinishReason, GenerateRequest, ModelInfo, ModelRegistry, Provider,
    ProviderConfig, ProviderKind, Result as AiResult, StreamEvent, StreamEventKind, Usage,
};
use pi_tools::ToolRegistry;
use serde_json::json;
use std::sync::Arc;
use std::sync::Mutex as StdMutex;

#[derive(Clone)]
struct MockProvider {
    cfg: ProviderConfig,
    canned: Arc<StdMutex<Vec<Vec<StreamEvent>>>>,
}

impl MockProvider {
    fn new(turns: Vec<Vec<StreamEvent>>) -> Self {
        Self {
            cfg: ProviderConfig {
                name: "anthropic".into(),
                kind: ProviderKind::Anthropic,
                base_url: "mock".into(),
                auth_header: "x-api-key".into(),
                auth_format: "{token}".into(),
                models: vec![],
            },
            canned: Arc::new(StdMutex::new(turns)),
        }
    }
}

#[async_trait]
impl Provider for MockProvider {
    fn config(&self) -> &ProviderConfig {
        &self.cfg
    }
    fn auth(&self) -> &AuthMethod {
        static N: AuthMethod = AuthMethod::None;
        &N
    }
    async fn stream(&self, _req: GenerateRequest, _model: &ModelInfo) -> AiResult<EventStream> {
        let turn = self
            .canned
            .lock()
            .unwrap()
            .drain(..1)
            .next()
            .unwrap_or_default();
        Ok(Box::pin(futures::stream::iter(turn.into_iter().map(Ok))))
    }
}

struct MockFactory(MockProvider);
impl ProviderFactory for MockFactory {
    fn build(
        &self,
        _cfg: ProviderConfig,
        _auth: AuthMethod,
    ) -> Result<Box<dyn Provider>, RuntimeError> {
        Ok(Box::new(self.0.clone()))
    }
}

fn ev(k: StreamEventKind) -> StreamEvent {
    StreamEvent::new(k)
}

fn build_cfg(provider: MockProvider) -> RuntimeConfig {
    let auth = AuthStorage::in_memory();
    auth.set("anthropic", AuthMethod::ApiKey { value: "k".into() });
    let mut settings = Settings::default();
    settings.provider = "anthropic".into();
    settings.model = "sonnet".into();
    RuntimeConfig::builder()
        .session_manager(SessionManager::in_memory())
        .auth_storage(auth.clone())
        .model_registry(ModelRegistry::new(auth))
        .tools(ToolRegistry::new())
        .settings(settings)
        .system_prompt("you are pi")
        .cwd(std::env::current_dir().unwrap())
        .with_provider_factory(Arc::new(MockFactory(provider)))
        .with_max_session_tokens(100)
        .with_max_tool_invocations_per_turn(3)
        .build_unwrap()
}

#[tokio::test]
async fn budget_exhausted_when_session_token_total_exceeds_cap() {
    // First turn: one stream with a Usage event reporting 200 tokens.
    // The cap is 100 tokens (set above), so this single turn busts it.
    let turns = vec![vec![
        ev(StreamEventKind::TextDelta { text: "hi".into() }),
        ev(StreamEventKind::Usage {
            usage: Usage {
                input_tokens: 100,
                output_tokens: 100,
                cache_read_tokens: 0,
                cache_write_tokens: 0,
                reasoning_tokens: 0,
                cost_usd: 0.0,
            },
        }),
        ev(StreamEventKind::Finish {
            reason: FinishReason::Stop,
        }),
    ]];

    let cfg = build_cfg(MockProvider::new(turns));
    let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
    let (_runtime, session) = create_agent_session(cfg, Some(tx)).expect("session");
    let res = session.prompt("trigger budget".into()).await;
    match res {
        Err(RuntimeError::BudgetExhausted { used, cap }) => {
            assert_eq!(cap, 100);
            assert!(used > cap, "used={used} should exceed cap={cap}");
        }
        other => panic!("expected BudgetExhausted, got {other:?}"),
    }
}

#[tokio::test]
async fn invocation_cap_exceeded_when_too_many_tool_calls_in_one_turn() {
    // First turn: 4 tool calls back-to-back; cap is 3.
    let mut events = Vec::new();
    for i in 0..4 {
        events.push(ev(StreamEventKind::ToolCallComplete {
            id: format!("tu_{i}"),
            name: "noop".into(),
            input: json!({"i": i}),
        }));
    }
    events.push(ev(StreamEventKind::Finish {
        reason: FinishReason::ToolUse,
    }));
    let turns = vec![events];

    let cfg = build_cfg(MockProvider::new(turns));
    let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
    let (_runtime, session) = create_agent_session(cfg, Some(tx)).expect("session");
    let res = session.prompt("emit 4 tools".into()).await;
    match res {
        Err(RuntimeError::InvocationCapExceeded { invoked, cap }) => {
            assert_eq!(cap, 3);
            assert!(invoked > cap, "invoked={invoked} > cap={cap}");
        }
        other => panic!("expected InvocationCapExceeded, got {other:?}"),
    }
}

#[tokio::test]
async fn finish_tool_use_with_no_calls_is_rejected() {
    // Stream: TextDelta + Finish::ToolUse (no tool calls). Malformed
    // per RFD §4.5 #2; runtime should return ToolUseFinishWithoutCalls.
    let turns = vec![vec![
        ev(StreamEventKind::TextDelta { text: "I'll use a tool!".into() }),
        ev(StreamEventKind::Finish {
            reason: FinishReason::ToolUse,
        }),
    ]];
    let cfg = build_cfg(MockProvider::new(turns));
    let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
    let (_runtime, session) = create_agent_session(cfg, Some(tx)).expect("session");
    let res = session.prompt("trigger tool-use-without-calls".into()).await;
    assert!(
        matches!(res, Err(RuntimeError::ToolUseFinishWithoutCalls)),
        "expected ToolUseFinishWithoutCalls, got {res:?}"
    );
}

#[tokio::test]
async fn cumulative_usage_events_in_one_turn_are_not_double_counted() {
    // Per code-review pass-4 finding #1: Google emits Usage on a
    // standalone usageMetadata chunk AND again on the terminal
    // candidate chunk; both carry CUMULATIVE values. Pre-fix the
    // runtime added them together, so a turn that legitimately
    // consumed 50 tokens looked like 100 against the cap.
    //
    // This test sends two Usage events in one turn (50 input + 50
    // output cumulative on each) and asserts the session-token
    // accumulator only sees 100 total, NOT 200. Cap is 150 so the
    // pre-fix behaviour would trip BudgetExhausted; post-fix it
    // succeeds.
    let cumulative_usage = Usage {
        input_tokens: 50,
        output_tokens: 50,
        cache_read_tokens: 0,
        cache_write_tokens: 0,
        reasoning_tokens: 0,
        cost_usd: 0.0,
    };
    let turns = vec![vec![
        ev(StreamEventKind::TextDelta { text: "hi".into() }),
        // First Usage event (e.g. usageMetadata mid-stream chunk).
        ev(StreamEventKind::Usage { usage: cumulative_usage.clone() }),
        // Second Usage event (e.g. message_delta terminal chunk),
        // with the same cumulative numbers.
        ev(StreamEventKind::Usage { usage: cumulative_usage.clone() }),
        ev(StreamEventKind::Finish { reason: FinishReason::Stop }),
    ]];

    // Cap of 150 — pre-fix would trip on 50+50+50+50 = 200; post-fix
    // sees 100 cumulative and stays well under.
    let auth = AuthStorage::in_memory();
    auth.set("anthropic", AuthMethod::ApiKey { value: "k".into() });
    let mut settings = Settings::default();
    settings.provider = "anthropic".into();
    settings.model = "sonnet".into();
    let cfg = RuntimeConfig::builder()
        .session_manager(SessionManager::in_memory())
        .auth_storage(auth.clone())
        .model_registry(ModelRegistry::new(auth))
        .tools(ToolRegistry::new())
        .settings(settings)
        .system_prompt("you are pi")
        .cwd(std::env::current_dir().unwrap())
        .with_provider_factory(Arc::new(MockFactory(MockProvider::new(turns))))
        .with_max_session_tokens(150)
        .with_max_tool_invocations_per_turn(64)
        .build_unwrap();

    let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
    let (_runtime, session) = create_agent_session(cfg, Some(tx)).expect("session");
    let res = session.prompt("cumulative usage scenario".into()).await;
    assert!(
        res.is_ok(),
        "cumulative-Usage providers must not double-count; got {res:?}"
    );
}

#[tokio::test]
async fn max_session_tokens_zero_means_disabled_not_starve() {
    // Per code-review pass-4 finding #2: cap=0 must NOT immediately
    // starve every session on the first non-zero Usage event.
    // Pre-fix `total > 0` would trip BudgetExhausted on any spend.
    let cfg = {
        let auth = AuthStorage::in_memory();
        auth.set("anthropic", AuthMethod::ApiKey { value: "k".into() });
        let mut settings = Settings::default();
        settings.provider = "anthropic".into();
        settings.model = "sonnet".into();
        RuntimeConfig::builder()
            .session_manager(SessionManager::in_memory())
            .auth_storage(auth.clone())
            .model_registry(ModelRegistry::new(auth))
            .tools(ToolRegistry::new())
            .settings(settings)
            .system_prompt("you are pi")
            .cwd(std::env::current_dir().unwrap())
            .with_provider_factory(Arc::new(MockFactory(MockProvider::new(vec![vec![
                ev(StreamEventKind::TextDelta { text: "hi".into() }),
                ev(StreamEventKind::Usage {
                    usage: Usage {
                        input_tokens: 10_000,
                        output_tokens: 10_000,
                        cache_read_tokens: 0,
                        cache_write_tokens: 0,
                        reasoning_tokens: 0,
                        cost_usd: 0.0,
                    },
                }),
                ev(StreamEventKind::Finish { reason: FinishReason::Stop }),
            ]]))))
            .with_max_session_tokens(0) // disabled
            .build_unwrap()
    };

    let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
    let (_runtime, session) = create_agent_session(cfg, Some(tx)).expect("session");
    let res = session.prompt("test".into()).await;
    assert!(res.is_ok(), "max=0 should be disabled, not starve; got {res:?}");
}

#[tokio::test]
async fn saturating_add_does_not_panic_on_max_token_usage() {
    // Adversarial provider sends a Usage event with u64::MAX token
    // counts. saturating_add prevents the runtime from overflowing
    // (which pre-H2 would have wrapped to a small number); instead
    // we promptly hit BudgetExhausted with `used = u64::MAX`.
    let turns = vec![vec![
        ev(StreamEventKind::Usage {
            usage: Usage {
                input_tokens: u64::MAX,
                output_tokens: 1,
                cache_read_tokens: 0,
                cache_write_tokens: 0,
                reasoning_tokens: 0,
                cost_usd: 0.0,
            },
        }),
        ev(StreamEventKind::Finish {
            reason: FinishReason::Stop,
        }),
    ]];
    let cfg = build_cfg(MockProvider::new(turns));
    let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
    let (_runtime, session) = create_agent_session(cfg, Some(tx)).expect("session");
    let res = session.prompt("adversarial usage".into()).await;
    assert!(
        matches!(res, Err(RuntimeError::BudgetExhausted { .. })),
        "expected BudgetExhausted on u64::MAX usage, got {res:?}"
    );
}

/// Regression: every terminal-Err path must emit `TurnComplete` *before*
/// returning, so generated channel pumps (`crates/pi-build/src/codegen.rs`)
/// that `break` only on `TurnComplete` don't hang forever.
///
/// This test spins up a session with a 1-token cap (guaranteed to trip on
/// turn 1 with a provider that reports ≥1 output token), subscribes to
/// the event channel, calls `prompt`, and asserts that:
///   1. `TurnComplete` was received at least once, AND
///   2. the `prompt` future resolved to `Err(BudgetExhausted)`.
#[tokio::test]
async fn turn_complete_emitted_before_budget_exhausted_err() {
    let turns = vec![vec![
        ev(StreamEventKind::TextDelta { text: "hi".into() }),
        ev(StreamEventKind::Usage {
            usage: Usage {
                input_tokens: 1,
                output_tokens: 1,
                cache_read_tokens: 0,
                cache_write_tokens: 0,
                reasoning_tokens: 0,
                cost_usd: 0.0,
            },
        }),
        ev(StreamEventKind::Finish {
            reason: FinishReason::Stop,
        }),
    ]];

    // Cap of 1 token: total=2 after the turn, so BudgetExhausted fires.
    let auth = AuthStorage::in_memory();
    auth.set("anthropic", AuthMethod::ApiKey { value: "k".into() });
    let mut settings = Settings::default();
    settings.provider = "anthropic".into();
    settings.model = "sonnet".into();
    let cfg = RuntimeConfig::builder()
        .session_manager(SessionManager::in_memory())
        .auth_storage(auth.clone())
        .model_registry(ModelRegistry::new(auth))
        .tools(ToolRegistry::new())
        .settings(settings)
        .system_prompt("you are pi")
        .cwd(std::env::current_dir().unwrap())
        .with_provider_factory(Arc::new(MockFactory(MockProvider::new(turns))))
        .with_max_session_tokens(1)
        .with_max_tool_invocations_per_turn(64)
        .build_unwrap();

    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    let (_runtime, session) = create_agent_session(cfg, Some(tx)).expect("session");

    // Run prompt and collect all events concurrently.
    let prompt_fut = tokio::spawn(async move { session.prompt("budget test".into()).await });

    // Drain events until TurnComplete arrives (or channel closes).
    let mut saw_turn_complete = false;
    while let Some(event) = rx.recv().await {
        if matches!(event.kind, pi_agent_core::AgentEventKind::TurnComplete) {
            saw_turn_complete = true;
            break;
        }
    }

    // Now await the prompt result — it must have resolved (or be resolving
    // very shortly after TurnComplete was sent).
    let result = prompt_fut.await.expect("prompt task panicked");

    assert!(
        saw_turn_complete,
        "TurnComplete was never emitted before Err(BudgetExhausted)"
    );
    assert!(
        matches!(result, Err(RuntimeError::BudgetExhausted { .. })),
        "expected BudgetExhausted, got {result:?}"
    );
}
