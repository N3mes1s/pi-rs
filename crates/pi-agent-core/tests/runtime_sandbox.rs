//! RFD 0022 — when a `SandboxProvider` is installed on `RuntimeConfig`,
//! tool calls must dispatch through `provider.execute_tool()` instead of
//! the inline `Tool::invoke()` path. The provider's stdout becomes the
//! ToolResult's `model_output`; non-zero exit_status flips `is_error`.

use async_trait::async_trait;
use pi_agent_core::event::AgentEventKind;
use pi_agent_core::{
    create_agent_session, ProviderFactory, RuntimeConfig, SessionEntryKind, SessionManager,
    Settings,
};
use pi_ai::provider::EventStream;
use pi_ai::{
    AuthMethod, AuthStorage, FinishReason, GenerateRequest, ModelInfo, ModelRegistry, Provider,
    ProviderConfig, ProviderKind, Result as AiResult, StreamEvent, StreamEventKind,
};
use pi_sandbox::{SandboxError, SandboxExecution, SandboxProvider};
use pi_tools::{Tool, ToolContext, ToolError, ToolRegistry};
use serde_json::json;
use std::sync::atomic::{AtomicUsize, Ordering};
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

/// A tool whose `invoke()` panics — proves the runtime did NOT take the
/// inline path. The sandbox provider must short-circuit to its own
/// `execute_tool()` before the inline branch is reached.
struct PoisonTool;
#[async_trait]
impl Tool for PoisonTool {
    fn spec(&self) -> pi_ai::ToolSpec {
        pi_ai::ToolSpec {
            name: "echo".into(),
            description: "poisoned inline path".into(),
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
        panic!("inline path must not run when a SandboxProvider is configured");
    }
}

/// A sandbox provider that records every call and returns a fixed
/// stdout. Exit status is configurable per-call so we can prove the
/// is_error mapping (exit_status != 0 → is_error=true).
struct CountingSandbox {
    calls: Arc<AtomicUsize>,
    last_tool: Arc<StdMutex<String>>,
    exit_status: i32,
}
#[async_trait]
impl SandboxProvider for CountingSandbox {
    fn name(&self) -> &'static str {
        "counting-test-sandbox"
    }
    async fn execute_tool(
        &self,
        _ctx: &ToolContext,
        tool_name: &str,
        _input: &serde_json::Value,
    ) -> Result<SandboxExecution, SandboxError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        *self.last_tool.lock().unwrap() = tool_name.to_string();
        Ok(SandboxExecution {
            stdout: format!("sandboxed:{tool_name}"),
            stderr: String::new(),
            exit_status: self.exit_status,
            round_trip_ms: None,
            cost_usd: None,
        })
    }
}

fn ev(k: StreamEventKind) -> StreamEvent {
    StreamEvent::new(k)
}

fn one_tool_call_then_done() -> Vec<Vec<StreamEvent>> {
    vec![
        vec![
            ev(StreamEventKind::ToolCallComplete {
                id: "tu_1".into(),
                name: "echo".into(),
                input: json!({"x": 1}),
            }),
            ev(StreamEventKind::Finish {
                reason: FinishReason::ToolUse,
            }),
        ],
        vec![ev(StreamEventKind::Finish {
            reason: FinishReason::Stop,
        })],
    ]
}

fn build_cfg(
    provider: MockProvider,
    sandbox: Option<Arc<dyn SandboxProvider>>,
) -> RuntimeConfig {
    let auth = AuthStorage::in_memory();
    auth.set("anthropic", AuthMethod::ApiKey { value: "k".into() });
    let mut settings = Settings::default();
    settings.provider = "anthropic".into();
    settings.model = "sonnet".into();
    let mut tools = ToolRegistry::new();
    tools.register(Arc::new(PoisonTool)).expect("unique");
    let mut builder = RuntimeConfig::builder()
        .session_manager(SessionManager::in_memory())
        .auth_storage(auth.clone())
        .model_registry(ModelRegistry::new(auth))
        .tools(tools)
        .settings(settings)
        .system_prompt("you are pi")
        .cwd(std::env::current_dir().unwrap())
        .with_provider_factory(Arc::new(MockFactory { inner: provider }));
    if let Some(s) = sandbox {
        builder = builder.with_sandbox_provider(s);
    }
    builder.build_unwrap()
}

#[tokio::test]
async fn sandbox_provider_intercepts_tool_dispatch() {
    let calls = Arc::new(AtomicUsize::new(0));
    let last_tool = Arc::new(StdMutex::new(String::new()));
    let sandbox = Arc::new(CountingSandbox {
        calls: calls.clone(),
        last_tool: last_tool.clone(),
        exit_status: 0,
    });

    let cfg = build_cfg(
        MockProvider::new(one_tool_call_then_done()),
        Some(sandbox),
    );

    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    let (_runtime, session) = create_agent_session(cfg, Some(tx)).expect("session");
    session.prompt("go".into()).await.expect("prompt ok");

    // Sandbox.execute_tool was hit exactly once for the single tool call.
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert_eq!(*last_tool.lock().unwrap(), "echo");

    // ToolResult event surfaces the sandbox stdout, with is_error=false
    // because exit_status == 0.
    let mut saw_sandboxed_result = false;
    while let Ok(Some(e)) =
        tokio::time::timeout(std::time::Duration::from_millis(50), rx.recv()).await
    {
        if let AgentEventKind::ToolResult { result } = e.kind {
            assert_eq!(result.model_output, "sandboxed:echo");
            assert!(!result.is_error);
            saw_sandboxed_result = true;
        }
    }
    assert!(saw_sandboxed_result, "expected a sandboxed ToolResult event");
}

#[tokio::test]
async fn sandbox_emits_action_entry_before_tool_result_in_session() {
    let calls = Arc::new(AtomicUsize::new(0));
    let sandbox = Arc::new(CountingSandbox {
        calls: calls.clone(),
        last_tool: Arc::new(StdMutex::new(String::new())),
        exit_status: 0,
    });

    let mgr = SessionManager::in_memory();
    let mut cfg = build_cfg(
        MockProvider::new(one_tool_call_then_done()),
        Some(sandbox),
    );
    cfg.session_manager = mgr.clone();

    let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
    let (_runtime, session) = create_agent_session(cfg, Some(tx)).expect("session");
    session.prompt("go".into()).await.expect("prompt ok");

    let branch = mgr.current_branch(&session.id);
    let mut sandbox_idx: Option<usize> = None;
    let mut tool_result_idx: Option<usize> = None;
    for (i, e) in branch.iter().enumerate() {
        match &e.kind {
            SessionEntryKind::SandboxAction {
                provider,
                tool_name,
                exit_status,
                is_error,
                ..
            } => {
                assert_eq!(provider, "counting-test-sandbox");
                assert_eq!(tool_name, "echo");
                assert_eq!(*exit_status, 0);
                assert!(!*is_error);
                sandbox_idx = Some(i);
            }
            SessionEntryKind::ToolResult { .. } => {
                tool_result_idx = Some(i);
            }
            _ => {}
        }
    }
    let s = sandbox_idx.expect("SandboxAction entry must be appended");
    let r = tool_result_idx.expect("ToolResult entry must be appended");
    assert!(
        s < r,
        "SandboxAction must precede ToolResult in the session log (got s={s}, r={r})"
    );
}

#[tokio::test]
async fn sandbox_nonzero_exit_marks_tool_result_is_error() {
    let calls = Arc::new(AtomicUsize::new(0));
    let sandbox = Arc::new(CountingSandbox {
        calls: calls.clone(),
        last_tool: Arc::new(StdMutex::new(String::new())),
        exit_status: 42,
    });

    let cfg = build_cfg(
        MockProvider::new(one_tool_call_then_done()),
        Some(sandbox),
    );

    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    let (_runtime, session) = create_agent_session(cfg, Some(tx)).expect("session");
    session.prompt("go".into()).await.expect("prompt ok");

    let mut error_count = 0;
    while let Ok(Some(e)) =
        tokio::time::timeout(std::time::Duration::from_millis(50), rx.recv()).await
    {
        if let AgentEventKind::ToolResult { result } = e.kind {
            if result.is_error {
                error_count += 1;
            }
        }
    }
    assert_eq!(
        error_count, 1,
        "non-zero sandbox exit_status must produce exactly one is_error=true ToolResult"
    );
}
