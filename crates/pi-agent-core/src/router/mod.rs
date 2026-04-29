//! Routing — types, traits, and the two concrete router impls.
//!
//! Public API surface (re-exported by `pi_agent_core::lib`):
//!   * `Router` trait + `RoutingContext` + `RoutingDecision` + `Outcome`
//!     + `RouteMode` + `ForceOverride` + `RouterError` + `ToolSpec`
//!     + `RouteEntry` (this file)
//!   * `StaticRouter` (this file)
//!   * `EmbeddingRouter` (`embedding.rs`)
//!   * `EmbeddingEngine` trait + `OnnxRealEngine` (cfg-gated) +
//!     `default_embedding_model_path` + `validate_embedding_model` +
//!     `fetch_default_embeddings` (`engine.rs`)
//!   * `parse_tale_ep_budget` (`text.rs`)
//!   * `resolve_router_dir` (`exemplars.rs`)
//!
//! Submodule layout:
//!   * `text` — pure functions: cosine sim, hashed embedding,
//!     tokenize/stem, prompt assembly, force-override resolution.
//!   * `exemplars` — bundled `data/routes/*.txt`, the `parse_route_file`
//!     subtraction-aware parser, override-directory resolution, and
//!     the `default_routes()` constructor.
//!   * `engine` — `EmbeddingEngine` trait + the hashed shim and
//!     real ONNX engine + model/tokenizer fetch + `validate_embedding_model`.
//!   * `embedding` — `EmbeddingRouter` cosine max-pool routing.

mod embedding;
mod engine;
mod exemplars;
mod text;

pub use embedding::EmbeddingRouter;
pub use engine::{
    default_embedding_model_path, fetch_default_embeddings, validate_embedding_model,
    EmbeddingEngine,
};
#[cfg(feature = "onnx-inference")]
pub use engine::OnnxRealEngine;
pub use exemplars::resolve_router_dir;
pub use text::parse_tale_ep_budget;

use pi_ai::{Message, ModelRegistry, ThinkingLevel};
use serde::{Deserialize, Serialize};
use text::resolve_force;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum RouteMode {
    Off,
    #[default]
    Static,
    Auto,
    Learned,
}

impl RouteMode {
    pub fn parse(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "off" => Some(Self::Off),
            "static" => Some(Self::Static),
            "auto" => Some(Self::Auto),
            "learned" => Some(Self::Learned),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct RoutingDecision {
    pub route_id: String,
    pub provider: String,
    pub model: String,
    pub thinking: ThinkingLevel,
}

#[derive(Debug, Clone)]
pub enum ForceOverride {
    CliFlag {
        provider: Option<String>,
        model: String,
        thinking: Option<ThinkingLevel>,
    },
}

#[derive(Debug, Clone)]
pub struct RoutingContext<'a> {
    pub registry: &'a ModelRegistry,
    pub user_lambda: f64,
    pub force: Option<ForceOverride>,
    pub session_id: &'a str,
    pub cache_read_tokens: u64,
    pub cache_write_tokens: u64,
}

#[derive(Debug, Clone)]
pub struct Outcome {
    pub cost_usd: f64,
}

pub trait Router: Send + Sync {
    fn route(
        &self,
        prompt: &str,
        history: &[Message],
        tools: &[ToolSpec],
        ctx: &RoutingContext,
    ) -> Result<RoutingDecision, RouterError>;

    fn observe(&self, _decision: &RoutingDecision, _outcome: &Outcome) {}
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolSpec {
    pub name: String,
}

#[derive(Debug, thiserror::Error)]
pub enum RouterError {
    #[error("unknown model: {0}")]
    UnknownModel(String),
    #[error("router config error: {0}")]
    Config(String),
    #[error("embedding model unavailable: {0}")]
    EmbeddingsUnavailable(String),
    #[error("embedding inference failed: {0}")]
    Inference(String),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RouteEntry {
    pub id: String,
    pub examples: Vec<String>,
    pub threshold: f32,
    pub provider: String,
    pub model: String,
    pub thinking: String,
}

#[derive(Debug, Clone)]
pub struct StaticRouter {
    decision: RoutingDecision,
}

impl StaticRouter {
    pub fn new(decision: RoutingDecision) -> Self {
        Self { decision }
    }
}

impl Router for StaticRouter {
    fn route(
        &self,
        _prompt: &str,
        _history: &[Message],
        _tools: &[ToolSpec],
        ctx: &RoutingContext,
    ) -> Result<RoutingDecision, RouterError> {
        if let Some(force) = &ctx.force {
            return Ok(resolve_force(force));
        }
        Ok(self.decision.clone())
    }
}
