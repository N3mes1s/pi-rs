//! Integration test for RFD 0022 `--sandbox-provider` CLI flag.
//!
//! Verifies that:
//! 1. Parsing `--sandbox-provider local-process` succeeds.
//! 2. `install_sandbox_from_flag` wires a `LocalProcessProvider` into the config.
//! 3. An unknown provider name returns a clean error.
//! 4. Running a prompt that triggers a tool call produces exactly one
//!    `SessionEntryKind::SandboxAction` entry with provider == "local-process"
//!    and is_error == false.

use async_trait::async_trait;
use pi_agent_core::{
    create_agent_session, ProviderFactory, RuntimeConfig, SessionEntryKind, SessionManager,
    Settings,
};
use pi_ai::{
    AuthMethod, AuthStorage, FinishReason, GenerateRequest, ModelInfo, ModelRegistry, Provider,
    ProviderConfig, ProviderKind, Result as AiResult, StreamEvent, StreamEventKind,
};
use pi_ai::provider::EventStream;
use pi_coding_agent::startup::install_sandbox_from_flag;
use pi_tools::{Tool, ToolContext, ToolError, ToolRegistry};
use serde_json::json;
use std::sync::{Arc, Mutex as StdMutex};

// ── minimal mock provider ────────────────────────────────────────────────────

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

// ── a simple echo tool (safe to call inline) ─────────────────────────────────

struct EchoTool;
#[async_trait]
impl Tool for EchoTool {
    fn spec(&self) -> pi_ai::ToolSpec {
        pi_ai::ToolSpec {
            name: "echo".into(),
            description: "echo the input".into(),
            input_schema: json!({"type":"object","properties":{"msg":{"type":"string"}}}),
        }
    }
    fn read_only(&self) -> bool {
        true
    }
    async fn invoke(
        &self,
        _ctx: &ToolContext,
        _call_id: &str,
        input: serde_json::Value,
    ) -> Result<pi_ai::ToolResult, ToolError> {
        let msg = input
            .get("msg")
            .and_then(|v| v.as_str())
            .unwrap_or("(empty)");
        Ok(pi_ai::ToolResult {
            tool_use_id: _call_id.to_string(),
            model_output: msg.to_string(),
            display: None,
            is_error: false,
        })
    }
}

// ── helpers ──────────────────────────────────────────────────────────────────

fn ev(k: StreamEventKind) -> StreamEvent {
    StreamEvent::new(k)
}

/// Two-turn script: tool-call then done.
fn one_tool_call_then_done() -> Vec<Vec<StreamEvent>> {
    vec![
        vec![
            ev(StreamEventKind::ToolCallComplete {
                id: "tu_1".into(),
                name: "echo".into(),
                input: json!({"msg": "hello"}),
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

fn base_config(provider: MockProvider) -> RuntimeConfig {
    let auth = AuthStorage::in_memory();
    auth.set("anthropic", AuthMethod::ApiKey { value: "k".into() });
    let mut settings = Settings::default();
    settings.provider = "anthropic".into();
    settings.model = "sonnet".into();
    let mut tools = ToolRegistry::new();
    tools.register(Arc::new(EchoTool)).expect("unique");
    RuntimeConfig::builder()
        .session_manager(SessionManager::in_memory())
        .auth_storage(auth.clone())
        .model_registry(ModelRegistry::new(auth))
        .tools(tools)
        .settings(settings)
        .system_prompt("you are pi")
        .cwd(std::env::current_dir().unwrap())
        .with_provider_factory(Arc::new(MockFactory { inner: provider }))
        .build_unwrap()
}

// ── tests ─────────────────────────────────────────────────────────────────────

/// The flag value `"local-process"` must parse without error and set a
/// non-None sandbox_provider.
#[test]
fn install_sandbox_from_flag_local_process_sets_provider() {
    let mut cfg = base_config(MockProvider::new(vec![]));
    assert!(cfg.sandbox_provider.is_none());
    install_sandbox_from_flag(&mut cfg, "local-process").expect("should succeed");
    assert!(
        cfg.sandbox_provider.is_some(),
        "sandbox_provider must be Some after install_sandbox_from_flag"
    );
    assert_eq!(
        cfg.sandbox_provider.as_ref().unwrap().name(),
        "local-process"
    );
}

/// An unknown provider name must return a clean error.
#[test]
fn install_sandbox_from_flag_unknown_returns_error() {
    let mut cfg = base_config(MockProvider::new(vec![]));
    let err = install_sandbox_from_flag(&mut cfg, "docker")
        .expect_err("unknown provider should fail");
    let msg = err.to_string();
    assert!(
        msg.contains("unknown sandbox provider"),
        "error should mention 'unknown sandbox provider', got: {msg}"
    );
    assert!(
        msg.contains("docker"),
        "error should echo the unknown name, got: {msg}"
    );
    assert!(
        msg.contains("local-process"),
        "error should list valid options, got: {msg}"
    );
}

/// Running a prompt with `local-process` sandbox wired produces exactly one
/// `SessionEntryKind::SandboxAction` with provider == "local-process" and
/// is_error == false.
#[tokio::test]
async fn sandbox_cli_flag_produces_sandbox_action_entry() {
    let mgr = SessionManager::in_memory();
    let mut cfg = base_config(MockProvider::new(one_tool_call_then_done()));
    cfg.session_manager = mgr.clone();

    install_sandbox_from_flag(&mut cfg, "local-process").expect("install ok");

    let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
    let (_runtime, session) = create_agent_session(cfg, Some(tx)).expect("session");
    session.prompt("go".into()).await.expect("prompt ok");

    let branch = mgr.current_branch(&session.id);
    let sandbox_entries: Vec<_> = branch
        .iter()
        .filter_map(|e| {
            if let SessionEntryKind::SandboxAction {
                provider,
                is_error,
                ..
            } = &e.kind
            {
                Some((provider.clone(), *is_error))
            } else {
                None
            }
        })
        .collect();

    assert_eq!(
        sandbox_entries.len(),
        1,
        "expected exactly one SandboxAction entry, got {}: {branch:?}",
        sandbox_entries.len()
    );
    let (provider, is_error) = &sandbox_entries[0];
    assert_eq!(
        provider, "local-process",
        "SandboxAction.provider must be 'local-process'"
    );
    assert!(
        !is_error,
        "SandboxAction.is_error must be false for a successful echo call"
    );
}
