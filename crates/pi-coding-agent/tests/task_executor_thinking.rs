//! Subagent executor thinking-level coverage.
//!
//! Confirms that frontmatter `thinking: xhigh` is preserved when the
//! child request is sent to the provider.

use async_trait::async_trait;
use pi_agent_core::{
    create_agent_session, ProviderFactory, RuntimeConfig, SessionManager, Settings,
};
use pi_ai::provider::EventStream;
use pi_ai::{
    AuthMethod, AuthStorage, FinishReason, GenerateRequest, ModelInfo, ModelRegistry, Provider,
    ProviderConfig, ProviderKind, Result as AiResult, StreamEvent, StreamEventKind, ThinkingLevel,
};
use pi_coding_agent::native::task::{
    definition::AgentDefinition,
    executor::{self, TaskInput},
    tool::ParentHandle,
};
use pi_tools::ToolRegistry;
use std::sync::{Arc, Mutex as StdMutex};

#[derive(Clone)]
struct CaptureProvider {
    cfg: ProviderConfig,
    seen: Arc<StdMutex<Vec<ThinkingLevel>>>,
}

impl CaptureProvider {
    fn new(seen: Arc<StdMutex<Vec<ThinkingLevel>>>) -> Self {
        Self {
            cfg: ProviderConfig {
                name: "anthropic".into(),
                kind: ProviderKind::Anthropic,
                base_url: "mock".into(),
                auth_header: "x-api-key".into(),
                auth_format: "{token}".into(),
                models: vec![],
            },
            seen,
        }
    }
}

#[async_trait]
impl Provider for CaptureProvider {
    fn config(&self) -> &ProviderConfig {
        &self.cfg
    }

    fn auth(&self) -> &AuthMethod {
        static NONE: AuthMethod = AuthMethod::None;
        &NONE
    }

    async fn stream(&self, req: GenerateRequest, _model: &ModelInfo) -> AiResult<EventStream> {
        self.seen.lock().unwrap().push(req.thinking);
        let stream = futures::stream::iter(
            vec![
                StreamEvent::new(StreamEventKind::TextDelta {
                    text: "done".into(),
                }),
                StreamEvent::new(StreamEventKind::Finish {
                    reason: FinishReason::Stop,
                }),
            ]
            .into_iter()
            .map(Ok),
        );
        Ok(Box::pin(stream))
    }
}

struct CaptureFactory {
    seen: Arc<StdMutex<Vec<ThinkingLevel>>>,
}

impl ProviderFactory for CaptureFactory {
    fn build(
        &self,
        _cfg: ProviderConfig,
        _auth: AuthMethod,
    ) -> Result<Box<dyn Provider>, pi_agent_core::runtime::RuntimeError> {
        Ok(Box::new(CaptureProvider::new(self.seen.clone())))
    }
}

fn build_parent_cfg(factory: Arc<dyn ProviderFactory>) -> RuntimeConfig {
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
        .system_prompt("parent system")
        .cwd(std::env::current_dir().unwrap())
        .with_provider_factory(factory)
        .build_unwrap()
}

#[tokio::test]
async fn executor_preserves_xhigh_subagent_thinking() {
    let seen = Arc::new(StdMutex::new(Vec::new()));
    let factory = Arc::new(CaptureFactory { seen: seen.clone() });
    let cfg = build_parent_cfg(factory);
    let (_runtime, parent_session) = create_agent_session(cfg.clone(), None).expect("session");

    let handle = ParentHandle {
        parent_cfg: Arc::new(cfg),
        parent_session,
        current_agent: None,
    };
    let agent = AgentDefinition::parse(
        "---\nname: stub\ndescription: stub agent\nthinking: xhigh\n---\nyou are a stub.\n",
    )
    .unwrap();
    let task = TaskInput {
        id: "t1".into(),
        description: None,
        assignment: "do thing".into(),
    };

    let outcome = executor::run_one(&handle, &agent, None, &task)
        .await
        .expect("run one");

    assert!(outcome.success, "subagent run should succeed");
    assert_eq!(seen.lock().unwrap().as_slice(), &[ThinkingLevel::XHigh]);
}
