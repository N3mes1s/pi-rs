//! pi-agent-core — agent loop, sessions, events.

pub mod compaction;
pub mod context;
pub mod event;
pub mod router;
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
pub use router::{
    default_embedding_model_path, fetch_default_embeddings, EmbeddingRouter, ForceOverride, Outcome,
    RouteMode, Router, RouterError, RoutingContext, RoutingDecision, StaticRouter, ToolSpec,
};
pub use session::{
    OutcomeSource, SessionEntry, SessionEntryKind, SessionManager, SessionMeta, SessionTree,
};
pub use settings::{EvolveSettings, Settings};
pub use system::default_system_prompt;
