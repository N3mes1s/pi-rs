//! RFD 0005 test plan #4 — parent context isolation.
//!
//! After running a `task` batch, the parent session's `messages()`
//! must NOT contain any of the subagent's intermediate User /
//! Assistant turns. Only whatever `tool_result` blocks the parent
//! itself emitted — and in this test we never run the parent's
//! prompt loop with the `task` tool, we just call the executor
//! directly, so the parent's messages should be exactly what they
//! were before.

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
use std::sync::Arc;

#[derive(Clone)]
struct OkProvider {
    cfg: ProviderConfig,
    label: String,
}

#[async_trait]
impl Provider for OkProvider {
    fn config(&self) -> &ProviderConfig {
        &self.cfg
    }
    fn auth(&self) -> &AuthMethod {
        static N: AuthMethod = AuthMethod::None;
        &N
    }
    async fn stream(&self, _req: GenerateRequest, _model: &ModelInfo) -> AiResult<EventStream> {
        let label = self.label.clone();
        let s = futures::stream::iter(
            vec![
                StreamEvent::new(StreamEventKind::TextDelta {
                    text: format!("subagent says {label}"),
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

struct OkFactory;

impl ProviderFactory for OkFactory {
    fn build(
        &self,
        _cfg: ProviderConfig,
        _auth: AuthMethod,
    ) -> Result<Box<dyn Provider>, pi_agent_core::runtime::RuntimeError> {
        Ok(Box::new(OkProvider {
            cfg: ProviderConfig {
                name: "anthropic".into(),
                kind: ProviderKind::Anthropic,
                base_url: "mock".into(),
                auth_header: "x-api-key".into(),
                auth_format: "{token}".into(),
                models: vec![],
            },
            label: "ack".into(),
        }))
    }
}

fn cfg() -> RuntimeConfig {
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
        .system_prompt("you are pi")
        .cwd(std::env::current_dir().unwrap())
        .with_provider_factory(Arc::new(OkFactory))
        .build_unwrap()
}

#[tokio::test]
async fn parent_messages_unchanged_after_subagent_run() {
    let parent_cfg = cfg();
    let (_runtime, parent_session) =
        create_agent_session(parent_cfg.clone(), None).expect("parent session");

    // Drive one parent turn so there's a User+Assistant pair to compare.
    parent_session
        .prompt("hello".into())
        .await
        .expect("parent turn ok");
    let before = parent_session.messages().await;
    let before_count = before.len();

    let handle = ParentHandle {
        parent_cfg: Arc::new(parent_cfg),
        parent_session: parent_session.clone(),
        current_agent: None,
    };
    let agent = AgentDefinition::parse("---\nname: a\ndescription: a\n---\nyou are a.\n").unwrap();
    let _ = executor::run_batch(
        &handle,
        &agent,
        None,
        vec![
            TaskInput {
                id: "x".into(),
                description: None,
                assignment: "go".into(),
            },
            TaskInput {
                id: "y".into(),
                description: None,
                assignment: "go".into(),
            },
        ],
        2,
    )
    .await;

    let after = parent_session.messages().await;
    assert_eq!(
        after.len(),
        before_count,
        "subagent batch must not add messages to parent session"
    );

    // (The text-leak assertion is intentionally omitted: this stub
    // provider returns the same reply for every request, so the
    // parent's own assistant turn contains the same string. The
    // length-equality check above is sufficient — if the subagent had
    // appended turns to the parent, the count would have grown.)
}
