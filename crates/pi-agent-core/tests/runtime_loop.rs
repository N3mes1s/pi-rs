//! End-to-end coverage of `runtime.rs` driven by an in-process mock
//! provider. Exercises the user/assistant turn pump, tool calls, the
//! LLM-driven compactor, and the abort path.

use async_trait::async_trait;
use futures::StreamExt;
use pi_agent_core::event::{AgentEvent, AgentEventKind};
use pi_agent_core::{
    create_agent_session, ProviderFactory, RuntimeConfig, SessionEntryKind, SessionManager,
    Settings,
};
use pi_ai::provider::EventStream;
use pi_ai::{
    AuthMethod, AuthStorage, FinishReason, GenerateRequest, ModelInfo, ModelRegistry, Provider,
    ProviderConfig, ProviderKind, Result as AiResult, StreamEvent, StreamEventKind,
};
use pi_tools::{Tool, ToolContext, ToolError, ToolRegistry};
use serde_json::json;
use std::sync::Arc;
use std::sync::Mutex as StdMutex;
use tokio::sync::mpsc::UnboundedReceiver;

// --- mock provider ---------------------------------------------------------

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
        // unused
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
        let s = futures::stream::iter(turn.into_iter().map(Ok));
        Ok(Box::pin(s))
    }
}

struct MockFactory {
    inner: MockProvider,
}

impl ProviderFactory for MockFactory {
    fn build(
        &self,
        _cfg: ProviderConfig,
        _auth: AuthMethod,
    ) -> Result<Box<dyn Provider>, pi_agent_core::runtime::RuntimeError> {
        Ok(Box::new(self.inner.clone()))
    }
}

// --- helpers ---------------------------------------------------------------

fn build_config(provider: MockProvider, tools: ToolRegistry) -> RuntimeConfig {
    let auth = AuthStorage::in_memory();
    auth.set("anthropic", AuthMethod::ApiKey { value: "k".into() });
    let mut settings = Settings::default();
    settings.provider = "anthropic".into();
    settings.model = "sonnet".into();
    RuntimeConfig {
        session_manager: SessionManager::in_memory(),
        auth_storage: auth.clone(),
        model_registry: ModelRegistry::new(auth),
        tools,
        settings,
        system_prompt: "you are pi".into(),
        context_files: Vec::new(),
        cwd: std::env::current_dir().unwrap(),
        provider_factory: Some(Arc::new(MockFactory { inner: provider })),
        tool_gate: None,
        gate_ask_is_approve: false,
        stream_interceptor: None,
    }
}

async fn drain(mut rx: UnboundedReceiver<AgentEvent>) -> Vec<AgentEventKind> {
    let mut out = Vec::new();
    while let Ok(ev) = tokio::time::timeout(std::time::Duration::from_millis(50), rx.recv()).await {
        match ev {
            Some(e) => out.push(e.kind),
            None => break,
        }
    }
    out
}

fn ev(k: StreamEventKind) -> StreamEvent {
    StreamEvent::new(k)
}

// --- 1. simple turn --------------------------------------------------------

#[tokio::test]
async fn runtime_simple_turn_emits_expected_events() {
    let canned = vec![vec![
        ev(StreamEventKind::TextDelta {
            text: "hi there".into(),
        }),
        ev(StreamEventKind::Finish {
            reason: FinishReason::Stop,
        }),
    ]];
    let provider = MockProvider::new(canned);
    let cfg = build_config(provider, ToolRegistry::new());
    let session_manager = cfg.session_manager.clone();

    let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
    let (_runtime, session) = create_agent_session(cfg, Some(tx)).expect("session");
    let session_id = session.id().to_string();
    session.prompt("hello".into()).await.expect("prompt ok");

    let kinds = drain(rx).await;
    let names: Vec<&'static str> = kinds
        .iter()
        .map(|k| match k {
            AgentEventKind::UserMessage { .. } => "user",
            AgentEventKind::AssistantStart => "start",
            AgentEventKind::AssistantTextDelta { .. } => "delta",
            AgentEventKind::AssistantMessage { .. } => "assistant",
            AgentEventKind::TurnComplete => "turn",
            _ => "other",
        })
        .collect();
    let pos = |needle: &str| names.iter().position(|n| *n == needle);
    assert!(pos("user").unwrap() < pos("start").unwrap());
    assert!(pos("start").unwrap() < pos("delta").unwrap());
    assert!(pos("delta").unwrap() < pos("assistant").unwrap());
    assert!(pos("assistant").unwrap() < pos("turn").unwrap());

    // Session storage saw Meta + User + Assistant.
    let branch = session_manager.current_branch(&session_id);
    assert!(branch
        .iter()
        .any(|e| matches!(e.kind, SessionEntryKind::Meta { .. })));
    assert!(branch
        .iter()
        .any(|e| matches!(e.kind, SessionEntryKind::User { .. })));
    assert!(branch
        .iter()
        .any(|e| matches!(e.kind, SessionEntryKind::Assistant { .. })));
}

// --- 2. tool call round-trip ----------------------------------------------

struct EchoTool;

#[async_trait]
impl Tool for EchoTool {
    fn spec(&self) -> pi_ai::ToolSpec {
        pi_ai::ToolSpec {
            name: "echo".into(),
            description: "echo".into(),
            input_schema: json!({"type":"object"}),
        }
    }
    fn read_only(&self) -> bool {
        true
    }
    async fn invoke(
        &self,
        _ctx: &ToolContext,
        call_id: &str,
        input: serde_json::Value,
    ) -> Result<pi_ai::ToolResult, ToolError> {
        Ok(pi_ai::ToolResult {
            tool_use_id: call_id.into(),
            model_output: format!("echo:{}", input),
            display: None,
            is_error: false,
        })
    }
}

#[tokio::test]
async fn runtime_tool_call_round_trip() {
    let turn1 = vec![
        ev(StreamEventKind::TextDelta {
            text: "calling".into(),
        }),
        ev(StreamEventKind::ToolCallComplete {
            id: "tu_1".into(),
            name: "echo".into(),
            input: json!({"x": 1}),
        }),
        ev(StreamEventKind::Finish {
            reason: FinishReason::ToolUse,
        }),
    ];
    let turn2 = vec![
        ev(StreamEventKind::TextDelta {
            text: "done".into(),
        }),
        ev(StreamEventKind::Finish {
            reason: FinishReason::Stop,
        }),
    ];
    let provider = MockProvider::new(vec![turn1, turn2]);
    let mut tools = ToolRegistry::new();
    tools.register(Arc::new(EchoTool));
    let cfg = build_config(provider, tools);

    let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
    let (_runtime, session) = create_agent_session(cfg, Some(tx)).expect("session");
    session.prompt("go".into()).await.expect("prompt ok");

    let kinds = drain(rx).await;
    let mut saw_tool_call = false;
    let mut saw_tool_result = false;
    let mut saw_turn_complete = false;
    for k in &kinds {
        match k {
            AgentEventKind::AssistantToolCall { call } => {
                assert_eq!(call.name, "echo");
                saw_tool_call = true;
            }
            AgentEventKind::ToolResult { result } => {
                assert!(result.model_output.starts_with("echo:"));
                saw_tool_result = true;
            }
            AgentEventKind::TurnComplete => saw_turn_complete = true,
            _ => {}
        }
    }
    assert!(saw_tool_call, "AssistantToolCall not emitted");
    assert!(saw_tool_result, "ToolResult not emitted");
    assert!(saw_turn_complete, "TurnComplete not emitted");
}

// --- 3. compact_with_llm prepends <context_recap> --------------------------

#[tokio::test]
async fn runtime_compact_with_llm_prepends_recap() {
    // First turn: a normal user prompt completes.
    let turn1 = vec![
        ev(StreamEventKind::TextDelta {
            text: "first".into(),
        }),
        ev(StreamEventKind::Finish {
            reason: FinishReason::Stop,
        }),
    ];
    // After the heuristic boundary we'll need many user messages for the
    // LlmCompactor to actually compact (keep_last_turns=6, so we need >6
    // user messages). To force compaction we'll inject extra user messages
    // directly into the session via prompt() then call compact_with_llm.
    // The compactor will issue another stream() call to summarise:
    let summary_turn = vec![
        ev(StreamEventKind::TextDelta {
            text: "RECAP-LINE".into(),
        }),
        ev(StreamEventKind::Finish {
            reason: FinishReason::Stop,
        }),
    ];
    let mut turns: Vec<Vec<StreamEvent>> = Vec::new();
    // 7 conversational turns + 1 compactor turn
    for i in 0..7 {
        let _ = i;
        turns.push(vec![
            ev(StreamEventKind::TextDelta { text: "ok".into() }),
            ev(StreamEventKind::Finish {
                reason: FinishReason::Stop,
            }),
        ]);
    }
    let _ = turn1; // keep variable for clarity
    turns.push(summary_turn);

    let provider = MockProvider::new(turns);
    let cfg = build_config(provider, ToolRegistry::new());
    let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
    let (_runtime, session) = create_agent_session(cfg, Some(tx)).expect("session");
    for i in 0..7 {
        session.prompt(format!("msg-{i}")).await.expect("prompt ok");
    }

    session.compact_with_llm(Some("be terse".into())).await;

    let msgs = session.messages().await;
    // First message after compaction should be the recap user message.
    let first = msgs.first().expect("at least one message");
    assert!(matches!(first.role, pi_ai::Role::User));
    let text = first.text();
    assert!(
        text.contains("<context_recap>"),
        "expected <context_recap> prefix, got: {text}"
    );
    assert!(text.contains("RECAP-LINE"));
}

// --- 4. abort during a turn -----------------------------------------------

/// A provider whose stream yields one delta, then waits for a signal,
/// allowing the test to call `abort()` mid-stream.
struct AbortableProvider {
    cfg: ProviderConfig,
    started: Arc<tokio::sync::Notify>,
    proceed: Arc<tokio::sync::Notify>,
}

#[async_trait]
impl Provider for AbortableProvider {
    fn config(&self) -> &ProviderConfig {
        &self.cfg
    }
    fn auth(&self) -> &AuthMethod {
        static N: AuthMethod = AuthMethod::None;
        &N
    }
    async fn stream(&self, _req: GenerateRequest, _model: &ModelInfo) -> AiResult<EventStream> {
        let started = self.started.clone();
        let proceed = self.proceed.clone();
        // State machine: emit partial, signal started, await proceed,
        // emit more + finish, then end.
        let s = futures::stream::unfold(
            (0u8, started, proceed),
            |(step, started, proceed)| async move {
                match step {
                    0 => {
                        let ev = StreamEvent::new(StreamEventKind::TextDelta {
                            text: "partial".into(),
                        });
                        started.notify_one();
                        Some((Ok(ev), (1, started, proceed)))
                    }
                    1 => {
                        proceed.notified().await;
                        let ev = StreamEvent::new(StreamEventKind::TextDelta {
                            text: "more".into(),
                        });
                        Some((Ok(ev), (2, started, proceed)))
                    }
                    2 => {
                        let ev = StreamEvent::new(StreamEventKind::Finish {
                            reason: FinishReason::Stop,
                        });
                        Some((Ok(ev), (3, started, proceed)))
                    }
                    _ => None,
                }
            },
        );
        Ok(Box::pin(s))
    }
}

struct AbortFactory {
    started: Arc<tokio::sync::Notify>,
    proceed: Arc<tokio::sync::Notify>,
}

impl ProviderFactory for AbortFactory {
    fn build(
        &self,
        cfg: ProviderConfig,
        _auth: AuthMethod,
    ) -> Result<Box<dyn Provider>, pi_agent_core::runtime::RuntimeError> {
        Ok(Box::new(AbortableProvider {
            cfg,
            started: self.started.clone(),
            proceed: self.proceed.clone(),
        }))
    }
}

#[tokio::test]
async fn runtime_abort_emits_aborted_event() {
    let started = Arc::new(tokio::sync::Notify::new());
    let proceed = Arc::new(tokio::sync::Notify::new());
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
        system_prompt: String::new(),
        context_files: Vec::new(),
        cwd: std::env::current_dir().unwrap(),
        provider_factory: Some(Arc::new(AbortFactory {
            started: started.clone(),
            proceed: proceed.clone(),
        })),
        tool_gate: None,
        gate_ask_is_approve: false,
        stream_interceptor: None,
    };

    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    let (_runtime, session) = create_agent_session(cfg, Some(tx)).expect("session");
    let s2 = session.clone();
    let h = tokio::spawn(async move { s2.prompt("hi".into()).await });

    started.notified().await;
    session.abort().await;
    proceed.notify_one();

    let res = h.await.expect("join");
    assert!(res.is_err(), "expected abort error");

    // Drain events; expect an Aborted somewhere.
    let mut saw_abort = false;
    while let Ok(Some(ev)) =
        tokio::time::timeout(std::time::Duration::from_millis(100), rx.recv()).await
    {
        if matches!(ev.kind, AgentEventKind::Aborted) {
            saw_abort = true;
        }
    }
    assert!(saw_abort, "expected Aborted event");

    // Suppress unused warnings for futures features unused here.
    let _ = futures::stream::empty::<()>();
    let _ = futures::stream::iter::<Vec<()>>(Vec::new()).next();
}

// --- 8. stream interceptor: TTSR-style abort + inject ---------------------

struct InjectOnceInterceptor {
    fired: tokio::sync::Mutex<bool>,
    trigger: String,
    reminder: String,
}

#[async_trait]
impl pi_agent_core::StreamInterceptor for InjectOnceInterceptor {
    async fn on_text_delta(&self, text: &str) -> pi_agent_core::InterceptAction {
        let mut fired = self.fired.lock().await;
        if *fired {
            return pi_agent_core::InterceptAction::Continue;
        }
        if text.contains(&self.trigger) {
            *fired = true;
            return pi_agent_core::InterceptAction::AbortAndInject(self.reminder.clone());
        }
        pi_agent_core::InterceptAction::Continue
    }
}

#[tokio::test]
async fn stream_interceptor_aborts_and_reinjects_then_completes() {
    // Turn 1: assistant says "let me PLAN this" — interceptor catches
    // "PLAN" and aborts. Turn 2 (after the synthetic reminder is
    // appended) emits a clean reply with no trigger, completing.
    let provider = MockProvider::new(vec![
        vec![
            ev(StreamEventKind::TextDelta {
                text: "let me ".into(),
            }),
            ev(StreamEventKind::TextDelta {
                text: "PLAN ".into(),
            }),
            ev(StreamEventKind::TextDelta {
                text: "the change".into(),
            }),
            ev(StreamEventKind::Finish {
                reason: FinishReason::Stop,
            }),
        ],
        vec![
            ev(StreamEventKind::TextDelta {
                text: "ok, here's the plan: do X".into(),
            }),
            ev(StreamEventKind::Finish {
                reason: FinishReason::Stop,
            }),
        ],
    ]);
    let mut cfg = build_config(provider, ToolRegistry::new());
    cfg.stream_interceptor = Some(Arc::new(InjectOnceInterceptor {
        fired: tokio::sync::Mutex::new(false),
        trigger: "PLAN".into(),
        reminder: "<system_reminder>do not plan</system_reminder>".into(),
    }));

    let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
    let (_runtime, session) = create_agent_session(cfg, Some(tx)).expect("session");
    let result = session.prompt("hi".into()).await.expect("turn ok");
    assert!(matches!(result.role, pi_ai::Role::Assistant));

    let kinds = drain(rx).await;

    // We expect an Aborted (from the interceptor), then a UserMessage
    // carrying the reminder, then a fresh AssistantStart, then a final
    // AssistantMessage with the second turn's content.
    let mut saw_abort = false;
    let mut saw_reminder_user = false;
    let mut saw_assistant_after = false;
    let mut after_abort = false;
    for k in &kinds {
        match k {
            AgentEventKind::Aborted => {
                saw_abort = true;
                after_abort = true;
            }
            AgentEventKind::UserMessage { message } if after_abort => {
                if format!("{:?}", message).contains("system_reminder") {
                    saw_reminder_user = true;
                }
            }
            AgentEventKind::AssistantMessage { message } if after_abort => {
                if format!("{:?}", message).contains("here's the plan") {
                    saw_assistant_after = true;
                }
            }
            _ => {}
        }
    }
    assert!(saw_abort, "expected Aborted from interceptor");
    assert!(
        saw_reminder_user,
        "expected synthetic user message carrying reminder"
    );
    assert!(
        saw_assistant_after,
        "expected fresh assistant turn after inject"
    );
}
