//! RFD 0005 test plan #3 — executor against a stub provider.
//!
//! Confirms that `executor::run_one` produces exactly one `TaskOutcome`
//! per input task and that the parent's `messages()` is unchanged
//! after the subagent runs.

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
    executor::{self, TaskInput},
    tool::ParentHandle,
};
use pi_tools::ToolRegistry;
use std::sync::{Arc, Mutex as StdMutex};

#[derive(Clone)]
struct EchoProvider {
    cfg: ProviderConfig,
    last_user: Arc<StdMutex<String>>,
}

impl EchoProvider {
    fn new(last_user: Arc<StdMutex<String>>) -> Self {
        Self {
            cfg: ProviderConfig {
                name: "anthropic".into(),
                kind: ProviderKind::Anthropic,
                base_url: "mock".into(),
                auth_header: "x-api-key".into(),
                auth_format: "{token}".into(),
                models: vec![],
            },
            last_user,
        }
    }
}

#[async_trait]
impl Provider for EchoProvider {
    fn config(&self) -> &ProviderConfig {
        &self.cfg
    }
    fn auth(&self) -> &AuthMethod {
        static N: AuthMethod = AuthMethod::None;
        &N
    }
    async fn stream(&self, req: GenerateRequest, _model: &ModelInfo) -> AiResult<EventStream> {
        // Capture the last user message text from the request.
        let last = req
            .messages
            .iter()
            .rev()
            .find(|m| matches!(m.role, pi_ai::Role::User))
            .map(|m| m.text())
            .unwrap_or_default();
        *self.last_user.lock().unwrap() = last.clone();
        // Echo the task id back in a one-shot reply.
        let id = last
            .lines()
            .find(|l| l.starts_with("## Task `"))
            .and_then(|l| l.strip_prefix("## Task `"))
            .and_then(|s| s.split('`').next())
            .unwrap_or("unknown")
            .to_string();
        let s = futures::stream::iter(
            vec![
                StreamEvent::new(StreamEventKind::TextDelta {
                    text: format!("OK: {id}"),
                }),
                StreamEvent::new(StreamEventKind::Finish {
                    reason: FinishReason::Stop,
                }),
            ]
            .into_iter()
            .map(Ok),
        );
        Ok(Box::pin(s))
    }
}

struct EchoFactory {
    last_user: Arc<StdMutex<String>>,
}

impl ProviderFactory for EchoFactory {
    fn build(
        &self,
        _cfg: ProviderConfig,
        _auth: AuthMethod,
    ) -> Result<Box<dyn Provider>, pi_agent_core::runtime::RuntimeError> {
        Ok(Box::new(EchoProvider::new(self.last_user.clone())))
    }
}

fn build_parent_cfg(factory: Arc<dyn ProviderFactory>) -> RuntimeConfig {
    let auth = AuthStorage::in_memory();
    auth.set("anthropic", AuthMethod::ApiKey { value: "k".into() });
    let mut s = Settings::default();
    s.provider = "anthropic".into();
    s.model = "sonnet".into();
    RuntimeConfig::builder()
        .session_manager(SessionManager::in_memory())
        .auth_storage(auth.clone())
        .model_registry(ModelRegistry::new(auth))
        .tools(ToolRegistry::new())
        .settings(s)
        .system_prompt("parent system")
        .cwd(std::env::current_dir().unwrap())
        .with_provider_factory(factory)
        .build_unwrap()
}

fn agent_def() -> AgentDefinition {
    AgentDefinition::parse("---\nname: stub\ndescription: stub agent\n---\nyou are a stub.\n")
        .unwrap()
}

#[tokio::test]
async fn executor_runs_each_task_and_does_not_pollute_parent() {
    let last_user = Arc::new(StdMutex::new(String::new()));
    let factory = Arc::new(EchoFactory {
        last_user: last_user.clone(),
    });
    let cfg = build_parent_cfg(factory);
    let (_runtime, parent_session) = create_agent_session(cfg.clone(), None).expect("session");

    // Seed the parent with a user prompt so synth_first_message has
    // something to fork from. (We don't run the parent's prompt loop —
    // we just splice a User message via the public API.)
    // The simplest way: append a User entry directly. But messages() is
    // populated only by the run loop. Instead, drive a no-op prompt via
    // the same factory: Echo replies "OK: …" and we move on.
    let _ = parent_session
        .prompt("explain this codebase".into())
        .await
        .expect("seed prompt");
    let parent_msgs_before = parent_session.messages().await;

    // Build ParentHandle.
    let handle = ParentHandle {
        parent_cfg: Arc::new(cfg),
        parent_session: parent_session.clone(),
        current_agent: None,
    };

    let agent = agent_def();
    let tasks = vec![
        TaskInput {
            id: "t1".into(),
            description: Some("first".into()),
            assignment: "do thing 1".into(),
        },
        TaskInput {
            id: "t2".into(),
            description: None,
            assignment: "do thing 2".into(),
        },
    ];

    let result = executor::run_batch(&handle, &agent, None, tasks, 4).await;
    assert_eq!(result.results.len(), 2, "one outcome per input task");
    // Outcomes can come back in any order due to buffer_unordered.
    let mut ids: Vec<&str> = result.results.iter().map(|r| r.id.as_str()).collect();
    ids.sort();
    assert_eq!(ids, vec!["t1", "t2"]);
    for r in &result.results {
        assert!(r.success, "task {} failed: {:?}", r.id, r.error);
        assert!(r.model_output.contains(&format!("OK: {}", r.id)));
    }

    // Parent transcript must be unchanged.
    let parent_msgs_after = parent_session.messages().await;
    assert_eq!(
        parent_msgs_after.len(),
        parent_msgs_before.len(),
        "subagent should not append to parent transcript"
    );
}
