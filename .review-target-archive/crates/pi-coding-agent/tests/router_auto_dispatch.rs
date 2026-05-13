use async_trait::async_trait;
use pi_agent_core::{
    create_agent_session, default_embedding_model_path, fetch_default_embeddings,
    validate_embedding_model, ProviderFactory, RuntimeConfig, SessionManager, Settings,
};
use pi_ai::provider::EventStream;
use pi_ai::{
    AuthMethod, AuthStorage, FinishReason, GenerateRequest, ModelInfo, ModelRegistry, Provider,
    ProviderConfig, ProviderKind, Result as AiResult, StreamEvent, StreamEventKind,
};
use pi_tools::ToolRegistry;
use std::sync::{Arc, Mutex};

#[derive(Clone)]
struct CaptureProvider {
    cfg: ProviderConfig,
    seen_model: Arc<Mutex<String>>,
}

#[async_trait]
impl Provider for CaptureProvider {
    fn config(&self) -> &ProviderConfig {
        &self.cfg
    }
    fn auth(&self) -> &AuthMethod {
        static N: AuthMethod = AuthMethod::None;
        &N
    }
    async fn stream(&self, req: GenerateRequest, _model: &ModelInfo) -> AiResult<EventStream> {
        *self.seen_model.lock().unwrap() = req.model.clone();
        let s = futures::stream::iter(vec![
            Ok(StreamEvent::new(StreamEventKind::TextDelta {
                text: "ok".into(),
            })),
            Ok(StreamEvent::new(StreamEventKind::Finish {
                reason: FinishReason::Stop,
            })),
        ]);
        Ok(Box::pin(s))
    }
}

struct CaptureFactory {
    seen_model: Arc<Mutex<String>>,
}

impl ProviderFactory for CaptureFactory {
    fn build(
        &self,
        _cfg: ProviderConfig,
        _auth: AuthMethod,
    ) -> Result<Box<dyn Provider>, pi_agent_core::runtime::RuntimeError> {
        Ok(Box::new(CaptureProvider {
            cfg: ProviderConfig {
                name: "anthropic".into(),
                kind: ProviderKind::Anthropic,
                base_url: "mock".into(),
                auth_header: "x-api-key".into(),
                auth_format: "{token}".into(),
                models: vec![],
            },
            seen_model: self.seen_model.clone(),
        }))
    }
}

#[tokio::test]
async fn route_auto_flows_into_model_dispatch() {
    let model_path = default_embedding_model_path();
    if !model_path.exists() {
        fetch_default_embeddings().await.unwrap();
    }
    validate_embedding_model(&model_path).unwrap();

    let auth = AuthStorage::in_memory();
    auth.set("anthropic", AuthMethod::ApiKey { value: "k".into() });
    let seen_model = Arc::new(Mutex::new(String::new()));
    let mut settings = Settings::default();
    settings.provider = "anthropic".into();
    settings.model = "claude-sonnet-4-6".into();
    settings.route = pi_agent_core::RouteMode::Auto;
    let cfg = RuntimeConfig::builder()
        .session_manager(SessionManager::in_memory())
        .auth_storage(auth.clone())
        .model_registry(ModelRegistry::new(auth))
        .tools(ToolRegistry::new())
        .settings(settings)
        .system_prompt("system")
        .cwd(std::env::current_dir().unwrap())
        .with_provider_factory(Arc::new(CaptureFactory {
            seen_model: seen_model.clone(),
        }))
        .build_unwrap();
    let (_runtime, session) = create_agent_session(cfg, None).unwrap();
    session
        .prompt("rename foo to bar in src/lib.rs (just describe the diff)".into())
        .await
        .unwrap();
    assert_eq!(&*seen_model.lock().unwrap(), "claude-haiku-4-5-20251001");
}
