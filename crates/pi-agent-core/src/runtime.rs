use chrono::Utc;
use futures::StreamExt;
use pi_ai::{
    AnthropicProvider, AuthMethod, AuthStorage, AzureOpenAiProvider, BedrockAnthropicProvider,
    ContentBlock, FinishReason, GenerateRequest, GoogleProvider, Message, ModelInfo, ModelRegistry,
    OpenAiCompatProvider, OpenAiProvider, Provider, ProviderConfig, ProviderKind, Role,
    ThinkingLevel, ToolCall, ToolResult, Usage,
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
    fn build(
        &self,
        cfg: ProviderConfig,
        auth: AuthMethod,
    ) -> Result<Box<dyn Provider>, RuntimeError>;
}

/// Approval gate consulted before each tool invocation. The runtime
/// calls [`ToolGate::approve`] with the tool name + JSON-serialised
/// input; the gate may approve, reject (with a reason fed back to the
/// model), or signal that the host UI should prompt the user. The
/// runtime treats `AskUser` as `Reject` in headless modes.
///
/// Default: no gate (every call runs). pi-coding-agent's
/// `auto_approve::AutoApproveGate` plugs in here.
#[async_trait::async_trait]
pub trait ToolGate: Send + Sync {
    async fn approve(&self, tool_name: &str, input: &serde_json::Value) -> ToolGateOutcome;
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ToolGateOutcome {
    Approve,
    Reject(String),
    AskUser(String),
}

/// Action requested by a [`StreamInterceptor`] after observing one
/// streamed assistant text delta.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InterceptAction {
    /// No-op — keep streaming.
    Continue,
    /// Abort the in-flight assistant turn, append the carried text as a
    /// new synthetic user message, and re-issue the turn. Used by TTSR
    /// (Time-Travelling Streamed Rules) to inject `<system_reminder>`
    /// messages mid-stream.
    AbortAndInject(String),
}

/// Hook invoked once per streamed `TextDelta` during an assistant turn.
/// Lives in `pi-agent-core` so the run loop can call it generically;
/// `pi-coding-agent` plugs the TTSR matcher in here.
///
/// The interceptor is `&self` because the runtime calls it concurrently
/// with whatever else the host is doing — implementations should hold
/// their own interior mutability (typically a `Mutex<Matcher>`).
#[async_trait::async_trait]
pub trait StreamInterceptor: Send + Sync {
    /// Called once at the *start* of every assistant turn. Lets the
    /// interceptor reset any per-turn buffers (the TTSR matcher uses
    /// this to clear its delta accumulator without losing the
    /// `fired_rules` set).
    async fn turn_start(&self) {}

    /// Called for each streamed assistant text delta. Returning
    /// [`InterceptAction::AbortAndInject`] aborts the current stream
    /// and re-issues the turn with the carried text appended as a user
    /// message.
    async fn on_text_delta(&self, text: &str) -> InterceptAction;
}

/// Default factory: dispatch on `ProviderKind`.
pub struct DefaultProviderFactory;

impl ProviderFactory for DefaultProviderFactory {
    fn build(
        &self,
        cfg: ProviderConfig,
        auth: AuthMethod,
    ) -> Result<Box<dyn Provider>, RuntimeError> {
        Ok(match cfg.kind {
            ProviderKind::Anthropic => Box::new(AnthropicProvider::new(cfg, auth)),
            ProviderKind::OpenAi => Box::new(OpenAiProvider::new(cfg, auth)),
            ProviderKind::OpenAiCompat => Box::new(OpenAiCompatProvider::new(cfg, auth)),
            ProviderKind::Google => Box::new(GoogleProvider::new(cfg, auth)),
            ProviderKind::Bedrock => Box::new(BedrockAnthropicProvider::new(cfg, auth)),
            ProviderKind::Azure => Box::new(AzureOpenAiProvider::new(cfg, auth)),
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
    /// Optional tool gate. When present, the runtime asks it whether each
    /// tool call may proceed. `None` ⇒ no gating (legacy behaviour).
    pub tool_gate: Option<Arc<dyn ToolGate>>,
    /// Whether `AskUser` outcomes from the gate should resolve to "approve"
    /// or be treated as "reject". TUI mode flips this on (and prompts the
    /// user); headless modes leave it false (fail-closed).
    pub gate_ask_is_approve: bool,
    /// Optional stream interceptor. Called for every assistant text
    /// delta; may signal an abort + re-injection (TTSR). `None` ⇒ no
    /// interception.
    pub stream_interceptor: Option<Arc<dyn StreamInterceptor>>,
}

impl RuntimeConfig {
    /// Replace the provider factory used by this runtime. Returns `self`
    /// for chaining.
    pub fn with_provider_factory(mut self, factory: Arc<dyn ProviderFactory>) -> Self {
        self.provider_factory = Some(factory);
        self
    }

    /// Install a tool gate. `ask_is_approve = true` is appropriate when
    /// the host has interactive UI and will prompt the user; `false`
    /// (fail-closed) is the safe default for print/json/rpc modes.
    pub fn with_tool_gate(mut self, gate: Arc<dyn ToolGate>, ask_is_approve: bool) -> Self {
        self.tool_gate = Some(gate);
        self.gate_ask_is_approve = ask_is_approve;
        self
    }

    /// Install a stream interceptor (typically the TTSR matcher).
    pub fn with_stream_interceptor(mut self, interceptor: Arc<dyn StreamInterceptor>) -> Self {
        self.stream_interceptor = Some(interceptor);
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
                context_loads_emitted: false,
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
                // Reopened sessions: assume any context_files emit
                // happened on first creation. Don't double-emit.
                context_loads_emitted: true,
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
    /// One-shot guard for emitting [`SessionEntryKind::ContextLoad`]
    /// entries: the runtime walks `cfg.context_files` exactly once per
    /// session (before the first user turn) and flips this to `true`.
    context_loads_emitted: bool,
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

    /// Resolve a [`Role`] (e.g. `Role::Smol`) against the configured
    /// [`crate::settings::ModelRoles`] and switch the active model to
    /// match. The provider stays the same; only the model id changes.
    /// When the role has no override the session keeps its current
    /// model. Returns the model id we ended up on.
    pub async fn set_role(
        &self,
        role: crate::settings::Role,
        roles: &crate::settings::ModelRoles,
    ) -> String {
        let mut g = self.inner.lock().await;
        let current = g.model.clone();
        let chosen = roles.resolve(role, &current).to_string();
        // If chosen is "provider/model", split.
        if let Some((p, m)) = chosen.split_once('/') {
            g.provider = p.to_string();
            g.model = m.to_string();
        } else {
            g.model = chosen.clone();
        }
        chosen
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
        self.emit(AgentEventKind::CompactionComplete {
            summary,
            freed_tokens,
        })
        .await;
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
        // RFD 0012 Part A: emit one ContextLoad JSONL entry per
        // discovered context file, exactly once per session, before the
        // very first User entry. Downstream consumers (the trajectory
        // judge, the flamegraph, evolve) need this to know the agent
        // *had* AGENTS.md / CLAUDE.md in its system prompt.
        let emit_context_loads = {
            let mut g = self.inner.lock().await;
            if g.context_loads_emitted {
                false
            } else {
                g.context_loads_emitted = true;
                true
            }
        };
        if emit_context_loads {
            for ctx in &self.cfg.context_files {
                let bytes = ctx.content.len() as u64;
                let _ = self.cfg.session_manager.append(
                    &self.id,
                    SessionEntryKind::ContextLoad {
                        source: ctx.path.display().to_string(),
                        bytes,
                        tokens: Some(pi_ai::tokenizer::count_default(&ctx.content)),
                    },
                );
            }
        }

        let user_msg = Message::user_text(user_text);
        {
            let mut g = self.inner.lock().await;
            g.aborted = false;
            g.messages.push(user_msg.clone());
        }
        let _ = self.cfg.session_manager.append(
            &self.id,
            SessionEntryKind::User {
                message: user_msg.clone(),
            },
        );
        self.emit(AgentEventKind::UserMessage { message: user_msg })
            .await;
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

            // Notify the stream interceptor (if any) that a new turn has
            // started so it can reset per-turn buffers without losing
            // session-scoped state like fired TTSR rules.
            if let Some(intercept) = &self.cfg.stream_interceptor {
                intercept.turn_start().await;
            }

            let mut stream = match provider.stream(req, model_info).await {
                Ok(s) => s,
                Err(e) => {
                    // Surface the failure to listeners *before* unwinding —
                    // otherwise JSON-mode printers (and other UIs that
                    // wait for a terminal event) block forever on the
                    // channel because no Aborted/TurnComplete ever fires.
                    let message = e.to_string();
                    self.emit(AgentEventKind::Error {
                        message: message.clone(),
                    })
                    .await;
                    self.emit(AgentEventKind::Aborted).await;
                    return Err(RuntimeError::Provider(message));
                }
            };
            let mut assistant_text = String::new();
            let mut assistant_thinking = String::new();
            let mut tool_calls: Vec<ToolCall> = Vec::new();
            let mut usage_total = Usage::default();
            let mut finish = FinishReason::Stop;
            // If an interceptor fires, we drop the partial assistant
            // message, queue an `<system_reminder>` user message and
            // restart the outer loop from the top.
            let mut intercept_inject: Option<String> = None;

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
                        self.emit(AgentEventKind::AssistantTextDelta { text: text.clone() })
                            .await;
                        if let Some(intercept) = &self.cfg.stream_interceptor {
                            match intercept.on_text_delta(&text).await {
                                InterceptAction::Continue => {}
                                InterceptAction::AbortAndInject(reminder) => {
                                    intercept_inject = Some(reminder);
                                    break;
                                }
                            }
                        }
                    }
                    K::ThinkingDelta { text } => {
                        assistant_thinking.push_str(&text);
                        self.emit(AgentEventKind::AssistantThinkingDelta { text })
                            .await;
                    }
                    K::ToolCallComplete { id, name, input } => {
                        let call = ToolCall {
                            id: id.clone(),
                            name: name.clone(),
                            input,
                        };
                        self.emit(AgentEventKind::AssistantToolCall { call: call.clone() })
                            .await;
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
                        self.emit(AgentEventKind::Error {
                            message: message.clone(),
                        })
                        .await;
                        return Err(RuntimeError::Provider(message));
                    }
                    _ => {}
                }
            }

            // TTSR / interceptor abort: throw away any partial assistant
            // output, emit Aborted so UIs collapse the half-finished
            // bubble, and inject the carried text as a fresh user turn.
            // The outer `loop` then re-issues the assistant turn against
            // the new history.
            if let Some(reminder) = intercept_inject {
                self.emit(AgentEventKind::Aborted).await;
                let next = Message::user_text(reminder);
                {
                    let mut g = self.inner.lock().await;
                    g.messages.push(next.clone());
                }
                let _ = self.cfg.session_manager.append(
                    &self.id,
                    SessionEntryKind::User {
                        message: next.clone(),
                    },
                );
                self.emit(AgentEventKind::UserMessage { message: next })
                    .await;
                continue;
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
                SessionEntryKind::Assistant {
                    message: assistant_msg.clone(),
                },
            );
            // Persist the per-turn token / cost roll-up so trajectory
            // recorders + pi-stats ingest can attribute spend back to
            // this exact assistant turn. Skipped when the provider
            // didn't emit a non-zero Usage (e.g. transport error
            // before message_delta).
            if usage_total.input_tokens
                | usage_total.output_tokens
                | usage_total.cache_read_tokens
                | usage_total.cache_write_tokens
                | usage_total.reasoning_tokens
                != 0
                || usage_total.cost_usd > 0.0
            {
                let _ = self.cfg.session_manager.append(
                    &self.id,
                    SessionEntryKind::Usage {
                        usage: usage_total.clone(),
                    },
                );
            }
            self.emit(AgentEventKind::AssistantMessage {
                message: assistant_msg.clone(),
            })
            .await;
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
                        SessionEntryKind::User {
                            message: next.clone(),
                        },
                    );
                    self.emit(AgentEventKind::UserMessage { message: next })
                        .await;
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
                            SessionEntryKind::ToolResult {
                                result: result.clone(),
                            },
                        );
                        self.emit(AgentEventKind::ToolResult {
                            result: result.clone(),
                        })
                        .await;
                        results_block.push(ContentBlock::ToolResult {
                            tool_use_id: call.id,
                            content: result.model_output,
                            is_error: true,
                        });
                        continue;
                    }
                };
                // Gate check: ask the configured ToolGate (if any) whether
                // this call may proceed. Reject → synthesise an error
                // ToolResult and skip the actual invoke; AskUser → fail
                // closed unless gate_ask_is_approve is set.
                if let Some(gate) = &self.cfg.tool_gate {
                    let outcome = gate.approve(&call.name, &call.input).await;
                    let blocked = match outcome {
                        ToolGateOutcome::Approve => None,
                        ToolGateOutcome::Reject(reason) => Some(reason),
                        ToolGateOutcome::AskUser(reason) => {
                            if self.cfg.gate_ask_is_approve {
                                None
                            } else {
                                Some(format!("user-confirmation required (headless): {reason}"))
                            }
                        }
                    };
                    if let Some(reason) = blocked {
                        let result = ToolResult {
                            tool_use_id: call.id.clone(),
                            model_output: format!("AUTO-APPROVE BLOCKED: {reason}"),
                            display: None,
                            is_error: true,
                        };
                        let _ = self.cfg.session_manager.append(
                            &self.id,
                            SessionEntryKind::ToolResult {
                                result: result.clone(),
                            },
                        );
                        self.emit(AgentEventKind::ToolResult {
                            result: result.clone(),
                        })
                        .await;
                        results_block.push(ContentBlock::ToolResult {
                            tool_use_id: call.id,
                            content: result.model_output,
                            is_error: true,
                        });
                        continue;
                    }
                }
                match tool.invoke(&tool_ctx, &call.id, call.input.clone()).await {
                    Ok(result) => {
                        let _ = self.cfg.session_manager.append(
                            &self.id,
                            SessionEntryKind::ToolResult {
                                result: result.clone(),
                            },
                        );
                        self.emit(AgentEventKind::ToolResult {
                            result: result.clone(),
                        })
                        .await;
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
                            SessionEntryKind::ToolResult {
                                result: result.clone(),
                            },
                        );
                        self.emit(AgentEventKind::ToolResult {
                            result: result.clone(),
                        })
                        .await;
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
            self.emit(AgentEventKind::CompactionStart { instructions: None })
                .await;
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
