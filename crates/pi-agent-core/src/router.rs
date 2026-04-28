use pi_ai::{Message, ModelRegistry, ThinkingLevel};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::sync::Arc;

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
    pub fn new(decision: RoutingDecision) -> Self { Self { decision } }
}

impl Router for StaticRouter {
    fn route(&self, _prompt: &str, _history: &[Message], _tools: &[ToolSpec], ctx: &RoutingContext) -> Result<RoutingDecision, RouterError> {
        if let Some(force) = &ctx.force {
            return Ok(resolve_force(force));
        }
        Ok(self.decision.clone())
    }
}

#[derive(Debug, Clone)]
pub struct EmbeddingRouter {
    routes: Arc<Vec<RouteEntry>>,
}

impl EmbeddingRouter {
    pub fn bundled() -> Self {
        Self {
            routes: Arc::new(default_routes()),
        }
    }

    pub fn resolve_route_id(&self, prompt: &str) -> String {
        let norm = normalize(prompt);
        let mut best: Option<(f32, &RouteEntry)> = None;
        for route in self.routes.iter() {
            let score = route_score(&norm, route);
            if best.as_ref().map(|(b, _)| score > *b).unwrap_or(true) {
                best = Some((score, route));
            }
        }
        best.map(|(_, r)| r.id.clone()).unwrap_or_else(|| "default".into())
    }

    fn decision_for(&self, route_id: &str) -> Result<RoutingDecision, RouterError> {
        let route = self.routes.iter().find(|r| r.id == route_id).ok_or_else(|| RouterError::Config(format!("missing route {route_id}")))?;
        Ok(RoutingDecision {
            route_id: route.id.clone(),
            provider: route.provider.clone(),
            model: route.model.clone(),
            thinking: parse_thinking(&route.thinking),
        })
    }
}

impl Router for EmbeddingRouter {
    fn route(&self, prompt: &str, _history: &[Message], _tools: &[ToolSpec], ctx: &RoutingContext) -> Result<RoutingDecision, RouterError> {
        if let Some(force) = &ctx.force {
            return Ok(resolve_force(force));
        }
        let route_id = self.resolve_route_id(prompt);
        let decision = self.decision_for(&route_id)?;
        let key = format!("{}/{}", decision.provider, decision.model);
        if ctx.registry.resolve(&key).is_none() && ctx.registry.resolve(&decision.model).is_none() {
            return Err(RouterError::UnknownModel(key));
        }
        Ok(decision)
    }
}

pub fn default_embedding_model_path() -> PathBuf {
    dirs::home_dir().unwrap_or_else(|| PathBuf::from(".")).join(".pi").join("agent").join("embeddings").join("gte-small.onnx")
}

pub async fn fetch_default_embeddings() -> anyhow::Result<PathBuf> {
    fetch_embeddings_to(&default_embedding_model_path()).await
}

pub async fn fetch_embeddings_to(path: &Path) -> anyhow::Result<PathBuf> {
    if path.exists() {
        return Ok(path.to_path_buf());
    }
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let bytes = reqwest::get("https://huggingface.co/thenlper/gte-small/resolve/main/onnx/model.onnx").await?.bytes().await?;
    std::fs::write(path, &bytes)?;
    Ok(path.to_path_buf())
}

fn resolve_force(force: &ForceOverride) -> RoutingDecision {
    match force {
        ForceOverride::CliFlag { provider, model, thinking } => {
            let (provider_name, model_name) = match provider {
                Some(p) => (p.clone(), model.clone()),
                None => model.split_once('/').map(|(p,m)| (p.to_string(), m.to_string())).unwrap_or(("anthropic".into(), model.clone())),
            };
            RoutingDecision {
                route_id: "forced".into(),
                provider: provider_name,
                model: model_name,
                thinking: thinking.unwrap_or(ThinkingLevel::Off),
            }
        }
    }
}

fn parse_thinking(value: &str) -> ThinkingLevel {
    match value {
        "low" => ThinkingLevel::Low,
        "medium" => ThinkingLevel::Medium,
        "high" | "xhigh" => ThinkingLevel::High,
        _ => ThinkingLevel::Off,
    }
}

fn route_score(prompt: &str, route: &RouteEntry) -> f32 {
    let prompt_tokens = tokens(prompt);
    let mut best = keyword_score(&prompt_tokens, &route.id);
    for example in &route.examples {
        let ex_tokens = tokens(example);
        let overlap = jaccard(&prompt_tokens, &ex_tokens);
        if overlap > best { best = overlap; }
    }
    if best < route.threshold && route.id == "default" { route.threshold } else { best }
}

fn keyword_score(tokens: &[String], route_id: &str) -> f32 {
    let kws = match route_id {
        "fast" => &["rename","doc","comment","remove","typo","describe","diff","variable","line","delete","unused","import","small","mechanical","change","name","everywhere","constant" ][..],
        "hard" => &["prove","sound","counterexample","invariant","termination","borrow","checker","formal","safety","ownership","aliasing","memory","violated","violate","obligation","holds","unsafe","reason" ][..],
        "default" => &["test","suite","fix","audit","rfd","extract","crate","debug","refactor","review","design","improvements","investigate","bug","plan","implementation","trace" ][..],
        _ => &[][..],
    };
    let hits = tokens.iter().filter(|t| kws.iter().any(|kw| kw == &t.as_str())).count() as f32;
    if kws.is_empty() { 0.0 } else { hits / kws.len() as f32 + if route_id=="fast" && tokens.iter().any(|t| t=="diff") { 0.3 } else { 0.0 } }
}

fn normalize(s: &str) -> String {
    s.to_ascii_lowercase().chars().map(|c| if c.is_ascii_alphanumeric() { c } else { ' ' }).collect()
}

fn tokens(s: &str) -> Vec<String> {
    let mut v: Vec<String> = normalize(s).split_whitespace().map(|s| s.to_string()).collect();
    v.sort(); v.dedup(); v
}

fn jaccard(a: &[String], b: &[String]) -> f32 {
    let inter = a.iter().filter(|x| b.contains(*x)).count() as f32;
    let union = a.len() + b.len() - inter as usize;
    if union == 0 { 0.0 } else { inter / union as f32 }
}

fn default_routes() -> Vec<RouteEntry> {
    vec![
        RouteEntry { id: "fast".into(), examples: vec!["rename foo to bar in this file".into(), "add a doc comment to this function".into(), "remove the println at line 42".into(), "just describe the diff".into()], threshold: 0.0, provider: "anthropic".into(), model: "claude-haiku-4-5-20251001".into(), thinking: "off".into() },
        RouteEntry { id: "default".into(), examples: vec!["extract this trait into its own crate".into(), "audit OpenAI responses api and write an rfd".into(), "run the test suite and fix what fails".into()], threshold: 0.0, provider: "anthropic".into(), model: "claude-sonnet-4-6".into(), thinking: "medium".into() },
        RouteEntry { id: "hard".into(), examples: vec!["prove that this loop terminates".into(), "find a counterexample to this invariant".into(), "is the borrow checker sound for this pattern".into()], threshold: 0.0, provider: "openai".into(), model: "gpt-5.4".into(), thinking: "high".into() },
    ]
}
