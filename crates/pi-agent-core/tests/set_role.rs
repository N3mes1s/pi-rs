//! B1: AgentSession::set_role wires settings.roles into the active model.

use async_trait::async_trait;
use pi_agent_core::settings::{ModelRoles, Role};
use pi_agent_core::{
    create_agent_session, ProviderFactory, RuntimeConfig, SessionManager, Settings,
};
use pi_ai::provider::EventStream;
use pi_ai::{
    AuthMethod, AuthStorage, GenerateRequest, ModelInfo, ModelRegistry, Provider, ProviderConfig,
    ProviderKind, Result as AiResult,
};
use pi_tools::ToolRegistry;
use std::sync::{Arc, Mutex as StdMutex};

#[derive(Clone)]
struct NullProvider {
    cfg: ProviderConfig,
    last_model: Arc<StdMutex<Option<String>>>,
}

#[async_trait]
impl Provider for NullProvider {
    fn config(&self) -> &ProviderConfig {
        &self.cfg
    }
    fn auth(&self) -> &AuthMethod {
        static N: AuthMethod = AuthMethod::None;
        &N
    }
    async fn stream(&self, _req: GenerateRequest, model: &ModelInfo) -> AiResult<EventStream> {
        *self.last_model.lock().unwrap() = Some(model.id.clone());
        Ok(Box::pin(futures::stream::empty()))
    }
}

struct NullFactory(NullProvider);
impl ProviderFactory for NullFactory {
    fn build(
        &self,
        _cfg: ProviderConfig,
        _auth: AuthMethod,
    ) -> Result<Box<dyn Provider>, pi_agent_core::runtime::RuntimeError> {
        Ok(Box::new(self.0.clone()))
    }
}

fn cfg(settings: Settings) -> RuntimeConfig {
    let auth = AuthStorage::in_memory();
    auth.set("anthropic", AuthMethod::ApiKey { value: "k".into() });
    let provider = NullProvider {
        cfg: ProviderConfig {
            name: "anthropic".into(),
            kind: ProviderKind::Anthropic,
            base_url: "mock".into(),
            auth_header: "x-api-key".into(),
            auth_format: "{token}".into(),
            models: vec![],
        },
        last_model: Arc::new(StdMutex::new(None)),
    };
    RuntimeConfig {
        session_manager: SessionManager::in_memory(),
        auth_storage: auth.clone(),
        model_registry: ModelRegistry::new(auth),
        tools: ToolRegistry::new(),
        settings,
        system_prompt: "".into(),
        context_files: Vec::new(),
        cwd: std::env::current_dir().unwrap(),
        provider_factory: Some(Arc::new(NullFactory(provider))),
        tool_gate: None,
        gate_ask_is_approve: false,
        stream_interceptor: None,
    }
}

#[tokio::test]
async fn set_role_smol_swaps_to_role_model() {
    let mut settings = Settings::default();
    settings.provider = "anthropic".into();
    settings.model = "sonnet".into();
    let roles = ModelRoles {
        smol: Some("haiku".into()),
        ..Default::default()
    };
    settings.roles = roles.clone();

    let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
    let (_runtime, session) = create_agent_session(cfg(settings), Some(tx)).expect("session");

    let chosen = session.set_role(Role::Smol, &roles).await;
    assert_eq!(chosen, "haiku");
}

#[tokio::test]
async fn set_role_unset_keeps_current_model() {
    let mut settings = Settings::default();
    settings.provider = "anthropic".into();
    settings.model = "sonnet".into();
    // No smol override.
    let roles = ModelRoles::default();
    settings.roles = roles.clone();

    let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
    let (_runtime, session) = create_agent_session(cfg(settings), Some(tx)).expect("session");

    let chosen = session.set_role(Role::Smol, &roles).await;
    assert_eq!(chosen, "sonnet");
}

#[tokio::test]
async fn set_role_with_provider_qualified_model_splits() {
    let mut settings = Settings::default();
    settings.provider = "anthropic".into();
    settings.model = "sonnet".into();
    let roles = ModelRoles {
        slow: Some("openai/o3-mini".into()),
        ..Default::default()
    };
    settings.roles = roles.clone();

    let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
    let (_runtime, session) = create_agent_session(cfg(settings), Some(tx)).expect("session");

    let chosen = session.set_role(Role::Slow, &roles).await;
    assert_eq!(chosen, "openai/o3-mini");
}
