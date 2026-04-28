//! Extra coverage for the heuristic and LLM compactors. Specifically targets
//! the empty-history early returns, the System/Tool role labels, and the
//! ToolUse / ToolResult branches inside `build_summary`.

use async_trait::async_trait;
use pi_agent_core::compaction::LlmCompactor;
use pi_agent_core::Compactor;
use pi_ai::auth::AuthMethod;
use pi_ai::provider::EventStream;
use pi_ai::registry::ProviderConfig;
use pi_ai::stream::StreamEvent;
use pi_ai::{
    ContentBlock, FinishReason, GenerateRequest, GenerateResponse, Message, ModelInfo, Provider,
    ProviderKind, Result as AiResult, Role, ToolCall, Usage,
};
use std::sync::Arc;

#[test]
fn compactor_with_fewer_user_turns_than_keep_last_returns_empty_summary() {
    let c = Compactor::default(); // keep_last_turns = 6
    let msgs = vec![Message::user_text("only one user turn")];
    let (out, summary) = c.compact(&msgs, None);
    assert!(summary.is_empty());
    // Original message preserved verbatim, no recap prepended.
    assert_eq!(out.len(), 1);
}

#[test]
fn compactor_summary_includes_tool_use_and_tool_result_markers() {
    let c = Compactor { keep_last_turns: 0 };
    let msgs = vec![
        Message {
            role: Role::Assistant,
            content: vec![
                ContentBlock::Text {
                    text: "thinking out loud".into(),
                },
                ContentBlock::ToolUse {
                    id: "id".into(),
                    name: "read".into(),
                    input: serde_json::json!({}),
                },
            ],
        },
        Message {
            role: Role::User,
            content: vec![ContentBlock::ToolResult {
                tool_use_id: "id".into(),
                content: "ok".into(),
                is_error: false,
            }],
        },
        Message {
            role: Role::User,
            content: vec![ContentBlock::ToolResult {
                tool_use_id: "id2".into(),
                content: "fail".into(),
                is_error: true,
            }],
        },
    ];
    let (_out, summary) = c.compact(&msgs, Some("focus on tools"));
    assert!(summary.contains("[tool:read]"));
    assert!(summary.contains("[tool_ok]"));
    assert!(summary.contains("[tool_error]"));
    assert!(summary.contains("focus on tools"));
}

#[test]
fn compactor_with_system_and_tool_roles_labels_them() {
    // The recap path only fires when there is at least one user turn to
    // anchor the boundary on; otherwise compact() returns early. Add a
    // trailing user turn so build_summary actually iterates the leading
    // system/tool messages and labels them.
    let c = Compactor { keep_last_turns: 0 };
    let msgs = vec![
        Message {
            role: Role::System,
            content: vec![ContentBlock::Text { text: "sys".into() }],
        },
        Message {
            role: Role::Tool,
            content: vec![ContentBlock::Text {
                text: "tool".into(),
            }],
        },
        Message::user_text("trigger"),
    ];
    let (_, summary) = c.compact(&msgs, None);
    assert!(summary.contains("system: sys"), "got summary: {summary:?}");
    assert!(summary.contains("tool: tool"), "got summary: {summary:?}");
}

#[test]
fn compactor_skips_messages_with_only_empty_content_blocks() {
    let c = Compactor { keep_last_turns: 0 };
    let msgs = vec![
        // Empty Text block trimmed → skipped.
        Message {
            role: Role::User,
            content: vec![ContentBlock::Text { text: "   ".into() }],
        },
        // Text-bearing.
        Message {
            role: Role::User,
            content: vec![ContentBlock::Text {
                text: "kept".into(),
            }],
        },
    ];
    let (_, summary) = c.compact(&msgs, None);
    assert!(summary.contains("kept"));
    let count = summary.matches("- user").count();
    assert_eq!(
        count, 1,
        "empty-content message should not contribute a line: {summary}"
    );
}

// --- LlmCompactor early return path --------------------------------

struct UnusedProvider {
    cfg: ProviderConfig,
    auth: AuthMethod,
}

#[async_trait]
impl Provider for UnusedProvider {
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
        Ok(GenerateResponse {
            message: Message::assistant_text("should-not-be-called"),
            tool_calls: Vec::<ToolCall>::new(),
            finish_reason: FinishReason::Stop,
            usage: Usage::default(),
        })
    }
    async fn stream(&self, _req: GenerateRequest, _model: &ModelInfo) -> AiResult<EventStream> {
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
async fn llm_compactor_early_returns_when_no_compaction_needed() {
    // user_indices.len() == 1, keep_last_turns == 6 → early return without
    // calling the provider.
    let provider = UnusedProvider {
        cfg: ProviderConfig {
            name: "mock".into(),
            kind: ProviderKind::OpenAiCompat,
            base_url: "http://localhost".into(),
            auth_header: "Authorization".into(),
            auth_format: "Bearer {token}".into(),
            models: vec![fake_model()],
        },
        auth: AuthMethod::None,
    };
    let model = fake_model();
    let c = LlmCompactor {
        keep_last_turns: 6,
        provider: &provider,
        model: &model,
    };
    let msgs = vec![Message::user_text("only one")];
    let (out, summary) = c.compact(&msgs, None).await.unwrap();
    assert_eq!(out.len(), msgs.len());
    assert!(summary.is_empty());
}

#[tokio::test]
async fn llm_compactor_without_instructions_uses_default_recap_prompt() {
    // Use a recording provider that captures the system prompt.
    use std::sync::Mutex;

    struct RecordingProvider {
        cfg: ProviderConfig,
        auth: AuthMethod,
        last_system: Arc<Mutex<Option<String>>>,
    }

    #[async_trait]
    impl Provider for RecordingProvider {
        fn config(&self) -> &ProviderConfig {
            &self.cfg
        }
        fn auth(&self) -> &AuthMethod {
            &self.auth
        }
        async fn generate(
            &self,
            req: GenerateRequest,
            _model: &ModelInfo,
        ) -> AiResult<GenerateResponse> {
            *self.last_system.lock().unwrap() = req.system.clone();
            Ok(GenerateResponse {
                message: Message::assistant_text("RECAP_BODY"),
                tool_calls: Vec::<ToolCall>::new(),
                finish_reason: FinishReason::Stop,
                usage: Usage::default(),
            })
        }
        async fn stream(&self, _req: GenerateRequest, _model: &ModelInfo) -> AiResult<EventStream> {
            Ok(Box::pin(futures::stream::empty::<AiResult<StreamEvent>>()))
        }
    }

    let last = Arc::new(Mutex::new(None));
    let provider = RecordingProvider {
        cfg: ProviderConfig {
            name: "mock".into(),
            kind: ProviderKind::OpenAiCompat,
            base_url: "http://localhost".into(),
            auth_header: "Authorization".into(),
            auth_format: "Bearer {token}".into(),
            models: vec![fake_model()],
        },
        auth: AuthMethod::None,
        last_system: last.clone(),
    };
    let model = fake_model();
    let c = LlmCompactor {
        keep_last_turns: 1,
        provider: &provider,
        model: &model,
    };
    let mut msgs: Vec<Message> = Vec::new();
    for i in 0..4 {
        msgs.push(Message::user_text(format!("u{i}")));
        msgs.push(Message::assistant_text(format!("a{i}")));
    }
    let (_out, summary) = c.compact(&msgs, None).await.unwrap();
    assert_eq!(summary, "RECAP_BODY");
    let captured = last.lock().unwrap().clone().expect("system captured");
    // Default recap prompt — no "Follow these instructions" follow-on.
    assert!(captured.contains("Summarise"));
    assert!(!captured.contains("Follow these instructions"));
}
