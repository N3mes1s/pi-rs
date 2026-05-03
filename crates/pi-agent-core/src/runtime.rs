use chrono::Utc;
use futures::{FutureExt, StreamExt};
use std::panic::AssertUnwindSafe;
use pi_ai::{
    AnthropicProvider, AuthMethod, AuthStorage, AzureOpenAiProvider, BedrockAnthropicProvider,
    ContentBlock, FinishReason, GenerateRequest, GoogleProvider, Message, ModelInfo, ModelRegistry,
    OpenAiCompatProvider, OpenAiProvider, Provider, ProviderConfig, ProviderKind, Role,
    ThinkingLevel, ToolCall, ToolResult, Usage,
};
use pi_sandbox::SandboxProvider;
use pi_tools::{ToolContext, ToolRegistry};
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::Mutex;

use crate::context::ContextFile;
use crate::event::{AgentEvent, AgentEventKind, EventSender};
use crate::router::{
    EmbeddingRouter, ForceOverride, RouteMode, Router as _, RoutingContext, StaticRouter, ToolSpec,
};
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

/// Per-call context handed to [`ToolGate::approve`]. Per RFD 0027
/// §4.5 #4 (Hardening H3): the gate must be able to scope its
/// approvals (e.g. "I approved this tool for session A; reject if a
/// subagent in session B tries the same"). Pre-H3 the gate had no
/// way to distinguish, so a "remember-the-answer" gate was
/// trivially bypassable cross-session.
///
/// `#[non_exhaustive]` per RFD §3 — additive fields are MINOR.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct GateContext {
    /// Session id of the call site. Stable for the lifetime of the
    /// session. Same value as `AgentSession::id()`.
    pub session_id: String,
    /// Zero-based count of completed turns in this session. Helps
    /// gates implement "approve once per turn" policies.
    pub turn_index: u32,
    /// If this session was spawned as a child of another (via
    /// pi-coding-agent's `task` tool, RFD 0005), the parent's
    /// session id. `None` for top-level sessions.
    pub parent_session: Option<String>,
    /// Re-entry depth — bumped each time `Tool::invoke` recursively
    /// calls back into `AgentSession::send`. `0` for the top-level
    /// turn. Future Hardening §4.5 #3 (Commit H2) wires the cap on
    /// this; for now it is informational.
    pub recursion_depth: usize,
}

impl GateContext {
    /// Construct a top-level (depth 0, no parent) gate context for a
    /// given session_id and turn.
    ///
    /// Per code-review finding #3 (pass-2): this is the canonical
    /// constructor for embedders writing `ToolGate` impls outside the
    /// pi-rs workspace. `#[non_exhaustive]` blocks struct-literal
    /// construction from external crates, so without this helper
    /// embedders writing tests for their own `approve()` impl have
    /// no way to build a `GateContext`.
    pub fn top_level(session_id: impl Into<String>, turn_index: u32) -> Self {
        Self {
            session_id: session_id.into(),
            turn_index,
            parent_session: None,
            recursion_depth: 0,
        }
    }
}

#[cfg(test)]
mod gate_context_tests {
    use super::*;

    #[test]
    fn top_level_round_trips_session_id_and_turn() {
        let ctx = GateContext::top_level("sess-abc", 7);
        assert_eq!(ctx.session_id, "sess-abc");
        assert_eq!(ctx.turn_index, 7);
        assert!(ctx.parent_session.is_none());
        assert_eq!(ctx.recursion_depth, 0);
    }

    #[test]
    fn top_level_accepts_owned_string() {
        let s: String = "sess-owned".into();
        let ctx = GateContext::top_level(s, 0);
        assert_eq!(ctx.session_id, "sess-owned");
    }
}

#[cfg(test)]
mod config_builder_cwd_tests {
    //! Per code-review pass-1 finding #9: ConfigBuilder.cwd() is no
    //! longer mandatory; build() defaults to std::env::current_dir().
    //! Most embedders just want "the current working directory" and
    //! the explicit setter was forcing them to copy-paste a fallback.

    use super::*;
    use crate::session::SessionManager;
    use crate::settings::Settings;
    use pi_ai::{AuthStorage, ModelRegistry};
    use pi_tools::ToolRegistry;

    fn minimal_builder() -> ConfigBuilder {
        let auth = AuthStorage::in_memory();
        RuntimeConfig::builder()
            .session_manager(SessionManager::in_memory())
            .auth_storage(auth.clone())
            .model_registry(ModelRegistry::new(auth))
            .tools(ToolRegistry::new())
            .settings(Settings::default())
            .system_prompt("test")
    }

    #[test]
    fn build_without_explicit_cwd_defaults_to_current_dir() {
        let cfg = minimal_builder().build().expect("should build with default cwd");
        let expected = std::env::current_dir().unwrap();
        assert_eq!(cfg.cwd, expected);
    }

    #[test]
    fn explicit_cwd_overrides_default() {
        let custom = std::path::PathBuf::from("/tmp/explicit-cwd");
        let cfg = minimal_builder()
            .cwd(custom.clone())
            .build()
            .expect("explicit cwd should still work");
        assert_eq!(cfg.cwd, custom);
    }

    #[test]
    fn cwd_from_env_helper_matches_current_dir() {
        let cfg = minimal_builder()
            .cwd_from_env()
            .build()
            .expect("cwd_from_env should produce a valid cwd");
        assert_eq!(cfg.cwd, std::env::current_dir().unwrap());
    }

    #[test]
    fn runtime_config_with_max_helpers_chain_off_built_config() {
        // Per polish-6: embedders constructing via `quick_start` (or
        // any other path that returns an already-built RuntimeConfig)
        // should be able to bump the H2 caps without going back through
        // the builder.
        let cfg = minimal_builder()
            .build()
            .expect("base builder should succeed")
            .with_max_session_tokens(50_000)
            .with_max_tool_invocations_per_turn(10)
            .with_max_recursion(2);
        assert_eq!(cfg.max_session_tokens, 50_000);
        assert_eq!(cfg.max_tool_invocations_per_turn, 10);
        assert_eq!(cfg.max_recursion, 2);
    }
}

/// Approval gate consulted before each tool invocation. The runtime
/// calls [`ToolGate::approve`] with the tool name + JSON-serialised
/// input + a [`GateContext`] carrying session/turn scope; the gate
/// may approve, reject (with a reason fed back to the model), or
/// signal that the host UI should prompt the user. The runtime treats
/// `AskUser` as `Reject` in headless modes (per RFD 0027 §4.5 #4
/// `gate_ask_is_approve = true` is TUI-only — see
/// `RuntimeConfig::with_tool_gate` doc).
///
/// Default: no gate (every call runs). pi-coding-agent's
/// `auto_approve::AutoApproveGate` plugs in here.
#[async_trait::async_trait]
pub trait ToolGate: Send + Sync {
    async fn approve(
        &self,
        ctx: &GateContext,
        tool_name: &str,
        input: &serde_json::Value,
    ) -> ToolGateOutcome;
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

/// Per RFD 0027 §4: marked `#[non_exhaustive]` so MINOR-additive field
/// growth is non-breaking. External crates (i.e. anything outside
/// `pi-agent-core` itself) cannot construct this via struct literal;
/// use [`RuntimeConfig::builder()`] instead.
#[derive(Clone)]
#[non_exhaustive]
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
    /// Optional sandbox provider (RFD 0022). When `Some`, every approved
    /// tool decision is dispatched through the sandbox boundary instead
    /// of the inline `tool.invoke()` path. When `None` the legacy inline
    /// invocation applies. The `ToolGate` still runs first regardless, so
    /// rejected calls never reach the sandbox.
    pub sandbox_provider: Option<Arc<dyn SandboxProvider>>,
    /// Per-session budget guards (Hardening §4.5 #3, RFD 0027 H2).
    /// `max_session_tokens` caps the total input + output token spend
    /// across all turns of one session; exceeding emits
    /// `RuntimeError::BudgetExhausted`. Default: 10_000_000 (10M).
    pub max_session_tokens: u64,
    /// Per-turn cap on the number of tool invocations. A model that
    /// emits an unbounded sequence of tool calls in one turn is
    /// truncated; emits `RuntimeError::InvocationCapExceeded`.
    /// Default: 64.
    pub max_tool_invocations_per_turn: usize,
    /// Reserved for tool-recursion enforcement (custom Tool::invoke
    /// re-entering AgentSession::send). Carried in `RuntimeConfig`
    /// today; H2.5 wires the actual depth check via a thread-local
    /// counter once the call sites that re-enter are audited.
    /// Default: 8.
    pub max_recursion: usize,
}

impl RuntimeConfig {
    /// Begin constructing a `RuntimeConfig` via the fluent builder.
    /// See [`ConfigBuilder`] for the full set of setters.
    ///
    /// Per RFD 0027 §4, this is the canonical construction path for
    /// embedders. Internal pi-rs callers may still use struct literals
    /// (allowed because they are inside the same crate).
    pub fn builder() -> ConfigBuilder {
        ConfigBuilder::default()
    }

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

    /// Install a sandbox provider (RFD 0022). All approved tool decisions
    /// will be dispatched through the provider's `execute_tool()` instead
    /// of running inline.
    pub fn with_sandbox_provider(mut self, provider: Arc<dyn SandboxProvider>) -> Self {
        self.sandbox_provider = Some(provider);
        self
    }

    /// Set the per-session token cap (Hardening §4.5 #3, H2). Returns
    /// `self` for chaining. Mirrors `ConfigBuilder::with_max_session_tokens`
    /// so embedders that already have a constructed `RuntimeConfig`
    /// (e.g. from `quick_start`) can adjust the cap without re-running
    /// the builder. `n = 0` disables the cap.
    pub fn with_max_session_tokens(mut self, n: u64) -> Self {
        self.max_session_tokens = n;
        self
    }

    /// Set the per-turn tool-invocation cap (Hardening §4.5 #3, H2).
    /// Returns `self` for chaining. Mirrors the builder.
    pub fn with_max_tool_invocations_per_turn(mut self, n: usize) -> Self {
        self.max_tool_invocations_per_turn = n;
        self
    }

    /// Set the tool-recursion depth cap (Hardening §4.5 #3, H2 reserved).
    /// Returns `self` for chaining. Enforcement lands in H2.5.
    pub fn with_max_recursion(mut self, n: usize) -> Self {
        self.max_recursion = n;
        self
    }
}

/// Builder for [`RuntimeConfig`]. Per RFD 0027 §4, this is the canonical
/// construction path for embedders pinning `pi-sdk`. Required setters
/// (no `with_` prefix) must be called before [`build`](Self::build);
/// optional plug-ins use the `with_*` prefix.
///
/// The [`build`](Self::build) method returns `Err(ConfigError::Missing { field })`
/// if any required setter was skipped. [`build_unwrap`](Self::build_unwrap)
/// panics on the same condition — meant for tests / quick-start where a
/// missing field is a programmer error, not an embedder error.
pub struct ConfigBuilder {
    session_manager: Option<SessionManager>,
    auth_storage: Option<AuthStorage>,
    model_registry: Option<ModelRegistry>,
    tools: Option<ToolRegistry>,
    settings: Option<Settings>,
    system_prompt: Option<String>,
    cwd: Option<PathBuf>,
    context_files: Vec<ContextFile>,
    provider_factory: Option<Arc<dyn ProviderFactory>>,
    tool_gate: Option<Arc<dyn ToolGate>>,
    gate_ask_is_approve: bool,
    stream_interceptor: Option<Arc<dyn StreamInterceptor>>,
    sandbox_provider: Option<Arc<dyn SandboxProvider>>,
    max_session_tokens: u64,
    max_tool_invocations_per_turn: usize,
    max_recursion: usize,
}

impl Default for ConfigBuilder {
    fn default() -> Self {
        Self {
            session_manager: None,
            auth_storage: None,
            model_registry: None,
            tools: None,
            settings: None,
            system_prompt: None,
            cwd: None,
            context_files: Vec::new(),
            provider_factory: None,
            tool_gate: None,
            gate_ask_is_approve: false,
            stream_interceptor: None,
            sandbox_provider: None,
            // Per RFD 0027 §4.5 #3 (Hardening H2) defaults:
            max_session_tokens: 10_000_000,
            max_tool_invocations_per_turn: 64,
            max_recursion: 8,
        }
    }
}

/// Error returned by [`ConfigBuilder::build`] when a required field
/// was not set before `build()` was called.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum ConfigError {
    #[error("required RuntimeConfig field `{field}` was not set on the builder")]
    Missing { field: &'static str },
}

impl ConfigBuilder {
    pub fn new() -> Self {
        Self::default()
    }

    // ─── Required setters ──────────────────────────────────────
    pub fn session_manager(mut self, m: SessionManager) -> Self {
        self.session_manager = Some(m);
        self
    }
    pub fn auth_storage(mut self, a: AuthStorage) -> Self {
        self.auth_storage = Some(a);
        self
    }
    pub fn model_registry(mut self, r: ModelRegistry) -> Self {
        self.model_registry = Some(r);
        self
    }
    pub fn tools(mut self, t: ToolRegistry) -> Self {
        self.tools = Some(t);
        self
    }
    pub fn settings(mut self, s: Settings) -> Self {
        self.settings = Some(s);
        self
    }
    pub fn system_prompt<S: Into<String>>(mut self, p: S) -> Self {
        self.system_prompt = Some(p.into());
        self
    }
    pub fn cwd(mut self, p: PathBuf) -> Self {
        self.cwd = Some(p);
        self
    }

    /// Set `cwd` to `std::env::current_dir()`. Per code-review pass-1
    /// finding #9: every embedder ends up writing
    /// `std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."))`
    /// — ship the helper. Falls back to `.` if `current_dir()` errors
    /// (e.g. the cwd was unlinked).
    pub fn cwd_from_env(self) -> Self {
        self.cwd(std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")))
    }

    // ─── Optional plug-ins ─────────────────────────────────────
    pub fn with_context_files(mut self, c: Vec<ContextFile>) -> Self {
        self.context_files = c;
        self
    }
    pub fn with_provider_factory(mut self, f: Arc<dyn ProviderFactory>) -> Self {
        self.provider_factory = Some(f);
        self
    }
    pub fn with_tool_gate(mut self, g: Arc<dyn ToolGate>, ask_is_approve: bool) -> Self {
        self.tool_gate = Some(g);
        self.gate_ask_is_approve = ask_is_approve;
        self
    }
    pub fn with_stream_interceptor(mut self, i: Arc<dyn StreamInterceptor>) -> Self {
        self.stream_interceptor = Some(i);
        self
    }
    pub fn with_sandbox_provider(mut self, s: Arc<dyn SandboxProvider>) -> Self {
        self.sandbox_provider = Some(s);
        self
    }

    /// Set the per-session token cap (Hardening §4.5 #3, H2). Default
    /// 10M. Embedders should size this per-tenant; the default catches
    /// pathological model loops without blocking normal sessions.
    ///
    /// Per code-review pass-4 finding #2: `n = 0` is treated as
    /// "disabled" — the runtime short-circuits the cap check entirely.
    /// Otherwise the first non-zero Usage event would immediately
    /// trip BudgetExhausted on every session.
    pub fn with_max_session_tokens(mut self, n: u64) -> Self {
        self.max_session_tokens = n;
        self
    }

    /// Set the per-turn tool-invocation cap (Hardening §4.5 #3, H2).
    /// Default 64. A model emitting more than this many tool calls in
    /// one assistant turn is truncated; the runtime emits
    /// `RuntimeError::InvocationCapExceeded`.
    pub fn with_max_tool_invocations_per_turn(mut self, n: usize) -> Self {
        self.max_tool_invocations_per_turn = n;
        self
    }

    /// Set the tool-recursion depth cap (Hardening §4.5 #3, H2 reserved).
    /// Default 8. Wired in a follow-up commit; carried in
    /// `RuntimeConfig` today so embedders' Cargo.toml entries don't
    /// need to change when enforcement lands.
    pub fn with_max_recursion(mut self, n: usize) -> Self {
        self.max_recursion = n;
        self
    }

    // ─── Terminals ─────────────────────────────────────────────
    pub fn build(self) -> Result<RuntimeConfig, ConfigError> {
        Ok(RuntimeConfig {
            session_manager: self
                .session_manager
                .ok_or(ConfigError::Missing { field: "session_manager" })?,
            auth_storage: self
                .auth_storage
                .ok_or(ConfigError::Missing { field: "auth_storage" })?,
            model_registry: self
                .model_registry
                .ok_or(ConfigError::Missing { field: "model_registry" })?,
            tools: self.tools.ok_or(ConfigError::Missing { field: "tools" })?,
            settings: self
                .settings
                .ok_or(ConfigError::Missing { field: "settings" })?,
            system_prompt: self
                .system_prompt
                .ok_or(ConfigError::Missing { field: "system_prompt" })?,
            context_files: self.context_files,
            // Per code-review pass-1 finding #9: if `cwd` was not set
            // explicitly, default to `std::env::current_dir()`. Almost
            // every embedder wrote that fallback by hand. Embedders
            // wanting an explicit cwd still call `.cwd(path)`; this is
            // pure additive ergonomics.
            cwd: self
                .cwd
                .or_else(|| std::env::current_dir().ok())
                .ok_or(ConfigError::Missing { field: "cwd" })?,
            provider_factory: self.provider_factory,
            tool_gate: self.tool_gate,
            gate_ask_is_approve: self.gate_ask_is_approve,
            stream_interceptor: self.stream_interceptor,
            sandbox_provider: self.sandbox_provider,
            max_session_tokens: self.max_session_tokens,
            max_tool_invocations_per_turn: self.max_tool_invocations_per_turn,
            max_recursion: self.max_recursion,
        })
    }

    /// For tests / quick-start: panics on missing required fields.
    /// Production code should use [`build`](Self::build) and handle
    /// the `Result`.
    pub fn build_unwrap(self) -> RuntimeConfig {
        self.build().expect("ConfigBuilder::build_unwrap: required field missing")
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
        let provider = cfg.settings.provider.clone();
        let model = cfg.settings.model.clone();
        let thinking: ThinkingLevel = cfg.settings.thinking.into();
        let meta = cfg.session_manager.create(&provider, &model)?;
        Ok(AgentSession {
            id: meta.id,
            inner: Arc::new(Mutex::new(AgentSessionInner {
                sender,
                aborted: false,
                queued_messages: Vec::new(),
                messages: Vec::new(),
                provider,
                model,
                thinking,
                tools: cfg.tools.clone(),
                context_loads_emitted: false,
                session_input_tokens: 0,
                session_output_tokens: 0,
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

        // Reassemble the message stream preserving the original
        // interleaving. Tool results land in the user message that
        // *immediately* follows their assistant tool_use turn — that
        // ordering is what the Anthropic API checks (and rejects with
        // 400 `tool_use ids were found without tool_result blocks
        // immediately after` if violated).
        //
        // The previous implementation coalesced *all* tool_results
        // into a single trailing user message at the very end, which
        // broke the contract on any session that had >1 tool-call
        // turn. It also did no sanitisation, so an interrupted run
        // (Ctrl+C / panic / `/quit` mid-tool) left an orphaned
        // tool_use whose missing tool_result poisoned every
        // subsequent prompt the user sent on the resumed session.
        //
        // Rebuilt loop: flush any accumulated tool_results into a
        // user message *before* writing the next User/Assistant
        // entry. After the entries are reassembled, do a second pass
        // that scans for assistant turns whose tool_use ids aren't
        // covered by the next user message's tool_results and
        // injects synthetic `[interrupted]` results so the API
        // accepts the request and the user sees the missing results
        // as such in the transcript.
        let mut messages: Vec<Message> = Vec::new();
        let mut current_tool_results: Vec<ContentBlock> = Vec::new();
        let flush = |msgs: &mut Vec<Message>, results: &mut Vec<ContentBlock>| {
            if !results.is_empty() {
                msgs.push(Message {
                    role: Role::User,
                    content: std::mem::take(results),
                });
            }
        };
        for entry in history {
            match entry.kind {
                SessionEntryKind::User { message } => {
                    flush(&mut messages, &mut current_tool_results);
                    messages.push(message);
                }
                SessionEntryKind::Assistant { message } => {
                    flush(&mut messages, &mut current_tool_results);
                    messages.push(message);
                }
                SessionEntryKind::ToolResult { result } => {
                    current_tool_results.push(ContentBlock::ToolResult {
                        tool_use_id: result.tool_use_id,
                        content: result.model_output,
                        is_error: result.is_error,
                    });
                }
                _ => {}
            }
        }
        flush(&mut messages, &mut current_tool_results);

        let messages = sanitise_session_messages(messages);
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
                // H2: re-opened sessions reset the in-memory token
                // accumulator. Persistence of the session-total across
                // restarts is a future RFD if the demand surfaces.
                session_input_tokens: 0,
                session_output_tokens: 0,
            })),
            cfg,
        })
    }
}

/// Two-pass session sanitiser invoked when resuming a session
/// (`pi -r <id>`, `pi -c`, `--fork`, etc).
///
/// **Pass 1 — orphan tool_use → synthetic tool_result.** For any
/// assistant message that emits `tool_use` blocks, ensure the next
/// message in the stream is a user message whose `tool_result`s
/// cover every `tool_use_id`. Missing ids get a synthetic
/// `is_error: true` result with the body
/// `[tool call interrupted before completing; no result was recorded]`.
/// Without this, a Ctrl-C / panic / `/quit` mid-tool poisons every
/// subsequent prompt against the Anthropic API
/// (`tool_use ids were found without tool_result blocks
/// immediately after`). Original fix: `8b921e7`.
///
/// **Pass 2 — orphan tool_result → drop.** The mirror case: a
/// user message with `tool_result` blocks whose `tool_use_id`s
/// don't match the *immediately preceding* assistant message's
/// tool_use ids. Encountered with the OpenAI Responses API as
/// `No tool call found for function call output with call_id ...`
/// when a session log somehow keeps a tool_result whose tool_use
/// was lost upstream. Defence in depth: the tracked id-set is
/// cleared after each user message, so a tool_result that *appears
/// not immediately following* an assistant tool_use (e.g. two user
/// messages in a row, or a tool_result block at a turn boundary)
/// is treated as orphan and dropped. Empty user messages produced
/// by this pruning are dropped too.
///
/// Pure transformation — no I/O, deterministic. Safe to call from
/// tests directly.
pub(crate) fn sanitise_session_messages(messages: Vec<Message>) -> Vec<Message> {
    // Pass 1: orphan tool_use → inject synthetic tool_result.
    let mut sanitised: Vec<Message> = Vec::with_capacity(messages.len());
    let mut iter = messages.into_iter().peekable();
    while let Some(msg) = iter.next() {
        let required_ids: Vec<String> = if matches!(msg.role, Role::Assistant) {
            msg.content
                .iter()
                .filter_map(|b| match b {
                    ContentBlock::ToolUse { id, .. } => Some(id.clone()),
                    _ => None,
                })
                .collect()
        } else {
            Vec::new()
        };
        sanitised.push(msg);
        if required_ids.is_empty() {
            continue;
        }
        let provided: std::collections::HashSet<String> = match iter.peek() {
            Some(m) if matches!(m.role, Role::User) => m
                .content
                .iter()
                .filter_map(|b| match b {
                    ContentBlock::ToolResult { tool_use_id, .. } => Some(tool_use_id.clone()),
                    _ => None,
                })
                .collect(),
            _ => Default::default(),
        };
        let missing: Vec<String> = required_ids
            .into_iter()
            .filter(|id| !provided.contains(id))
            .collect();
        if missing.is_empty() {
            continue;
        }
        let synthetic: Vec<ContentBlock> = missing
            .into_iter()
            .map(|id| ContentBlock::ToolResult {
                tool_use_id: id,
                content: "[tool call interrupted before completing; no result was recorded]"
                    .to_string(),
                is_error: true,
            })
            .collect();
        if matches!(iter.peek(), Some(m) if matches!(m.role, Role::User)) {
            let mut next = iter.next().expect("peeked Some");
            let mut combined = synthetic;
            combined.append(&mut next.content);
            sanitised.push(Message {
                role: Role::User,
                content: combined,
            });
        } else {
            sanitised.push(Message {
                role: Role::User,
                content: synthetic,
            });
        }
    }

    // Pass 2: orphan tool_result → drop.
    let mut sanitised2: Vec<Message> = Vec::with_capacity(sanitised.len());
    let mut last_assistant_tool_use_ids: std::collections::HashSet<String> = Default::default();
    for msg in sanitised {
        if matches!(msg.role, Role::Assistant) {
            last_assistant_tool_use_ids = msg
                .content
                .iter()
                .filter_map(|b| match b {
                    ContentBlock::ToolUse { id, .. } => Some(id.clone()),
                    _ => None,
                })
                .collect();
            sanitised2.push(msg);
            continue;
        }
        let kept: Vec<ContentBlock> = msg
            .content
            .into_iter()
            .filter(|b| match b {
                ContentBlock::ToolResult { tool_use_id, .. } => {
                    last_assistant_tool_use_ids.contains(tool_use_id)
                }
                _ => true,
            })
            .collect();
        // Once the user message paired with the prior assistant turn is
        // consumed, the tracked set is exhausted — any LATER user-msg
        // tool_results would be orphans relative to nothing.
        last_assistant_tool_use_ids.clear();
        if kept.is_empty() {
            // Pure-orphan-tool_result user message: drop entirely.
            continue;
        }
        sanitised2.push(Message {
            role: Role::User,
            content: kept,
        });
    }
    sanitised2
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
    /// Per RFD 0027 §4.5 #3 (Hardening H2): per-session running token
    /// accumulator. `saturating_add` prevents overflow on adversarial
    /// stream events with `u64::MAX` token counts; the cumulative
    /// total is checked against `cfg.max_session_tokens` after each
    /// `Usage` event and exceeding triggers
    /// `RuntimeError::BudgetExhausted`.
    session_input_tokens: u64,
    session_output_tokens: u64,
}

impl AgentSession {
    fn routing_force_override(&self) -> Option<ForceOverride> {
        let settings = &self.cfg.settings;
        if matches!(
            settings.route,
            RouteMode::Static | RouteMode::Auto | RouteMode::Learned
        ) && (settings.route_model_override.is_some()
            || settings.route_thinking_override.is_some())
        {
            Some(ForceOverride::CliFlag {
                provider: settings.route_provider_override.clone(),
                model: settings
                    .route_model_override
                    .clone()
                    .unwrap_or_else(|| settings.model.clone()),
                thinking: settings.route_thinking_override.map(Into::into),
            })
        } else {
            None
        }
    }

    async fn apply_routing(
        &self,
        router_mode: RouteMode,
        force: Option<ForceOverride>,
        provider: String,
        model: String,
        thinking: ThinkingLevel,
        messages: &[Message],
        tools: &ToolRegistry,
    ) -> Result<(String, String, ThinkingLevel), RuntimeError> {
        if matches!(router_mode, RouteMode::Off) {
            return Ok((provider, model, thinking));
        }
        let last_user_text = messages
            .iter()
            .rev()
            .find(|m| matches!(m.role, Role::User))
            .map(extract_message_text)
            .unwrap_or_default();
        let ctx = RoutingContext {
            registry: &self.cfg.model_registry,
            user_lambda: 1.0,
            force,
            session_id: &self.id,
            cache_read_tokens: 0,
            cache_write_tokens: 0,
        };
        let tool_specs: Vec<ToolSpec> = tools
            .specs()
            .into_iter()
            .map(|s| ToolSpec { name: s.name })
            .collect();
        let decision = match router_mode {
            RouteMode::Auto => EmbeddingRouter::bundled()
                .map_err(|e| RuntimeError::Provider(e.to_string()))?
                .route(&last_user_text, messages, &tool_specs, &ctx),
            _ => StaticRouter::new(crate::router::RoutingDecision {
                route_id: "static".into(),
                provider,
                model,
                thinking,
            })
            .route(&last_user_text, messages, &tool_specs, &ctx),
        }
        .map_err(|e| RuntimeError::Provider(e.to_string()))?;
        {
            let mut g = self.inner.lock().await;
            g.provider = decision.provider.clone();
            g.model = decision.model.clone();
            g.thinking = decision.thinking;
        }
        // Telemetry: emit a `RoutingDecision` session entry so pi-stats can
        // aggregate per-route metrics. TALE-EP `<budget>` is parsed only
        // for the `hard` route — that's where heavy-thinking budgeting
        // matters; the field stays `None` everywhere else.
        let budget_tokens = if decision.route_id == "hard" {
            crate::router::parse_tale_ep_budget(&last_user_text)
        } else {
            None
        };
        let _ = self.cfg.session_manager.append(
            &self.id,
            SessionEntryKind::RoutingDecision {
                route_id: decision.route_id.clone(),
                provider: decision.provider.clone(),
                model: decision.model.clone(),
                thinking: format_thinking(decision.thinking),
                budget_tokens,
            },
        );
        Ok((decision.provider, decision.model, decision.thinking))
    }

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
        // Per RFD 0027 §4.5 #4 (Hardening H3): zero-based turn
        // counter handed to the ToolGate via GateContext so policies
        // can scope on `turn_index`.
        let mut turn_index_for_gate: u32 = 0;
        loop {
            if self.inner.lock().await.aborted {
                self.emit(AgentEventKind::Aborted).await;
                return Err(RuntimeError::Aborted);
            }

            let (router_mode, force_override, provider_name, model_name, thinking, messages, tools) = {
                let g = self.inner.lock().await;
                (
                    self.cfg.settings.route,
                    self.routing_force_override(),
                    g.provider.clone(),
                    g.model.clone(),
                    g.thinking,
                    g.messages.clone(),
                    g.tools.clone(),
                )
            };
            let (provider_name, model_name, thinking) = self
                .apply_routing(
                    router_mode,
                    force_override,
                    provider_name,
                    model_name,
                    thinking,
                    &messages,
                    &tools,
                )
                .await?;

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
                let ev = match ev {
                    Ok(ev) => ev,
                    Err(e) => {
                        // Stream-level error mid-turn (transport
                        // drop, decode failure, etc). Surface to
                        // listeners FIRST — otherwise -p / --json
                        // mode printers hang on the channel waiting
                        // for a TurnComplete that will never fire,
                        // and the operator sees a wedged process
                        // with no diagnostic. This mirrors the
                        // initial-stream() failure path above.
                        let message = e.to_string();
                        self.emit(AgentEventKind::Error {
                            message: message.clone(),
                        })
                        .await;
                        self.emit(AgentEventKind::Aborted).await;
                        return Err(RuntimeError::Provider(message));
                    }
                };
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
                        // Per RFD 0027 §4.5 #3 (Hardening H2): per-turn
                        // tool-invocation cap. A model emitting >N tool
                        // calls in one assistant turn is truncated.
                        if tool_calls.len() >= self.cfg.max_tool_invocations_per_turn {
                            tracing::warn!(
                                cap = self.cfg.max_tool_invocations_per_turn,
                                "per-turn tool-invocation cap exceeded; remaining tool calls dropped"
                            );
                            return Err(RuntimeError::InvocationCapExceeded {
                                invoked: tool_calls.len() + 1,
                                cap: self.cfg.max_tool_invocations_per_turn,
                            });
                        }
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
                        // Per RFD 0027 §4.5 #3 (Hardening H2) + code-review
                        // pass-4 finding #1: PROVIDERS EMIT CUMULATIVE
                        // USAGE EVENTS. Google emits a Usage on a
                        // standalone usageMetadata chunk AND again on the
                        // terminal-candidate chunk; Anthropic does similar
                        // via message_delta. The per-event accumulation
                        // pre-pass-4 double-counted these turns and threw
                        // legitimate sessions into BudgetExhausted at a
                        // fraction of the configured cap.
                        //
                        // Fix: keep `usage_total = usage.clone()` per event
                        // (idempotent vs cumulative providers — last value
                        // wins), then accumulate the per-turn TOTAL into
                        // the session-wide accumulator at end-of-turn
                        // (after the stream loop ends, before the
                        // assistant-message persistence).
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
            // Per RFD 0027 §4.5 #2 (Hardening H2) + code-review pass-4
            // finding #5: check Finish::ToolUse + empty tool_calls
            // BEFORE persisting / emitting the assistant message.
            // Otherwise replay tooling sees a bogus assistant turn
            // followed by an out-of-band runtime error.
            if matches!(finish, FinishReason::ToolUse) && tool_calls.is_empty() {
                return Err(RuntimeError::ToolUseFinishWithoutCalls);
            }

            // Per RFD 0027 §4.5 #3 (Hardening H2) + pass-4 finding #1:
            // accumulate the per-turn TOTAL (already cumulative for
            // Google/Anthropic providers thanks to the per-event
            // last-write-wins above) into the session-wide accumulator,
            // then check the cap BEFORE persisting the assistant
            // message. If the budget trips, the malformed turn is
            // never persisted and the embedder sees a clean error.
            //
            // `cfg.max_session_tokens == 0` is treated as "disabled"
            // (per pass-4 finding #2): any non-zero turn would
            // otherwise immediately starve all sessions.
            let session_cap = self.cfg.max_session_tokens;
            if session_cap > 0
                && (usage_total.input_tokens != 0 || usage_total.output_tokens != 0)
            {
                let mut g = self.inner.lock().await;
                g.session_input_tokens =
                    g.session_input_tokens.saturating_add(usage_total.input_tokens);
                g.session_output_tokens =
                    g.session_output_tokens.saturating_add(usage_total.output_tokens);
                let total =
                    g.session_input_tokens.saturating_add(g.session_output_tokens);
                if total > session_cap {
                    let cap = session_cap;
                    let session_input = g.session_input_tokens;
                    let session_output = g.session_output_tokens;
                    drop(g);
                    tracing::warn!(
                        used = total,
                        cap,
                        "per-session token budget exhausted"
                    );
                    // Per code-review pass-4 finding #4 + pass-5 #3:
                    // synthesize a final Usage event whose
                    // `input_tokens`/`output_tokens` BOTH reflect the
                    // cumulative session totals — pre-fix the
                    // `output_tokens` field accidentally carried the
                    // per-turn value, mismatching the figures in
                    // `RuntimeError::BudgetExhausted`.
                    let cumulative_usage = Usage {
                        input_tokens: session_input,
                        output_tokens: session_output,
                        ..usage_total.clone()
                    };
                    self.emit(AgentEventKind::Usage { usage: cumulative_usage })
                        .await;
                    return Err(RuntimeError::BudgetExhausted { used: total, cap });
                }
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
            // Bump turn counter for any subsequent ToolGate calls
            // within this same body. (We bump *before* the gate
            // checks so the first turn handed to a gate is `1`, not
            // `0`; `0` is reserved for "before any assistant message
            // emitted" which never reaches the gate code path.)
            turn_index_for_gate = turn_index_for_gate.saturating_add(1);

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
                // Per Hardening §4.5 #2 (RFD 0027): replace `unwrap()`
                // with an explicit `EmptyTurn` error. Reachable when a
                // stream emits only Usage + Finish without any assistant
                // message — the prior unwrap path panicked the worker.
                return last_assistant.ok_or(RuntimeError::EmptyTurn);
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
                    // Per RFD 0027 §4.5 #4 (Hardening H3): hand the
                    // gate a GateContext so session-scoped policies
                    // can't be bypassed cross-session. recursion_depth
                    // is informational at H3 (the cap lives in H2);
                    // turn_index counts AssistantMessage emissions.
                    let gate_ctx = GateContext {
                        session_id: self.id.clone(),
                        turn_index: turn_index_for_gate,
                        parent_session: None,
                        recursion_depth: 0,
                    };
                    let outcome = gate.approve(&gate_ctx, &call.name, &call.input).await;
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
                let invocation = if let Some(sandbox) = &self.cfg.sandbox_provider {
                    let started = std::time::Instant::now();
                    let res = self
                        .invoke_via_sandbox(sandbox.as_ref(), &tool_ctx, &call)
                        .await;
                    let duration_ms = started.elapsed().as_millis() as u64;
                    // Telemetry row goes BEFORE the ToolResult so analyses
                    // that join action↔result by ordinal still line up.
                    let (exit_status, is_error) = match &res {
                        Ok(r) => (if r.is_error { 1 } else { 0 }, r.is_error),
                        Err(_) => (1, true),
                    };
                    let _ = self.cfg.session_manager.append(
                        &self.id,
                        SessionEntryKind::SandboxAction {
                            provider: sandbox.name().to_string(),
                            tool_name: call.name.clone(),
                            duration_ms,
                            exit_status,
                            is_error,
                        },
                    );
                    res
                } else {
                    // Per Hardening §4.5 #1 (RFD 0027): wrap
                    // `tool.invoke()` in `AssertUnwindSafe(...).catch_unwind()`
                    // so a panicking custom Tool returns a `ToolPanicked`-style
                    // error instead of killing the tokio worker thread.
                    // The future is wrapped in `AssertUnwindSafe` because tools
                    // are by definition unwind-safe contracts (a panicking tool
                    // forfeits its own state; the runtime makes no observable
                    // changes after the catch).
                    let invoke_fut =
                        AssertUnwindSafe(tool.invoke(&tool_ctx, &call.id, call.input.clone()));
                    match invoke_fut.catch_unwind().await {
                        Ok(r) => r.map_err(|e| e.to_string()),
                        Err(panic_payload) => {
                            // Per code-review finding #1 (pass-2): use the
                            // RuntimeError::ToolPanicked variant constructor
                            // so the variant identity is exercised. Display
                            // produces `"tool `{name}` panicked: {message}"`
                            // — the same shape as the prior inline format.
                            let err = RuntimeError::tool_panic_message(
                                call.name.as_str(),
                                panic_payload.as_ref(),
                            );
                            tracing::warn!(
                                tool = %call.name,
                                err = %err,
                                "tool.invoke() panicked; caught by H1 hardening guard"
                            );
                            Err(err.to_string())
                        }
                    }
                };
                match invocation {
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

    /// Dispatch one tool decision through the configured sandbox provider
    /// and reshape its `(stdout, stderr, exit_status)` output into a
    /// standard `ToolResult`. Telemetry emission happens in the caller.
    async fn invoke_via_sandbox(
        &self,
        provider: &dyn SandboxProvider,
        ctx: &ToolContext,
        call: &ToolCall,
    ) -> Result<ToolResult, String> {
        match provider.execute_tool(ctx, &call.name, &call.input).await {
            Ok(exec) => Ok(ToolResult {
                tool_use_id: call.id.clone(),
                model_output: exec.stdout,
                display: None,
                is_error: exec.exit_status != 0,
            }),
            Err(e) => Err(e.to_string()),
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

fn format_thinking(level: ThinkingLevel) -> String {
    match level {
        ThinkingLevel::Off => "off",
        ThinkingLevel::Low => "low",
        ThinkingLevel::Medium => "medium",
        ThinkingLevel::High => "high",
        ThinkingLevel::XHigh => "xhigh",
    }
    .to_string()
}

fn extract_message_text(message: &Message) -> String {
    let mut text = String::new();
    for block in &message.content {
        match block {
            ContentBlock::Text { text: block_text }
            | ContentBlock::Thinking {
                text: block_text, ..
            } => {
                if !text.is_empty() {
                    text.push('\n');
                }
                text.push_str(block_text);
            }
            ContentBlock::ToolResult { content, .. } => {
                if !text.is_empty() {
                    text.push('\n');
                }
                text.push_str(content);
            }
            ContentBlock::ToolUse { name, .. } => {
                if !text.is_empty() {
                    text.push('\n');
                }
                text.push_str(name);
            }
            _ => {}
        }
    }
    text
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
#[non_exhaustive]
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
    /// Assistant turn finished without producing any assistant message
    /// (Hardening §4.5 #2). Replaces `last_assistant.unwrap()`.
    #[error("turn ended without an assistant message")]
    EmptyTurn,
    /// A custom `Tool::invoke` panicked. The runtime caught the panic
    /// and returns this variant instead of aborting the worker thread
    /// (Hardening §4.5 #1).
    ///
    /// **Variant identity, not error path.** The runtime's tool-call
    /// path catches the panic and converts the result into a
    /// `ToolResult { is_error: true, model_output: "ERROR: tool ..."`
    /// so the model sees the failure as part of normal tool dispatch.
    /// This variant exists so embedders implementing their own
    /// `Tool::invoke` wrapper (e.g. for telemetry or alerting) can
    /// pattern-match on the panic case via the
    /// [`tool_panic_message`](Self::tool_panic_message) helper or
    /// via direct construction in their own runtime layer.
    #[error("tool `{tool}` panicked: {message}")]
    ToolPanicked { tool: String, message: String },
    /// Per-session token budget cap exceeded (Hardening §4.5 #3, H2).
    /// `cfg.max_session_tokens` was crossed by the cumulative input +
    /// output tokens reported via `Usage` events. Embedders treating
    /// this as recoverable can bump `max_session_tokens` and retry.
    #[error("session token budget exhausted: used {used}, cap {cap}")]
    BudgetExhausted { used: u64, cap: u64 },
    /// Per-turn tool-invocation cap exceeded (Hardening §4.5 #3, H2).
    /// `cfg.max_tool_invocations_per_turn` was reached and the model
    /// kept emitting `ToolCallComplete` events. Indicates the model
    /// is in a tool-call loop or the cap is sized too low for the
    /// workload.
    #[error("per-turn tool-invocation cap exceeded: invoked {invoked}, cap {cap}")]
    InvocationCapExceeded { invoked: usize, cap: usize },
    /// Tool-recursion depth cap exceeded (Hardening §4.5 #3, H2 reserved).
    /// Currently produced only via direct construction; the in-loop
    /// enforcement against `cfg.max_recursion` lands in H2.5 once the
    /// recursion sites are audited.
    #[error("tool recursion depth exceeded: depth {depth}, cap {cap}")]
    DepthExceeded { depth: usize, cap: usize },
    /// `Finish { reason: ToolUse }` was emitted with no tool calls
    /// in the same assistant message (Hardening §4.5 #2). Indicates
    /// either a malformed provider stream or a model trying to mark
    /// the turn as a tool-use turn without actually requesting any
    /// tools.
    #[error("Finish::ToolUse received with no tool calls in the assistant message")]
    ToolUseFinishWithoutCalls,
}

impl RuntimeError {
    /// Construct a [`ToolPanicked`](Self::ToolPanicked) variant from a
    /// `std::panic::catch_unwind` payload. Useful for embedders
    /// implementing their own `Tool::invoke` wrapper.
    ///
    /// Per code-review finding #6 (commit-pass-2): takes
    /// `&(dyn Any + Send)` (idiomatic) instead of `&Box<dyn Any + Send>`
    /// to avoid `clippy::borrowed_box`. Callers using `&boxed_payload`
    /// continue to work via auto-deref.
    pub fn tool_panic_message(
        tool: impl Into<String>,
        payload: &(dyn std::any::Any + Send),
    ) -> Self {
        let message = if let Some(s) = payload.downcast_ref::<&'static str>() {
            (*s).to_string()
        } else if let Some(s) = payload.downcast_ref::<String>() {
            s.clone()
        } else {
            "tool panicked (non-string payload)".to_string()
        };
        Self::ToolPanicked { tool: tool.into(), message }
    }
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

#[cfg(test)]
mod sanitise_tests {
    //! Lock the two-pass session sanitiser invoked when resuming a
    //! session. Pass 1 (orphan tool_use → synthetic tool_result) was
    //! introduced in `8b921e7`; pass 2 (orphan tool_result → drop)
    //! mirrors it for the OpenAI Responses-side bug.

    use super::sanitise_session_messages;
    use pi_ai::{ContentBlock, Message, Role};

    fn user(blocks: Vec<ContentBlock>) -> Message {
        Message {
            role: Role::User,
            content: blocks,
        }
    }
    fn assistant(blocks: Vec<ContentBlock>) -> Message {
        Message {
            role: Role::Assistant,
            content: blocks,
        }
    }
    fn text(s: &str) -> ContentBlock {
        ContentBlock::Text { text: s.into() }
    }
    fn tool_use(id: &str, name: &str) -> ContentBlock {
        ContentBlock::ToolUse {
            id: id.into(),
            name: name.into(),
            input: serde_json::json!({}),
        }
    }
    fn tool_result(id: &str, body: &str) -> ContentBlock {
        ContentBlock::ToolResult {
            tool_use_id: id.into(),
            content: body.into(),
            is_error: false,
        }
    }
    fn tool_use_ids(msg: &Message) -> Vec<String> {
        msg.content
            .iter()
            .filter_map(|b| match b {
                ContentBlock::ToolUse { id, .. } => Some(id.clone()),
                _ => None,
            })
            .collect()
    }
    fn tool_result_ids(msg: &Message) -> Vec<String> {
        msg.content
            .iter()
            .filter_map(|b| match b {
                ContentBlock::ToolResult { tool_use_id, .. } => Some(tool_use_id.clone()),
                _ => None,
            })
            .collect()
    }

    // ─── pass 1 ─────────────────────────────────────────────

    #[test]
    fn pass1_clean_session_passes_through_unchanged() {
        let input = vec![
            user(vec![text("hi")]),
            assistant(vec![text("ok"), tool_use("call_a", "bash")]),
            user(vec![tool_result("call_a", "done")]),
            assistant(vec![text("good")]),
        ];
        let output = sanitise_session_messages(input);
        assert_eq!(output.len(), 4);
        assert_eq!(tool_use_ids(&output[1]), vec!["call_a"]);
        assert_eq!(tool_result_ids(&output[2]), vec!["call_a"]);
    }

    #[test]
    fn pass1_orphan_tool_use_at_eof_gets_synthetic_result() {
        let input = vec![
            user(vec![text("do it")]),
            assistant(vec![tool_use("call_x", "bash")]),
        ];
        let output = sanitise_session_messages(input);
        assert_eq!(output.len(), 3);
        assert!(matches!(output[2].role, Role::User));
        assert_eq!(tool_result_ids(&output[2]), vec!["call_x"]);
        let body = match &output[2].content[0] {
            ContentBlock::ToolResult {
                content, is_error, ..
            } => {
                assert!(is_error);
                content.clone()
            }
            _ => panic!("expected ToolResult"),
        };
        assert!(body.contains("interrupted"));
    }

    #[test]
    fn pass1_partial_coverage_only_missing_ids_get_synthetic() {
        let input = vec![
            assistant(vec![
                tool_use("a", "bash"),
                tool_use("b", "bash"),
                tool_use("c", "bash"),
            ]),
            user(vec![tool_result("a", "ok-a"), tool_result("c", "ok-c")]),
        ];
        let output = sanitise_session_messages(input);
        assert_eq!(output.len(), 2);
        let mut ids = tool_result_ids(&output[1]);
        ids.sort();
        assert_eq!(ids, vec!["a", "b", "c"]);
    }

    // ─── pass 2 ─────────────────────────────────────────────

    #[test]
    fn pass2_drops_tool_result_with_no_matching_prior_tool_use() {
        // The exact bug we hit today: a user message carries a
        // tool_result whose tool_use_id has no matching tool_use in
        // the immediately preceding assistant turn (the tool_use was
        // lost upstream). Pass 2 drops the result; the user message
        // becomes empty → user message itself is dropped.
        let input = vec![
            user(vec![text("do it")]),
            assistant(vec![text("ok")]),
            user(vec![tool_result("orphan_id", "should be dropped")]),
        ];
        let output = sanitise_session_messages(input);
        assert_eq!(output.len(), 2);
        assert!(matches!(output[0].role, Role::User));
        assert!(matches!(output[1].role, Role::Assistant));
    }

    #[test]
    fn pass2_keeps_text_blocks_around_orphan_tool_results() {
        let input = vec![
            user(vec![text("do it")]),
            assistant(vec![text("ok")]),
            user(vec![
                text("real follow-up question"),
                tool_result("orphan_id", "should be dropped"),
            ]),
        ];
        let output = sanitise_session_messages(input);
        assert_eq!(output.len(), 3);
        let last = &output[2];
        assert!(matches!(last.role, Role::User));
        assert_eq!(last.content.len(), 1);
        assert!(matches!(last.content[0], ContentBlock::Text { .. }));
    }

    #[test]
    fn pass2_id_set_clears_after_first_consumer() {
        // Two consecutive user messages after a tool-call turn: the
        // second one's tool_result is an orphan because the prior
        // user already consumed the matching tool_use_id set.
        let input = vec![
            assistant(vec![tool_use("call_x", "bash")]),
            user(vec![tool_result("call_x", "first consumer")]),
            user(vec![tool_result("call_x", "second consumer")]),
        ];
        let output = sanitise_session_messages(input);
        assert_eq!(output.len(), 2);
        assert_eq!(tool_result_ids(&output[1]), vec!["call_x"]);
    }

    // ─── pass 1 + pass 2 interaction ────────────────────────

    #[test]
    fn passes_interact_synthetic_survives_pass2() {
        // Pass 1 injects a synthetic tool_result. Pass 2 must keep
        // it — it matches the prior assistant tool_use.
        let input = vec![assistant(vec![tool_use("call_y", "bash")])];
        let output = sanitise_session_messages(input);
        assert_eq!(output.len(), 2);
        assert_eq!(tool_result_ids(&output[1]), vec!["call_y"]);
    }

    #[test]
    fn empty_input_returns_empty() {
        assert_eq!(sanitise_session_messages(Vec::new()).len(), 0);
    }
}
