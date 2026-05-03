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
#[cfg(feature = "onnx-inference")]
pub use router::OnnxRealEngine;
pub use router::{
    default_embedding_model_path, fetch_default_embeddings, parse_tale_ep_budget,
    validate_embedding_model, EmbeddingEngine, EmbeddingRouter, ForceOverride, Outcome, RouteMode,
    Router, RouterError, RoutingContext, RoutingDecision, StaticRouter, ToolSpec,
};
pub use runtime::{
    create_agent_session, AgentSession, AgentSessionRuntime, ConfigBuilder, ConfigError,
    DefaultProviderFactory, GateContext, InterceptAction, ProviderFactory, RuntimeConfig,
    RuntimeError, StreamInterceptor, ToolGate, ToolGateOutcome,
};
pub use session::{
    OutcomeSource, SessionEntry, SessionEntryKind, SessionManager, SessionMeta, SessionTree,
    WireSerializer,
};
pub use settings::{EvolveSettings, Settings};
pub use system::default_system_prompt;
