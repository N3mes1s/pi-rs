//! RFD 0012: the runtime appends one [`SessionEntryKind::ContextLoad`]
//! per `RuntimeConfig.context_files` element on first prompt, ordered
//! before the first User entry, and never re-emits on subsequent prompts.

use async_trait::async_trait;
use pi_agent_core::{
    create_agent_session, ContextFile, ProviderFactory, RuntimeConfig, SessionEntryKind,
    SessionManager, Settings,
};
use pi_ai::provider::EventStream;
use pi_ai::{
    AuthMethod, AuthStorage, FinishReason, GenerateRequest, ModelInfo, ModelRegistry, Provider,
    ProviderConfig, ProviderKind, Result as AiResult, StreamEvent, StreamEventKind,
};
use pi_tools::ToolRegistry;
use std::path::PathBuf;
use std::sync::{Arc, Mutex as StdMutex};

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
        let s = futures::stream::iter(turn.into_iter().map(Ok));
        Ok(Box::pin(s))
    }
}

struct MockFactory(MockProvider);
impl ProviderFactory for MockFactory {
    fn build(
        &self,
        _cfg: ProviderConfig,
        _auth: AuthMethod,
    ) -> Result<Box<dyn Provider>, pi_agent_core::runtime::RuntimeError> {
        Ok(Box::new(self.0.clone()))
    }
}

fn ev(k: StreamEventKind) -> StreamEvent {
    StreamEvent::new(k)
}

fn build_cfg(provider: MockProvider, context_files: Vec<ContextFile>) -> RuntimeConfig {
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
        .system_prompt("you are pi")
        .with_context_files(context_files)
        .cwd(std::env::current_dir().unwrap())
        .with_provider_factory(Arc::new(MockFactory(provider)))
        .build_unwrap()
}

#[tokio::test]
async fn context_load_emitted_once_before_first_user_entry() {
    let canned = vec![
        vec![
            ev(StreamEventKind::TextDelta { text: "ok1".into() }),
            ev(StreamEventKind::Finish {
                reason: FinishReason::Stop,
            }),
        ],
        vec![
            ev(StreamEventKind::TextDelta { text: "ok2".into() }),
            ev(StreamEventKind::Finish {
                reason: FinishReason::Stop,
            }),
        ],
    ];
    let cfg = build_cfg(
        MockProvider::new(canned),
        vec![
            ContextFile {
                path: PathBuf::from("/tmp/AGENTS.md"),
                content: "hello world".into(),
            },
            ContextFile {
                path: PathBuf::from("/tmp/CLAUDE.md"),
                content: "another file".into(),
            },
        ],
    );
    let mgr = cfg.session_manager.clone();
    let (_rt, session) = create_agent_session(cfg, None).expect("session");
    let id = session.id().to_string();

    session.prompt("first".into()).await.expect("prompt 1");
    session.prompt("second".into()).await.expect("prompt 2");

    let branch = mgr.current_branch(&id);

    // Two ContextLoad entries — one per ContextFile.
    let context_loads: Vec<_> = branch
        .iter()
        .filter(|e| matches!(e.kind, SessionEntryKind::ContextLoad { .. }))
        .collect();
    assert_eq!(
        context_loads.len(),
        2,
        "expected exactly 2 ContextLoad entries (one per file), got {}",
        context_loads.len()
    );

    // ContextLoad sources match the configured files.
    let sources: Vec<String> = context_loads
        .iter()
        .filter_map(|e| match &e.kind {
            SessionEntryKind::ContextLoad { source, .. } => Some(source.clone()),
            _ => None,
        })
        .collect();
    assert!(sources.iter().any(|s| s.ends_with("AGENTS.md")));
    assert!(sources.iter().any(|s| s.ends_with("CLAUDE.md")));

    // tokens come from the real BPE tokenizer (RFD 0014), not the
    // RFD 0012 bytes/4 heuristic. Sanity-check the count is positive
    // and not exactly bytes/4 for at least one fixture (regression
    // guard that the real tokenizer is plumbed in).
    let mut saw_non_bytes_div_4 = false;
    for e in &context_loads {
        if let SessionEntryKind::ContextLoad { bytes, tokens, .. } = &e.kind {
            let t = tokens.expect("tokens populated");
            assert!(t > 0, "expected non-zero tokens for {bytes} bytes");
            if t != *bytes / 4 && t != (*bytes).div_ceil(4) {
                saw_non_bytes_div_4 = true;
            }
        }
    }
    // "hello world" (11 bytes) → cl100k says 2 tokens, bytes/4 = 2,
    // they match. "another file" (12 bytes) → cl100k says 2,
    // bytes/4 = 3, so this fires.
    assert!(
        saw_non_bytes_div_4,
        "at least one ContextLoad should differ from bytes/4 once the real tokenizer is plumbed in"
    );

    // Ordering: every ContextLoad sits before the first User entry.
    let first_user_idx = branch
        .iter()
        .position(|e| matches!(e.kind, SessionEntryKind::User { .. }))
        .expect("user entry");
    for (i, e) in branch.iter().enumerate() {
        if matches!(e.kind, SessionEntryKind::ContextLoad { .. }) {
            assert!(
                i < first_user_idx,
                "ContextLoad at idx {i} should come before first User at {first_user_idx}"
            );
        }
    }
}
