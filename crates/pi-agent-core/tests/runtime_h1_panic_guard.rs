//! Hardening §4.5 #1 (RFD 0027 Commit H1): a panicking custom Tool
//! must NOT kill the tokio worker thread. The runtime catches the
//! panic, returns a synthetic ToolResult with `is_error = true`, and
//! continues processing.

use async_trait::async_trait;
use pi_agent_core::event::{AgentEvent, AgentEventKind};
use pi_agent_core::{
    create_agent_session, ProviderFactory, RuntimeConfig, SessionManager, Settings,
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
    ) -> Result<Box<dyn Provider>, pi_agent_core::RuntimeError> {
        Ok(Box::new(self.0.clone()))
    }
}

/// Custom Tool whose `invoke()` always panics. Pre-H1, this killed the
/// tokio worker thread. Post-H1, the runtime catches the panic and
/// returns the panic message as a tool error.
struct PanickingTool;

#[async_trait]
impl Tool for PanickingTool {
    fn spec(&self) -> pi_ai::ToolSpec {
        pi_ai::ToolSpec {
            name: "panic_me".into(),
            description: "always panics".into(),
            input_schema: json!({"type":"object"}),
        }
    }
    fn read_only(&self) -> bool {
        true
    }
    async fn invoke(
        &self,
        _ctx: &ToolContext,
        _call_id: &str,
        _input: serde_json::Value,
    ) -> Result<pi_ai::ToolResult, ToolError> {
        panic!("intentional panic for H1 regression test");
    }
}

fn ev(k: StreamEventKind) -> StreamEvent {
    StreamEvent::new(k)
}

#[tokio::test]
async fn panicking_tool_does_not_crash_runtime() {
    // Turn 1: the assistant calls panic_me.
    // Turn 2: the assistant returns a final stop after seeing the
    //         tool error in the next user message.
    let turns = vec![
        vec![
            ev(StreamEventKind::ToolCallComplete {
                id: "tu_1".into(),
                name: "panic_me".into(),
                input: json!({}),
            }),
            ev(StreamEventKind::Finish {
                reason: FinishReason::ToolUse,
            }),
        ],
        vec![
            ev(StreamEventKind::TextDelta { text: "ok".into() }),
            ev(StreamEventKind::Finish {
                reason: FinishReason::Stop,
            }),
        ],
    ];

    let auth = AuthStorage::in_memory();
    auth.set("anthropic", AuthMethod::ApiKey { value: "k".into() });
    let mut settings = Settings::default();
    settings.provider = "anthropic".into();
    settings.model = "sonnet".into();

    let mut tools = ToolRegistry::new();
    tools.register(Arc::new(PanickingTool));

    let cfg = RuntimeConfig::builder()
        .session_manager(SessionManager::in_memory())
        .auth_storage(auth.clone())
        .model_registry(ModelRegistry::new(auth))
        .tools(tools)
        .settings(settings)
        .system_prompt("you are pi")
        .cwd(std::env::current_dir().unwrap())
        .with_provider_factory(Arc::new(MockFactory(MockProvider::new(turns))))
        .build_unwrap();

    let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
    let (_runtime, session) = create_agent_session(cfg, Some(tx)).expect("session");

    // The prompt() call must NOT panic the test thread (and by
    // extension would not have panicked the embedder's runtime).
    let result = session.prompt("trigger panic_me".into()).await;
    assert!(result.is_ok(), "runtime must survive panicking tool, got {result:?}");

    let events = drain(rx).await;
    let tool_result_event = events.iter().find_map(|e| match &e {
        AgentEventKind::ToolResult { result } => Some(result),
        _ => None,
    });
    let tr = tool_result_event.expect("a ToolResult must be emitted even when the tool panicked");
    assert!(
        tr.is_error,
        "panicking tool must produce a ToolResult with is_error=true"
    );
    let out = tr.model_output.to_lowercase();
    assert!(
        out.contains("panic") || out.contains("intentional"),
        "model_output should mention the panic, got: {}",
        tr.model_output
    );
}

async fn drain(mut rx: UnboundedReceiver<AgentEvent>) -> Vec<AgentEventKind> {
    let mut out = Vec::new();
    while let Ok(ev) = tokio::time::timeout(std::time::Duration::from_millis(100), rx.recv()).await
    {
        match ev {
            Some(e) => out.push(e.kind),
            None => break,
        }
    }
    out
}
