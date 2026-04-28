//! SDK surface for embedding pi-rs in another application.
//! Mirrors `createAgentSession` / `createAgentSessionRuntime` from upstream pi.

pub use pi_agent_core::{
    create_agent_session, AgentEvent, AgentEventKind, AgentSession, AgentSessionRuntime, Compactor,
    ContextFile, EventSender, RuntimeConfig, SessionEntry, SessionEntryKind, SessionManager,
    SessionMeta, SessionTree, Settings,
};
pub use pi_ai::{
    AuthMethod, AuthStorage, ContentBlock, FinishReason, Message, ModelInfo, ModelRegistry,
    ProviderConfig, ProviderKind, Role, ThinkingLevel, ToolCall, ToolResult, ToolSpec, Usage,
};
pub use pi_tools::{Tool, ToolContext, ToolRegistry};

use std::path::PathBuf;

/// Convenience builder used by the binary, exposed for SDK callers.
#[derive(Clone)]
pub struct BuildConfig {
    pub auth: AuthStorage,
    pub registry: ModelRegistry,
    pub session_manager: SessionManager,
    pub tools: ToolRegistry,
    pub settings: Settings,
    pub system_prompt: String,
    pub context_files: Vec<ContextFile>,
    pub cwd: PathBuf,
}

impl Default for BuildConfig {
    fn default() -> Self {
        let auth = AuthStorage::from_env();
        let registry = ModelRegistry::new(auth.clone());
        Self {
            auth,
            registry,
            session_manager: SessionManager::in_memory(),
            tools: ToolRegistry::with_defaults(),
            settings: Settings::default(),
            system_prompt: pi_agent_core::default_system_prompt().to_string(),
            context_files: Vec::new(),
            cwd: std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")),
        }
    }
}

pub fn build_runtime_config(b: BuildConfig) -> RuntimeConfig {
    RuntimeConfig {
        session_manager: b.session_manager,
        auth_storage: b.auth,
        model_registry: b.registry,
        tools: b.tools,
        settings: b.settings,
        system_prompt: b.system_prompt,
        context_files: b.context_files,
        cwd: b.cwd,
        provider_factory: None,
        tool_gate: None,
        gate_ask_is_approve: false,
        stream_interceptor: None,
    }
}
