//! pi-agent-core — agent loop, sessions, events.

pub mod compaction;
pub mod context;
pub mod event;
pub mod runtime;
pub mod session;
pub mod settings;
pub mod system;

pub use compaction::{Compactor, LlmCompactor};
pub use context::{discover_context_files, ContextFile};
pub use event::{AgentEvent, AgentEventKind, EventSender};
pub use runtime::{
    create_agent_session, AgentSession, AgentSessionRuntime, DefaultProviderFactory,
    InterceptAction, ProviderFactory, RuntimeConfig, StreamInterceptor, ToolGate, ToolGateOutcome,
};
pub use session::{
    SessionEntry, SessionEntryKind, SessionManager, SessionMeta, SessionTree,
};
pub use settings::Settings;
pub use system::default_system_prompt;
