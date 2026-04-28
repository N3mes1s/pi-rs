use async_trait::async_trait;
use pi_agent_core::{discover_context_files, Compactor, Settings};
use pi_agent_core::compaction::LlmCompactor;
use pi_ai::auth::AuthMethod;
use pi_ai::provider::EventStream;
use pi_ai::registry::ProviderConfig;
use pi_ai::stream::StreamEvent;
use pi_ai::{
    ContentBlock, GenerateRequest, GenerateResponse, Message, ModelInfo, Provider,
    ProviderKind, Result as AiResult, Role, ToolCall, FinishReason, Usage,
};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

fn user(text: &str) -> Message {
    Message::user_text(text)
}
fn assistant(text: &str) -> Message {
    Message::assistant_text(text)
}

#[test]
fn default_keeps_last_six_user_turns_and_summarises_rest() {
    let c = Compactor::default();
    // 8 user turns interleaved with assistant replies.
    let mut msgs: Vec<Message> = Vec::new();
    for i in 0..8 {
        msgs.push(user(&format!("u{i}")));
        msgs.push(assistant(&format!("a{i}")));
    }
    let (out, summary) = c.compact(&msgs, None);
    assert!(!summary.is_empty(), "should produce a summary block");
    // The first user message in `out` is the synthesized <context_recap>.
    let first_text = match &out[0].content[0] {
        ContentBlock::Text { text } => text.clone(),
        _ => panic!("expected text recap"),
    };
    assert!(first_text.starts_with("<context_recap>"));
    assert!(first_text.contains("u0"));
    assert!(first_text.contains("u1"));
    // The last 6 user turns should still be present verbatim.
    let user_texts: Vec<String> = out
        .iter()
        .filter(|m| matches!(m.role, Role::User))
        .skip(1) // skip the synthesised recap
        .map(|m| m.text())
        .collect();
    assert_eq!(user_texts, vec!["u2", "u3", "u4", "u5", "u6", "u7"]);
}

#[test]
fn keep_last_zero_keeps_no_history() {
    // Bug fix: previous code computed `user_indices[len - 0]` and panicked
    // with index OOB when keep_last_turns was 0. The compactor now treats
    // keep_last_turns == 0 as "summarise everything, keep no original
    // messages", which is the only sensible reading.
    let c = Compactor { keep_last_turns: 0 };
    let msgs = vec![user("hello"), assistant("hi")];
    let (out, summary) = c.compact(&msgs, None);
    assert!(!summary.is_empty());
    // Only the recap message remains (everything was summarised).
    assert_eq!(out.len(), 1);
    let txt = match &out[0].content[0] {
        ContentBlock::Text { text } => text.clone(),
        _ => panic!(),
    };
    assert!(txt.contains("<context_recap>"));
}

// --- mock provider for LlmCompactor ----------------------------------------

struct MockProvider {
    cfg: ProviderConfig,
    auth: AuthMethod,
    summary: String,
    calls: Arc<AtomicUsize>,
}

#[async_trait]
impl Provider for MockProvider {
    fn config(&self) -> &ProviderConfig {
        &self.cfg
    }
    fn auth(&self) -> &AuthMethod {
        &self.auth
    }
    async fn generate(
        &self,
        _req: GenerateRequest,
        _model: &ModelInfo,
    ) -> AiResult<GenerateResponse> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Ok(GenerateResponse {
            message: Message::assistant_text(self.summary.clone()),
            tool_calls: Vec::<ToolCall>::new(),
            finish_reason: FinishReason::Stop,
            usage: Usage::default(),
        })
    }
    async fn stream(
        &self,
        _req: GenerateRequest,
        _model: &ModelInfo,
    ) -> AiResult<EventStream> {
        // Not used because we override generate().
        Ok(Box::pin(futures::stream::empty::<AiResult<StreamEvent>>()))
    }
}

fn fake_model() -> ModelInfo {
    ModelInfo {
        provider: "mock".into(),
        id: "mock-1".into(),
        alias: Some("mock".into()),
        context_window: 1000,
        max_output_tokens: 100,
        supports_thinking: false,
        supports_tools: false,
        supports_vision: false,
        input_cost_per_mtok: 0.0,
        output_cost_per_mtok: 0.0,
        cache_read_cost_per_mtok: None,
        cache_write_cost_per_mtok: None,
        api_kind: Default::default(),
    }
}

#[tokio::test]
async fn llm_compactor_emits_context_recap_with_mock_provider() {
    let calls = Arc::new(AtomicUsize::new(0));
    let provider = MockProvider {
        cfg: ProviderConfig {
            name: "mock".into(),
            kind: ProviderKind::OpenAiCompat,
            base_url: "http://localhost".into(),
            auth_header: "Authorization".into(),
            auth_format: "Bearer {token}".into(),
            models: vec![fake_model()],
        },
        auth: AuthMethod::None,
        summary: "FAKE_SUMMARY".into(),
        calls: calls.clone(),
    };
    let model = fake_model();
    let c = LlmCompactor {
        keep_last_turns: 1,
        provider: &provider,
        model: &model,
    };
    let mut msgs: Vec<Message> = Vec::new();
    for i in 0..4 {
        msgs.push(user(&format!("u{i}")));
        msgs.push(assistant(&format!("a{i}")));
    }
    let (out, summary) = c.compact(&msgs, Some("focus on TODOs")).await.unwrap();
    assert_eq!(calls.load(Ordering::SeqCst), 1, "provider should be called once");
    assert_eq!(summary, "FAKE_SUMMARY");
    let first_text = match &out[0].content[0] {
        ContentBlock::Text { text } => text.clone(),
        _ => panic!("expected text recap"),
    };
    assert!(first_text.contains("<context_recap>"));
    assert!(first_text.contains("FAKE_SUMMARY"));
}

// --- settings: project overlay ----------------------------------------------

#[test]
fn settings_merge_project_overlays_global() {
    let dir = tempfile::tempdir().unwrap();
    let global = dir.path().join("settings.json");
    std::fs::write(
        &global,
        serde_json::json!({
            "provider": "anthropic",
            "model": "sonnet",
            "theme": "dark"
        })
        .to_string(),
    )
    .unwrap();
    let mut s = Settings::load(&global);
    assert_eq!(s.provider, "anthropic");
    assert_eq!(s.model, "sonnet");
    assert_eq!(s.theme, "dark");

    let proj = dir.path().join("proj.json");
    std::fs::write(
        &proj,
        serde_json::json!({"model": "opus", "theme": "light"}).to_string(),
    )
    .unwrap();
    s.merge_project(&proj);
    assert_eq!(s.provider, "anthropic", "global keys preserved");
    assert_eq!(s.model, "opus", "project overrides model");
    assert_eq!(s.theme, "light", "project overrides theme");
}

// --- context: AGENTS.md discovery -------------------------------------------

#[test]
fn discover_context_finds_agents_in_cwd_and_parents() {
    let dir = tempfile::tempdir().unwrap();
    let parent = dir.path().to_path_buf();
    let child = parent.join("sub").join("deeper");
    std::fs::create_dir_all(&child).unwrap();
    std::fs::write(parent.join("AGENTS.md"), "# parent agents").unwrap();
    std::fs::write(child.join("AGENTS.md"), "# child agents").unwrap();

    // agent_dir is empty (no global agents.md), so all hits come from
    // walking ancestors of `child`.
    let agent_dir = tempfile::tempdir().unwrap();
    let found = discover_context_files(&child, agent_dir.path(), &["AGENTS.md"]);
    assert!(found.iter().any(|f| f.content.contains("parent agents")));
    assert!(found.iter().any(|f| f.content.contains("child agents")));
}
