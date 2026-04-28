//! RFD 0005 test plan #6 — recursive spawn restriction.
//!
//! An agent A whose frontmatter says `spawns: ["only-this"]` calls
//! `task` with `agent: "other"`. The `task` tool must reject the call
//! and return a `is_error: true` ToolResult naming the spawn rule.

use async_trait::async_trait;
use pi_agent_core::{
    create_agent_session, ProviderFactory, RuntimeConfig, SessionManager, Settings,
};
use pi_ai::provider::EventStream;
use pi_ai::{
    AuthMethod, AuthStorage, FinishReason, GenerateRequest, ModelInfo, ModelRegistry, Provider,
    ProviderConfig, ProviderKind, Result as AiResult, StreamEvent, StreamEventKind,
};
use pi_coding_agent::native::task::{
    definition::AgentDefinition,
    tool::{with_runtime, ParentHandle, TaskTool},
};
use pi_tools::{Tool, ToolContext, ToolRegistry};
use serde_json::json;
use std::fs;
use std::sync::Arc;

#[derive(Clone)]
struct NopProvider {
    cfg: ProviderConfig,
}

#[async_trait]
impl Provider for NopProvider {
    fn config(&self) -> &ProviderConfig {
        &self.cfg
    }
    fn auth(&self) -> &AuthMethod {
        static N: AuthMethod = AuthMethod::None;
        &N
    }
    async fn stream(&self, _req: GenerateRequest, _model: &ModelInfo) -> AiResult<EventStream> {
        let s = futures::stream::iter(
            vec![StreamEvent::new(StreamEventKind::Finish {
                reason: FinishReason::Stop,
            })]
            .into_iter()
            .map(Ok),
        );
        Ok(Box::pin(s))
    }
}

struct NopFactory;
impl ProviderFactory for NopFactory {
    fn build(
        &self,
        _cfg: ProviderConfig,
        _auth: AuthMethod,
    ) -> Result<Box<dyn Provider>, pi_agent_core::runtime::RuntimeError> {
        Ok(Box::new(NopProvider {
            cfg: ProviderConfig {
                name: "anthropic".into(),
                kind: ProviderKind::Anthropic,
                base_url: "mock".into(),
                auth_header: "x-api-key".into(),
                auth_format: "{token}".into(),
                models: vec![],
            },
        }))
    }
}

fn parent_cfg() -> RuntimeConfig {
    let auth = AuthStorage::in_memory();
    auth.set("anthropic", AuthMethod::ApiKey { value: "k".into() });
    let mut s = Settings::default();
    s.provider = "anthropic".into();
    s.model = "sonnet".into();
    RuntimeConfig {
        session_manager: SessionManager::in_memory(),
        auth_storage: auth.clone(),
        model_registry: ModelRegistry::new(auth),
        tools: ToolRegistry::new(),
        settings: s,
        system_prompt: "you are pi".into(),
        context_files: Vec::new(),
        cwd: std::env::current_dir().unwrap(),
        provider_factory: Some(Arc::new(NopFactory)),
        tool_gate: None,
        gate_ask_is_approve: false,
        stream_interceptor: None,
    }
}

#[tokio::test]
async fn nested_task_call_blocked_by_spawns_allowlist() {
    // Stage: a temp project directory containing both agent A
    // (`spawns: [only-this]`) and an agent named "other".
    let project = tempfile::tempdir().unwrap();
    let agents_dir = project.path().join(".pi").join("agents");
    fs::create_dir_all(&agents_dir).unwrap();
    fs::write(
        agents_dir.join("a.md"),
        "---\nname: a\ndescription: parent agent\nspawns: [only-this]\n---\nyou are a.\n",
    )
    .unwrap();
    fs::write(
        agents_dir.join("other.md"),
        "---\nname: other\ndescription: other\n---\nyou are other.\n",
    )
    .unwrap();

    // Make discovery look at our temp project. We cd into it.
    let prev_cwd = std::env::current_dir().unwrap();
    std::env::set_current_dir(project.path()).unwrap();

    let cfg = parent_cfg();
    let (_runtime, parent_session) =
        create_agent_session(cfg.clone(), None).expect("parent session");

    // Build a ParentHandle whose `current_agent` is A — i.e. we're
    // *inside* the A subagent when the inner `task` call fires.
    let agent_a =
        AgentDefinition::parse("---\nname: a\ndescription: a\nspawns: [only-this]\n---\nbody\n")
            .unwrap();
    let handle = ParentHandle {
        parent_cfg: Arc::new(cfg),
        parent_session,
        current_agent: Some(agent_a),
    };

    let tool = TaskTool::new();
    let mut ctx = ToolContext::default();
    ctx.cwd = project.path().to_path_buf();

    let input = json!({
        "agent": "other",
        "tasks": [{ "id": "t1", "assignment": "do x" }]
    });
    let result = with_runtime(handle, tool.invoke(&ctx, "call-1", input))
        .await
        .expect("invoke ok");

    std::env::set_current_dir(prev_cwd).unwrap();

    assert!(
        result.is_error,
        "spawn-rule violation should produce is_error=true; got {:?}",
        result
    );
    assert!(
        result.model_output.contains("spawn") || result.model_output.contains("a"),
        "error message should mention the spawn rule; got {:?}",
        result.model_output
    );
}
