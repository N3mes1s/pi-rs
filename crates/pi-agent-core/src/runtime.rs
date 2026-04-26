use chrono::Utc;
use futures::StreamExt;
use pi_ai::{
    AnthropicProvider, AuthMethod, AuthStorage, ContentBlock, FinishReason, GenerateRequest,
    Message, ModelInfo, ModelRegistry, OpenAiCompatProvider, OpenAiProvider, Provider,
    ProviderConfig, ProviderKind, Role, ThinkingLevel, ToolCall, ToolResult, Usage,
};
use pi_tools::{ToolContext, ToolRegistry};
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::Mutex;

use crate::context::ContextFile;
use crate::event::{AgentEvent, AgentEventKind, EventSender};
use crate::session::{SessionEntryKind, SessionManager};
use crate::settings::Settings;

/// Pluggable provider builder. The default implementation matches on
/// `ProviderKind` and returns one of the built-in `pi_ai` providers.
/// Tests can swap this out to inject mock providers without going over
/// the network.
pub trait ProviderFactory: Send + Sync {
    fn build(&self, cfg: ProviderConfig, auth: AuthMethod) -> Result<Box<dyn Provider>, RuntimeError>;
}

/// Default factory: dispatch on `ProviderKind`.
pub struct DefaultProviderFactory;

impl ProviderFactory for DefaultProviderFactory {
    fn build(&self, cfg: ProviderConfig, auth: AuthMethod) -> Result<Box<dyn Provider>, RuntimeError> {
        Ok(match cfg.kind {
            ProviderKind::Anthropic => Box::new(AnthropicProvider::new(cfg, auth)),
            ProviderKind::OpenAi => Box::new(OpenAiProvider::new(cfg, auth)),
            ProviderKind::OpenAiCompat => Box::new(OpenAiCompatProvider::new(cfg, auth)),
        })
    }
}

#[derive(Clone)]
pub struct RuntimeConfig {
    pub session_manager: SessionManager,
    pub auth_storage: AuthStorage,
    pub model_registry: ModelRegistry,
    pub tools: ToolRegistry,
    pub settings: Settings,
    pub system_prompt: String,
    pub context_files: Vec<ContextFile>,
    pub cwd: PathBuf,
    /// Optional override for provider construction. When `None`, the runtime
    /// uses [`DefaultProviderFactory`].
    pub provider_factory: Option<Arc<dyn ProviderFactory>>,
}

impl RuntimeConfig {
    /// Replace the provider factory used by this runtime. Returns `self`
    /// for chaining.
    pub fn with_provider_factory(mut self, factory: Arc<dyn ProviderFactory>) -> Self {
        self.provider_factory = Some(factory);
        self
    }
}

/// `AgentSessionRuntime` mirrors `createAgentSessionRuntime` in upstream pi:
/// owns the registries, settings and tool list shared across sessions.
pub struct AgentSessionRuntime {
    config: Arc<RuntimeConfig>,
}

impl AgentSessionRuntime {
    pub fn new(config: RuntimeConfig) -> Self {
        Self {
            config: Arc::new(config),
        }
    }

    pub fn config(&self) -> &RuntimeConfig {
        &self.config
    }

    pub fn create_session(&self, sender: Option<EventSender>) -> std::io::Result<AgentSession> {
        let cfg = self.config.clone();
        let meta = cfg
            .session_manager
            .create(&cfg.settings.provider, &cfg.settings.model)?;
        Ok(AgentSession {
            id: meta.id,
            inner: Arc::new(Mutex::new(AgentSessionInner {
                sender,
                aborted: false,
                queued_messages: Vec::new(),
                messages: Vec::new(),
                provider: cfg.settings.provider.clone(),
                model: cfg.settings.model.clone(),
                thinking: cfg.settings.thinking.into(),
                tools: cfg.tools.clone(),
            })),
            cfg,
        })
    }

    pub fn open_session(
        &self,
        id_or_path: &str,
        sender: Option<EventSender>,
    ) -> std::io::Result<AgentSession> {
        let cfg = self.config.clone();
        let meta = cfg.session_manager.open_existing(id_or_path)?;
        let history = cfg.session_manager.current_branch(&meta.id);
        let mut messages: Vec<Message> = Vec::new();
        let mut pending_tool_results: Vec<ContentBlock> = Vec::new();
        for entry in history {
            match entry.kind {
                SessionEntryKind::User { message } => messages.push(message),
                SessionEntryKind::Assistant { message } => messages.push(message),
                SessionEntryKind::ToolResult { result } => {
                    pending_tool_results.push(ContentBlock::ToolResult {
                        tool_use_id: result.tool_use_id,
                        content: result.model_output,
                        is_error: result.is_error,
                    });
                }
                _ => {}
            }
        }
        if !pending_tool_results.is_empty() {
            messages.push(Message {
                role: Role::User,
                content: pending_tool_results,
            });
        }
        Ok(AgentSession {
            id: meta.id,
            inner: Arc::new(Mutex::new(AgentSessionInner {
                sender,
                aborted: false,
                queued_messages: Vec::new(),
                messages,
                provider: meta.provider,
                model: meta.model,
                thinking: cfg.settings.thinking.into(),
                tools: cfg.tools.clone(),
            })),
            cfg,
        })
    }
}

/// One conversation thread.
#[derive(Clone)]
pub struct AgentSession {
    pub id: String,
    inner: Arc<Mutex<AgentSessionInner>>,
    cfg: Arc<RuntimeConfig>,
}

struct AgentSessionInner {
    sender: Option<EventSender>,
    aborted: bool,
    queued_messages: Vec<String>,
    messages: Vec<Message>,
    provider: String,
    model: String,
    thinking: ThinkingLevel,
    tools: ToolRegistry,
}

impl AgentSession {
    pub fn id(&self) -> &str {
        &self.id
    }

    pub async fn set_sender(&self, sender: Option<EventSender>) {
        self.inner.lock().await.sender = sender;
    }

    pub async fn set_model(&self, provider: String, model: String) {
        let mut g = self.inner.lock().await;
        g.provider = provider;
        g.model = model;
    }

    pub async fn set_thinking(&self, t: ThinkingLevel) {
        self.inner.lock().await.thinking = t;
    }

    pub async fn enqueue(&self, msg: String) {
        self.inner.lock().await.queued_messages.push(msg);
    }

    pub async fn abort(&self) {
        self.inner.lock().await.aborted = true;
    }

    pub async fn messages(&self) -> Vec<Message> {
        self.inner.lock().await.messages.clone()
    }

    pub async fn compact(&self, instructions: Option<String>) {
        self.compact_inner(instructions, false).await
    }

    /// Compact using the active provider+model (LLM-driven). Falls back to
    /// the heuristic compactor on provider error.
    pub async fn compact_with_llm(&self, instructions: Option<String>) {
        self.compact_inner(instructions, true).await
    }

    async fn compact_inner(&self, instructions: Option<String>, prefer_llm: bool) {
        let messages = self.inner.lock().await.messages.clone();
        let original_len = messages.len();
        let (new_msgs, summary) = if prefer_llm {
            self.try_llm_compact(&messages, instructions.as_deref())
                .await
                .unwrap_or_else(|| {
                    let comp = crate::compaction::Compactor::default();
                    comp.compact(&messages, instructions.as_deref())
                })
        } else {
            let comp = crate::compaction::Compactor::default();
            comp.compact(&messages, instructions.as_deref())
        };
        let mut g = self.inner.lock().await;
        g.messages = new_msgs;
        let freed_tokens = (original_len - g.messages.len()) as u64 * 200;
        drop(g);
        let _ = self.cfg.session_manager.append(
            &self.id,
            SessionEntryKind::Compaction {
                summary: summary.clone(),
                replaced_ids: vec![],
            },
        );
        self.emit(AgentEventKind::CompactionComplete { summary, freed_tokens }).await;
    }

    async fn try_llm_compact(
        &self,
        messages: &[Message],
        instructions: Option<&str>,
    ) -> Option<(Vec<Message>, String)> {
        let (provider_name, model_name) = {
            let g = self.inner.lock().await;
            (g.provider.clone(), g.model.clone())
        };
        let (provider_cfg, model_info) = self
            .cfg
            .model_registry
            .resolve(&format!("{}/{}", provider_name, model_name))
            .or_else(|| self.cfg.model_registry.resolve(&model_name))?;
        let auth = self
            .cfg
            .auth_storage
            .get(&provider_cfg.name)
            .unwrap_or(AuthMethod::None);
        let provider = build_provider(&self.cfg, provider_cfg.clone(), auth).ok()?;
        let comp = crate::compaction::LlmCompactor {
            keep_last_turns: 6,
            provider: provider.as_ref(),
            model: model_info,
        };
        comp.compact(messages, instructions).await.ok()
    }

    pub async fn prompt(&self, user_text: String) -> Result<Message, RuntimeError> {
        let user_msg = Message::user_text(user_text);
        {
            let mut g = self.inner.lock().await;
            g.aborted = false;
            g.messages.push(user_msg.clone());
        }
        let _ = self
            .cfg
            .session_manager
            .append(&self.id, SessionEntryKind::User { message: user_msg.clone() });
        self.emit(AgentEventKind::UserMessage { message: user_msg }).await;
        self.run_loop().await
    }

    async fn run_loop(&self) -> Result<Message, RuntimeError> {
        let mut last_assistant: Option<Message>;
        loop {
            if self.inner.lock().await.aborted {
                self.emit(AgentEventKind::Aborted).await;
                return Err(RuntimeError::Aborted);
            }

            let (provider_name, model_name, thinking, messages, tools) = {
                let g = self.inner.lock().await;
                (
                    g.provider.clone(),
                    g.model.clone(),
                    g.thinking,
                    g.messages.clone(),
                    g.tools.clone(),
                )
            };

            let (provider_cfg, model_info) = self
                .cfg
                .model_registry
                .resolve(&format!("{}/{}", provider_name, model_name))
                .or_else(|| self.cfg.model_registry.resolve(&model_name))
                .ok_or_else(|| RuntimeError::UnknownModel(model_name.clone()))?;

            let auth = self
                .cfg
                .auth_storage
                .get(&provider_cfg.name)
                .unwrap_or(AuthMethod::None);
            let provider = build_provider(&self.cfg, provider_cfg.clone(), auth)?;

            let mut system = self.cfg.system_prompt.clone();
            for ctx in &self.cfg.context_files {
                system.push_str(&format!(
                    "\n\n<context source=\"{}\">\n{}\n</context>",
                    ctx.path.display(),
                    ctx.content
                ));
            }

            let req = GenerateRequest {
                model: model_info.id.clone(),
                system: Some(system),
                messages,
                tools: tools.specs(),
                thinking,
                temperature: None,
                max_output_tokens: Some(model_info.max_output_tokens),
                extras: serde_json::Value::Null,
            };

            self.emit(AgentEventKind::AssistantStart).await;

            let mut stream = provider.stream(req, model_info).await.map_err(|e| RuntimeError::Provider(e.to_string()))?;
            let mut assistant_text = String::new();
            let mut assistant_thinking = String::new();
            let mut tool_calls: Vec<ToolCall> = Vec::new();
            let mut usage_total = Usage::default();
            let mut finish = FinishReason::Stop;

            while let Some(ev) = stream.next().await {
                if self.inner.lock().await.aborted {
                    self.emit(AgentEventKind::Aborted).await;
                    return Err(RuntimeError::Aborted);
                }
                let ev = ev.map_err(|e| RuntimeError::Provider(e.to_string()))?;
                use pi_ai::StreamEventKind as K;
                match ev.kind {
                    K::TextDelta { text } => {
                        assistant_text.push_str(&text);
                        self.emit(AgentEventKind::AssistantTextDelta { text }).await;
                    }
                    K::ThinkingDelta { text } => {
                        assistant_thinking.push_str(&text);
                        self.emit(AgentEventKind::AssistantThinkingDelta { text }).await;
                    }
                    K::ToolCallComplete { id, name, input } => {
                        let call = ToolCall { id: id.clone(), name: name.clone(), input };
                        self.emit(AgentEventKind::AssistantToolCall { call: call.clone() }).await;
                        tool_calls.push(call);
                    }
                    K::Usage { usage } => {
                        usage_total = usage.clone();
                        self.emit(AgentEventKind::Usage { usage }).await;
                    }
                    K::Finish { reason } => {
                        finish = reason;
                    }
                    K::Error { message } => {
                        self.emit(AgentEventKind::Error { message: message.clone() }).await;
                        return Err(RuntimeError::Provider(message));
                    }
                    _ => {}
                }
            }

            let mut assistant_blocks: Vec<ContentBlock> = Vec::new();
            if !assistant_thinking.is_empty() {
                assistant_blocks.push(ContentBlock::Thinking {
                    text: assistant_thinking,
                    signature: None,
                });
            }
            if !assistant_text.is_empty() {
                assistant_blocks.push(ContentBlock::Text {
                    text: assistant_text.clone(),
                });
            }
            for c in &tool_calls {
                assistant_blocks.push(ContentBlock::ToolUse {
                    id: c.id.clone(),
                    name: c.name.clone(),
                    input: c.input.clone(),
                });
            }
            let assistant_msg = Message {
                role: Role::Assistant,
                content: assistant_blocks,
            };
            {
                let mut g = self.inner.lock().await;
                g.messages.push(assistant_msg.clone());
            }
            let _ = self.cfg.session_manager.append(
                &self.id,
                SessionEntryKind::Assistant { message: assistant_msg.clone() },
            );
            self.emit(AgentEventKind::AssistantMessage { message: assistant_msg.clone() }).await;
            last_assistant = Some(assistant_msg);

            if tool_calls.is_empty() {
                // drain queued steering messages — convert into next user turn
                let queued: Vec<String> = {
                    let mut g = self.inner.lock().await;
                    std::mem::take(&mut g.queued_messages)
                };
                if !queued.is_empty() {
                    let next = Message::user_text(queued.join("\n\n"));
                    {
                        let mut g = self.inner.lock().await;
                        g.messages.push(next.clone());
                    }
                    let _ = self.cfg.session_manager.append(
                        &self.id,
                        SessionEntryKind::User { message: next.clone() },
                    );
                    self.emit(AgentEventKind::UserMessage { message: next }).await;
                    continue;
                }
                self.emit(AgentEventKind::TurnComplete).await;
                self.maybe_auto_compact(&usage_total, model_info).await;
                let _ = finish;
                return Ok(last_assistant.unwrap());
            }

            // Execute tool calls sequentially.
            let cwd = self.cfg.cwd.clone();
            let tool_ctx = ToolContext {
                cwd,
                max_output_bytes: 256 * 1024,
            };
            let mut results_block = Vec::new();
            for call in tool_calls {
                let tool = match self.inner.lock().await.tools.get(&call.name) {
                    Some(t) => t,
                    None => {
                        let result = ToolResult {
                            tool_use_id: call.id.clone(),
                            model_output: format!("ERROR: unknown tool `{}`", call.name),
                            display: None,
                            is_error: true,
                        };
                        let _ = self.cfg.session_manager.append(
                            &self.id,
                            SessionEntryKind::ToolResult { result: result.clone() },
                        );
                        self.emit(AgentEventKind::ToolResult { result: result.clone() }).await;
                        results_block.push(ContentBlock::ToolResult {
                            tool_use_id: call.id,
                            content: result.model_output,
                            is_error: true,
                        });
                        continue;
                    }
                };
                match tool.invoke(&tool_ctx, &call.id, call.input.clone()).await {
                    Ok(result) => {
                        let _ = self.cfg.session_manager.append(
                            &self.id,
                            SessionEntryKind::ToolResult { result: result.clone() },
                        );
                        self.emit(AgentEventKind::ToolResult { result: result.clone() }).await;
                        results_block.push(ContentBlock::ToolResult {
                            tool_use_id: call.id,
                            content: result.model_output,
                            is_error: result.is_error,
                        });
                    }
                    Err(e) => {
                        let result = ToolResult {
                            tool_use_id: call.id.clone(),
                            model_output: format!("ERROR: {}", e),
                            display: None,
                            is_error: true,
                        };
                        let _ = self.cfg.session_manager.append(
                            &self.id,
                            SessionEntryKind::ToolResult { result: result.clone() },
                        );
                        self.emit(AgentEventKind::ToolResult { result: result.clone() }).await;
                        results_block.push(ContentBlock::ToolResult {
                            tool_use_id: call.id,
                            content: result.model_output,
                            is_error: true,
                        });
                    }
                }
            }
            // Feed results back as a user message.
            let user_msg = Message {
                role: Role::User,
                content: results_block,
            };
            {
                let mut g = self.inner.lock().await;
                g.messages.push(user_msg.clone());
            }
        }
    }

    async fn maybe_auto_compact(&self, usage: &Usage, model: &ModelInfo) {
        let used = usage.input_tokens + usage.output_tokens;
        let remaining = model.context_window.saturating_sub(used as u32);
        let threshold = (model.context_window as f32 * self.cfg.settings.compact_threshold) as u32;
        if remaining < threshold && remaining > 0 {
            self.emit(AgentEventKind::CompactionStart { instructions: None }).await;
            self.compact(None).await;
        }
    }

    async fn emit(&self, kind: AgentEventKind) {
        let sender = self.inner.lock().await.sender.clone();
        if let Some(s) = sender {
            let _ = s.send(AgentEvent {
                session_id: self.id.clone(),
                entry_id: String::new(),
                timestamp: Utc::now().timestamp_millis(),
                kind,
            });
        }
    }
}

fn build_provider(
    cfg: &RuntimeConfig,
    provider_cfg: ProviderConfig,
    auth: AuthMethod,
) -> Result<Box<dyn Provider>, RuntimeError> {
    if let Some(factory) = &cfg.provider_factory {
        return factory.build(provider_cfg, auth);
    }
    DefaultProviderFactory.build(provider_cfg, auth)
}

#[derive(Debug, thiserror::Error)]
pub enum RuntimeError {
    #[error("aborted")]
    Aborted,
    #[error("unknown model: {0}")]
    UnknownModel(String),
    #[error("unsupported: {0}")]
    Unsupported(String),
    #[error("provider error: {0}")]
    Provider(String),
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
}

/// Convenience wrapper matching `createAgentSession` in upstream pi.
pub fn create_agent_session(
    config: RuntimeConfig,
    sender: Option<EventSender>,
) -> std::io::Result<(AgentSessionRuntime, AgentSession)> {
    let runtime = AgentSessionRuntime::new(config);
    let session = runtime.create_session(sender)?;
    Ok((runtime, session))
}
