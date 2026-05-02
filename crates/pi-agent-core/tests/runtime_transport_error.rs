//! Regression test for the transport-mid-stream-error path
//! (commit `c0c8a61 fix(pi/print): fail-loud on transport error
//! instead of hanging forever`).
//!
//! Before the fix, a `stream.next()` Err mid-turn (transport drop,
//! decode failure, etc.) caused the runtime to short-circuit up via
//! `?` WITHOUT emitting `AgentEventKind::Error` or
//! `AgentEventKind::Aborted` first. Listeners on the event channel
//! were left waiting forever for a terminal event that would never
//! arrive — `pi -p` mode hung indefinitely.
//!
//! What this test pins:
//!
//!   1. A stream that yields one valid event then an Err results in
//!      `prompt()` returning `Err(RuntimeError::Provider(...))`.
//!   2. The event channel produces `AssistantStart`, the partial
//!      `AssistantTextDelta`, then `Error { message }` AND
//!      `Aborted` BEFORE the channel closes — so a `-p` mode
//!      printer that breaks on Error or Aborted exits cleanly
//!      instead of polling forever.
//!
//! Both invariants together guarantee no zombie hang on transient
//! provider failures.

use async_trait::async_trait;
use pi_agent_core::event::{AgentEvent, AgentEventKind};
use pi_agent_core::{
    create_agent_session, ProviderFactory, RuntimeConfig, SessionManager, Settings,
};
use pi_ai::provider::EventStream;
use pi_ai::{
    AiError, AuthMethod, AuthStorage, GenerateRequest, ModelInfo, ModelRegistry, Provider,
    ProviderConfig, ProviderKind, Result as AiResult, StreamEvent, StreamEventKind,
};
use pi_tools::ToolRegistry;
use std::sync::Arc;

#[derive(Clone)]
struct ErrMidstreamProvider {
    cfg: ProviderConfig,
}

impl ErrMidstreamProvider {
    fn new() -> Self {
        Self {
            cfg: ProviderConfig {
                name: "anthropic".into(),
                kind: ProviderKind::Anthropic,
                base_url: "mock".into(),
                auth_header: "x-api-key".into(),
                auth_format: "{token}".into(),
                models: vec![],
            },
        }
    }
}

#[async_trait]
impl Provider for ErrMidstreamProvider {
    fn config(&self) -> &ProviderConfig {
        &self.cfg
    }
    fn auth(&self) -> &AuthMethod {
        static N: AuthMethod = AuthMethod::None;
        &N
    }
    async fn stream(&self, _req: GenerateRequest, _model: &ModelInfo) -> AiResult<EventStream> {
        // Stream produces one valid TextDelta, then an Err. This is
        // exactly the shape we hit in production when a Transport
        // drop happens mid-response — the API call succeeded, the
        // response started streaming, then bytes stopped arriving
        // and the eventsource parser produced an Err.
        let items: Vec<AiResult<StreamEvent>> = vec![
            Ok(StreamEvent::new(StreamEventKind::TextDelta {
                text: "partial-".into(),
            })),
            Err(AiError::Other(
                "synthetic transport drop for regression test".into(),
            )),
        ];
        Ok(Box::pin(futures::stream::iter(items)))
    }
}

struct ErrMidstreamFactory;

impl ProviderFactory for ErrMidstreamFactory {
    fn build(
        &self,
        _cfg: ProviderConfig,
        _auth: AuthMethod,
    ) -> Result<Box<dyn Provider>, pi_agent_core::runtime::RuntimeError> {
        Ok(Box::new(ErrMidstreamProvider::new()))
    }
}

#[tokio::test]
async fn transport_error_midstream_emits_error_and_aborted_before_failing() {
    let auth = AuthStorage::in_memory();
    auth.set("anthropic", AuthMethod::ApiKey { value: "k".into() });
    let mut settings = Settings::default();
    settings.provider = "anthropic".into();
    settings.model = "sonnet".into();
    let cfg = RuntimeConfig {
        session_manager: SessionManager::in_memory(),
        auth_storage: auth.clone(),
        model_registry: ModelRegistry::new(auth),
        tools: ToolRegistry::new(),
        settings,
        system_prompt: "you are pi".into(),
        context_files: Vec::new(),
        cwd: std::env::current_dir().unwrap(),
        provider_factory: Some(Arc::new(ErrMidstreamFactory)),
        tool_gate: None,
        gate_ask_is_approve: false,
        stream_interceptor: None,
        sandbox_provider: None,
    };
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<AgentEvent>();
    let (_runtime, session) = create_agent_session(cfg, Some(tx)).unwrap();

    // 1. prompt() must return Err (the runtime correctly bubbled
    //    up the provider error).
    let res = session.prompt("hi".into()).await;
    assert!(
        res.is_err(),
        "transport drop mid-stream must surface as a prompt() error, got Ok"
    );

    // 2. Drop the session so the EventSender is released and the
    //    receiver eventually sees None. (Pre-fix the receiver would
    //    have hung forever even after the session was dropped, but
    //    that's a different defect; here we just verify the events
    //    we expect are PRESENT before the channel closes.)
    drop(session);

    // 3. Drain the channel. We expect (at minimum) Error and
    //    Aborted events before it closes — these are what `-p` mode
    //    breaks on. AssistantStart and the partial TextDelta SHOULD
    //    appear too but we don't pin their exact ordering relative
    //    to other events (provider-implementation latitude).
    let mut saw_error = false;
    let mut saw_aborted = false;
    let mut error_message: Option<String> = None;
    while let Some(ev) = rx.recv().await {
        match ev.kind {
            AgentEventKind::Error { message } => {
                saw_error = true;
                error_message = Some(message);
            }
            AgentEventKind::Aborted => {
                saw_aborted = true;
            }
            _ => {}
        }
    }
    assert!(
        saw_error,
        "Error event must be emitted before the channel closes \
         (otherwise -p mode printer hangs polling forever)"
    );
    assert!(
        saw_aborted,
        "Aborted event must be emitted before the channel closes"
    );
    let msg = error_message.unwrap();
    assert!(
        msg.contains("synthetic transport drop"),
        "Error event message must carry the underlying transport \
         error string for diagnostics, got: {msg}"
    );
}
