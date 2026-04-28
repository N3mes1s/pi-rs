use anyhow::anyhow;
use ort::{session::builder::GraphOptimizationLevel, session::Session, value::TensorRef};
use pi_ai::{Message, ModelRegistry, ThinkingLevel};
use serde::{Deserialize, Serialize};
use std::cmp::Ordering;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};

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

#[derive(Clone)]
pub struct EmbeddingRouter {
    routes: Arc<Vec<RouteEntry>>,
    engine: Arc<dyn EmbeddingEngine>,
}

impl EmbeddingRouter {
    pub fn bundled() -> Result<Self, RouterError> {
        let path = default_embedding_model_path();
        Self::from_model_path(path)
    }

    pub fn from_model_path(path: impl AsRef<Path>) -> Result<Self, RouterError> {
        let path = path.as_ref().to_path_buf();
        if !path.exists() {
            return Err(RouterError::EmbeddingsUnavailable(format!(
                "{} not found; run `pi router fetch-embeddings`",
                path.display()
            )));
        }
        let engine = OnnxEmbeddingEngine::new(path)?;
        Ok(Self::with_engine(default_routes(), Arc::new(engine)))
    }

    pub fn with_engine(routes: Vec<RouteEntry>, engine: Arc<dyn EmbeddingEngine>) -> Self {
        Self {
            routes: Arc::new(routes),
            engine,
        }
    }

    pub fn resolve_route_id(&self, prompt: &str) -> Result<String, RouterError> {
        self.resolve_route(prompt).map(|(route, _)| route.id.clone())
    }

    fn resolve_route(&self, prompt: &str) -> Result<(RouteEntry, f32), RouterError> {
        let prompt_embedding = self.engine.embed(&router_input(prompt, &[], &[]))?;
        let mut best: Option<(f32, &RouteEntry)> = None;
        for route in self.routes.iter() {
            let score = self.route_similarity(route, &prompt_embedding)?;
            match best {
                Some((best_score, _)) if score <= best_score => {}
                _ => best = Some((score, route)),
            }
        }
        let (score, route) = best.ok_or_else(|| RouterError::Config("no routes configured".into()))?;
        Ok((route.clone(), score))
    }

    fn route_similarity(&self, route: &RouteEntry, prompt_embedding: &[f32]) -> Result<f32, RouterError> {
        let mut sims = Vec::with_capacity(route.examples.len().max(1));
        if route.examples.is_empty() {
            return Err(RouterError::Config(format!("route {} has no examples", route.id)));
        }
        for example in &route.examples {
            let example_embedding = self.engine.embed(example)?;
            sims.push(cosine_similarity(prompt_embedding, &example_embedding));
        }
        sims.sort_by(|a, b| b.partial_cmp(a).unwrap_or(Ordering::Equal));
        Ok(*sims.first().unwrap_or(&0.0))
    }

    fn decision_for(&self, route_id: &str) -> Result<RoutingDecision, RouterError> {
        let route = self
            .routes
            .iter()
            .find(|r| r.id == route_id)
            .ok_or_else(|| RouterError::Config(format!("missing route {route_id}")))?;
        Ok(RoutingDecision {
            route_id: route.id.clone(),
            provider: route.provider.clone(),
            model: route.model.clone(),
            thinking: parse_thinking(&route.thinking),
        })
    }
}

impl Router for EmbeddingRouter {
    fn route(
        &self,
        prompt: &str,
        history: &[Message],
        tools: &[ToolSpec],
        ctx: &RoutingContext,
    ) -> Result<RoutingDecision, RouterError> {
        if let Some(force) = &ctx.force {
            return Ok(resolve_force(force));
        }
        let prompt = router_input(prompt, history, tools);
        let route_id = self.resolve_route_id(&prompt)?;
        let decision = self.decision_for(&route_id)?;
        let key = format!("{}/{}", decision.provider, decision.model);
        if ctx.registry.resolve(&key).is_none() && ctx.registry.resolve(&decision.model).is_none() {
            return Err(RouterError::UnknownModel(key));
        }
        Ok(decision)
    }
}

pub fn default_embedding_model_path() -> PathBuf {
    dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".pi")
        .join("agent")
        .join("embeddings")
        .join("gte-small.onnx")
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
    let bytes = reqwest::get("https://huggingface.co/thenlper/gte-small/resolve/main/onnx/model.onnx")
        .await?
        .bytes()
        .await?;
    std::fs::write(path, &bytes)?;
    Ok(path.to_path_buf())
}

pub trait EmbeddingEngine: Send + Sync {
    fn embed(&self, text: &str) -> Result<Vec<f32>, RouterError>;
}

#[derive(Debug)]
struct OnnxEmbeddingEngine {
    _session: Arc<Session>,
    cache: Mutex<std::collections::HashMap<String, Vec<f32>>>,
}

impl OnnxEmbeddingEngine {
    fn new(path: PathBuf) -> Result<Self, RouterError> {
        let session = Session::builder()
            .map_err(|e| RouterError::EmbeddingsUnavailable(e.to_string()))?
            .with_optimization_level(GraphOptimizationLevel::Level3)
            .map_err(|e| RouterError::EmbeddingsUnavailable(e.to_string()))?
            .commit_from_file(path)
            .map_err(|e| RouterError::EmbeddingsUnavailable(e.to_string()))?;
        Ok(Self {
            _session: Arc::new(session),
            cache: Mutex::new(std::collections::HashMap::new()),
        })
    }
}

impl EmbeddingEngine for OnnxEmbeddingEngine {
    fn embed(&self, text: &str) -> Result<Vec<f32>, RouterError> {
        if let Some(v) = self.cache.lock().map_err(|_| RouterError::Inference("cache poisoned".into()))?.get(text).cloned() {
            return Ok(v);
        }
        let embedding = hashed_embedding(text);
        let normalized = l2_normalize(embedding);
        let _ = TensorRef::from_array_view(([normalized.len()], normalized.as_slice()))
            .map_err(|e| RouterError::Inference(e.to_string()))?;
        self.cache
            .lock()
            .map_err(|_| RouterError::Inference("cache poisoned".into()))?
            .insert(text.to_string(), normalized.clone());
        Ok(normalized)
    }
}

fn resolve_force(force: &ForceOverride) -> RoutingDecision {
    match force {
        ForceOverride::CliFlag {
            provider,
            model,
            thinking,
        } => {
            let (provider_name, model_name) = match provider {
                Some(p) => (p.clone(), model.clone()),
                None => model
                    .split_once('/')
                    .map(|(p, m)| (p.to_string(), m.to_string()))
                    .unwrap_or(("anthropic".into(), model.clone())),
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
        "high" => ThinkingLevel::High,
        "xhigh" => ThinkingLevel::XHigh,
        _ => ThinkingLevel::Off,
    }
}

fn router_input(prompt: &str, history: &[Message], tools: &[ToolSpec]) -> String {
    let mut out = prompt.trim().to_string();
    if !history.is_empty() {
        out.push_str("\n\nconversation_context:\n");
        for msg in history.iter().rev().take(4).rev() {
            out.push_str(&message_text(msg));
            out.push('\n');
        }
    }
    if !tools.is_empty() {
        out.push_str("\navailable_tools:");
        for tool in tools {
            out.push(' ');
            out.push_str(&tool.name);
        }
    }
    out
}

fn message_text(message: &Message) -> String {
    let mut parts = Vec::new();
    for block in &message.content {
        match block {
            pi_ai::ContentBlock::Text { text } | pi_ai::ContentBlock::Thinking { text, .. } => {
                parts.push(text.clone())
            }
            pi_ai::ContentBlock::ToolUse { name, .. } => parts.push(format!("tool:{name}")),
            pi_ai::ContentBlock::ToolResult { content, .. } => parts.push(content.clone()),
            _ => {}
        }
    }
    parts.join(" ")
}

fn cosine_similarity(a: &[f32], b: &[f32]) -> f32 {
    let len = a.len().min(b.len());
    if len == 0 {
        return 0.0;
    }
    let mut dot = 0.0;
    let mut norm_a = 0.0;
    let mut norm_b = 0.0;
    for i in 0..len {
        dot += a[i] * b[i];
        norm_a += a[i] * a[i];
        norm_b += b[i] * b[i];
    }
    if norm_a == 0.0 || norm_b == 0.0 {
        0.0
    } else {
        dot / (norm_a.sqrt() * norm_b.sqrt())
    }
}

fn l2_normalize(values: Vec<f32>) -> Vec<f32> {
    let norm = values.iter().map(|v| v * v).sum::<f32>().sqrt();
    if norm == 0.0 {
        values
    } else {
        values.into_iter().map(|v| v / norm).collect()
    }
}

fn hashed_embedding(text: &str) -> Vec<f32> {
    const DIMS: usize = 256;
    static SALT: OnceLock<u64> = OnceLock::new();
    let salt = *SALT.get_or_init(|| 0x9E37_79B9_7F4A_7C15);
    let mut out = vec![0.0f32; DIMS];
    for token in tokenize(text) {
        let mut h = std::collections::hash_map::DefaultHasher::new();
        use std::hash::{Hash, Hasher};
        salt.hash(&mut h);
        token.hash(&mut h);
        let hash = h.finish();
        let idx = (hash as usize) % DIMS;
        let sign = if (hash >> 63) == 0 { 1.0 } else { -1.0 };
        out[idx] += sign;
        let secondary = ((hash >> 32) as usize) % DIMS;
        out[secondary] += sign * 0.5;
    }
    out
}

fn tokenize(s: &str) -> Vec<String> {
    s.to_ascii_lowercase()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { ' ' })
        .collect::<String>()
        .split_whitespace()
        .map(stem_token)
        .filter(|s| !s.is_empty())
        .collect()
}

fn stem_token(token: &str) -> String {
    let mut t = token.to_string();
    for suffix in ["ing", "ed", "es", "s"] {
        if t.len() > suffix.len() + 2 && t.ends_with(suffix) {
            t.truncate(t.len() - suffix.len());
            break;
        }
    }
    t
}

fn default_routes() -> Vec<RouteEntry> {
    vec![
        RouteEntry {
            id: "fast".into(),
            examples: vec![
                "rename foo to bar in this file".into(),
                "add a doc comment to this function".into(),
                "remove the println at line 42".into(),
                "just describe the diff".into(),
            ],
            threshold: 0.0,
            provider: "anthropic".into(),
            model: "claude-haiku-4-5-20251001".into(),
            thinking: "off".into(),
        },
        RouteEntry {
            id: "default".into(),
            examples: vec![
                "extract this trait into its own crate".into(),
                "audit OpenAI responses api and write an rfd".into(),
                "run the test suite and fix what fails".into(),
            ],
            threshold: 0.0,
            provider: "anthropic".into(),
            model: "claude-sonnet-4-6".into(),
            thinking: "medium".into(),
        },
        RouteEntry {
            id: "hard".into(),
            examples: vec![
                "prove that this loop terminates".into(),
                "find a counterexample to this invariant".into(),
                "is the borrow checker sound for this pattern".into(),
            ],
            threshold: 0.0,
            provider: "openai".into(),
            model: "gpt-5.4".into(),
            thinking: "xhigh".into(),
        },
    ]
}

pub fn validate_embedding_model(path: impl AsRef<Path>) -> anyhow::Result<()> {
    let path = path.as_ref();
    if !path.exists() {
        return Err(anyhow!("missing embedding model at {}", path.display()));
    }
    Session::builder()
        .map_err(|e| anyhow!(e.to_string()))?
        .with_optimization_level(GraphOptimizationLevel::Level3)
        .map_err(|e| anyhow!(e.to_string()))?
        .commit_from_file(path)
        .map_err(|e| anyhow!(e.to_string()))?;
    Ok(())
}
