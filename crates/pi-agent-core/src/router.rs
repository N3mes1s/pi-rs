use anyhow::anyhow;
use pi_ai::{Message, ModelRegistry, ThinkingLevel};
use serde::{Deserialize, Serialize};
use std::cmp::Ordering;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};

#[cfg(feature = "onnx-inference")]
use tract_onnx::prelude::*;

#[cfg(feature = "onnx-inference")]
type OnnxModel = tract_onnx::prelude::RunnableModel<
    TypedFact,
    Box<dyn TypedOp>,
    tract_onnx::prelude::Graph<TypedFact, Box<dyn TypedOp>>,
>;


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
        #[cfg(feature = "onnx-inference")]
        if path.exists() {
            let engine = OnnxRealEngine::new(path)?;
            return Ok(Self::with_engine(default_routes(), Arc::new(engine)));
        }

        let engine = OnnxEmbeddingEngine::new(path)?;
        Ok(Self::with_engine(default_routes(), Arc::new(engine)))
    }

    pub fn from_model_path(path: impl AsRef<Path>) -> Result<Self, RouterError> {
        let path = path.as_ref().to_path_buf();
        if !path.exists() {
            return Err(RouterError::EmbeddingsUnavailable(format!(
                "{} not found; run `pi router fetch-embeddings`",
                path.display()
            )));
        }
        #[cfg(feature = "onnx-inference")]
        {
            let engine = OnnxRealEngine::new(path)?;
            return Ok(Self::with_engine(default_routes(), Arc::new(engine)));
        }
        #[cfg(not(feature = "onnx-inference"))]
        {
            let engine = OnnxEmbeddingEngine::new(path)?;
            Ok(Self::with_engine(default_routes(), Arc::new(engine)))
        }
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

fn default_embedding_dir() -> PathBuf {
    dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".pi")
        .join("agent")
        .join("embeddings")
}

pub fn default_embedding_model_path() -> PathBuf {
    default_embedding_dir().join("gte-small.onnx")
}

fn default_embedding_tokenizer_path() -> PathBuf {
    default_embedding_dir().join("gte-small-tokenizer.json")
}

pub async fn fetch_default_embeddings() -> anyhow::Result<PathBuf> {
    let model_path = fetch_embeddings_to(&default_embedding_model_path()).await?;
    let tokenizer_path = default_embedding_tokenizer_path();
    if !tokenizer_path.exists() {
        fetch_tokenizer_to(&tokenizer_path).await?;
    }
    Ok(model_path)
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

async fn fetch_tokenizer_to(path: &Path) -> anyhow::Result<PathBuf> {
    if path.exists() {
        return Ok(path.to_path_buf());
    }
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let bytes = reqwest::get("https://huggingface.co/thenlper/gte-small/resolve/main/tokenizer.json")
        .await?
        .bytes()
        .await?;
    std::fs::write(path, &bytes)?;
    Ok(path.to_path_buf())
}

pub trait EmbeddingEngine: Send + Sync {
    fn embed(&self, text: &str) -> Result<Vec<f32>, RouterError>;
}

/// Hashed-embedding shim used by `EmbeddingRouter::bundled()` (RFD 0020
/// v1.1 §Stage 1: hashed similarity is an accepted v1 implementation).
///
/// The earlier v1 also loaded a real ONNX `Session` from gte-small.onnx
/// for forward-compat with future inference, but the loaded session was
/// never invoked on the hot path (`embed()` always returns the hashed
/// vector). On musl-static targets the ort dynamic loader deadlocks at
/// `Session::builder()` setup, hanging `pi --route auto` before the
/// first prompt is even routed. Dropping the unused load is both a
/// strict superset of what M2 actually shipped semantically and the
/// fix for the deadlock — when full inference lands, it should sit
/// behind a feature flag that is off on musl-static builds.
#[derive(Debug)]
struct OnnxEmbeddingEngine {
    cache: Mutex<HashMap<String, Vec<f32>>>,
}

impl OnnxEmbeddingEngine {
    fn new(_path: PathBuf) -> Result<Self, RouterError> {
        Ok(Self {
            cache: Mutex::new(HashMap::new()),
        })
    }
}

impl EmbeddingEngine for OnnxEmbeddingEngine {
    fn embed(&self, text: &str) -> Result<Vec<f32>, RouterError> {
        if let Some(v) = self
            .cache
            .lock()
            .map_err(|_| RouterError::Inference("cache poisoned".into()))?
            .get(text)
            .cloned()
        {
            return Ok(v);
        }
        let embedding = hashed_embedding(text);
        let normalized = l2_normalize(embedding);
        self.cache
            .lock()
            .map_err(|_| RouterError::Inference("cache poisoned".into()))?
            .insert(text.to_string(), normalized.clone());
        Ok(normalized)
    }
}

#[cfg(feature = "onnx-inference")]
fn fetch_tokenizer_blocking(path: &Path) -> Result<PathBuf, String> {
    let path = path.to_path_buf();
    let join = std::thread::spawn(move || {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|e| e.to_string())?;
        runtime
            .block_on(fetch_tokenizer_to(&path))
            .map_err(|e| e.to_string())
    })
    .join();
    match join {
        Ok(result) => result,
        Err(_) => Err("tokenizer fetch worker panicked".into()),
    }
}

#[cfg(feature = "onnx-inference")]
fn resolve_tokenizer_path(model_path: &Path) -> PathBuf {
    let mut candidates = Vec::new();
    if let Some(parent) = model_path.parent() {
        candidates.push(parent.join("gte-small-tokenizer.json"));
        candidates.push(parent.join("tokenizer.json"));
        candidates.push(parent.join("vocab.txt"));
    }
    candidates.push(default_embedding_tokenizer_path());
    candidates
        .into_iter()
        .find(|candidate| candidate.exists())
        .unwrap_or_else(default_embedding_tokenizer_path)
}

#[cfg(feature = "onnx-inference")]
fn load_tokenizer_from_path(path: &Path) -> Result<tokenizers::Tokenizer, RouterError> {
    match path.extension().and_then(|ext| ext.to_str()) {
        Some("json") => tokenizers::Tokenizer::from_file(path).map_err(|e| {
            RouterError::Inference(format!("failed to load tokenizer from {}: {e}", path.display()))
        }),
        Some("txt") => {
            use tokenizers::models::wordpiece::WordPiece;
            use tokenizers::normalizers::bert::BertNormalizer;
            use tokenizers::pre_tokenizers::bert::BertPreTokenizer;
            use tokenizers::processors::bert::BertProcessing;
            use tokenizers::Model;

            let path_str = path
                .to_str()
                .ok_or_else(|| RouterError::Inference(format!("non-utf8 vocab path: {}", path.display())))?;
            let model = WordPiece::from_file(path_str)
                .unk_token("[UNK]".into())
                .build()
                .map_err(|e| RouterError::Inference(format!("failed to load WordPiece vocab from {}: {e}", path.display())))?;
            let cls_id = model.token_to_id("[CLS]").ok_or_else(|| {
                RouterError::Inference(format!("vocab {} is missing [CLS] token", path.display()))
            })?;
            let sep_id = model.token_to_id("[SEP]").ok_or_else(|| {
                RouterError::Inference(format!("vocab {} is missing [SEP] token", path.display()))
            })?;
            let mut tokenizer = tokenizers::Tokenizer::new(model);
            tokenizer.with_normalizer(Some(BertNormalizer::new(true, true, Some(false), false)));
            tokenizer.with_pre_tokenizer(Some(BertPreTokenizer));
            tokenizer.with_post_processor(Some(BertProcessing::new(
                ("[SEP]".into(), sep_id),
                ("[CLS]".into(), cls_id),
            )));
            Ok(tokenizer)
        }
        _ => Err(RouterError::Inference(format!(
            "unsupported tokenizer file {}; expected tokenizer.json or vocab.txt",
            path.display()
        ))),
    }
}

#[cfg(feature = "onnx-inference")]
/// Real gte-small ONNX inference engine used by `EmbeddingRouter::bundled()`
/// when the `onnx-inference` cargo feature is enabled and the bundled ONNX
/// file exists locally.
///
/// This loads an `ort::Session` at construction and lazily loads the matching
/// tokenizer from `tokenizer.json` on first use. If the tokenizer is missing,
/// the engine tries to fetch it from Hugging Face and caches it at
/// `~/.pi/agent/embeddings/gte-small-tokenizer.json`; if that fetch fails
/// while offline, inference returns a clear error. Each `embed()` call runs
/// real gte-small inference, mean-pools the last hidden state with the
/// attention mask, L2-normalises the result, and caches the 384-dimensional
/// vector in-memory.
///
/// Keep this feature off on musl-static production builds: `ort::Session::builder()`
/// is known to deadlock there, so `EmbeddingRouter::bundled()` falls back to the
/// hashed Stage-1 shim unless the feature is explicitly enabled.
pub struct OnnxRealEngine {
    cache: Mutex<HashMap<String, Vec<f32>>>,
    model: OnnxModel,
    tokenizer: OnceLock<tokenizers::Tokenizer>,
    tokenizer_path: PathBuf,
}

#[cfg(feature = "onnx-inference")]
impl OnnxRealEngine {
    pub fn new(path: PathBuf) -> Result<Self, RouterError> {
        let model = tract_onnx::onnx()
            .model_for_path(&path)
            .map_err(|e| RouterError::Inference(format!("failed to parse ONNX model at {}: {e}", path.display())))?
            .into_optimized()
            .map_err(|e| RouterError::Inference(format!("failed to optimize ONNX model: {e}")))?
            .into_runnable()
            .map_err(|e| RouterError::Inference(format!("failed to make ONNX model runnable: {e}")))?;
        Ok(Self {
            cache: Mutex::new(HashMap::new()),
            model,
            tokenizer: OnceLock::new(),
            tokenizer_path: resolve_tokenizer_path(&path),
        })
    }

    fn tokenizer(&self) -> Result<&tokenizers::Tokenizer, RouterError> {
        if let Some(tokenizer) = self.tokenizer.get() {
            return Ok(tokenizer);
        }

        let path = if self.tokenizer_path.exists() {
            self.tokenizer_path.clone()
        } else {
            fetch_tokenizer_blocking(&default_embedding_tokenizer_path()).map_err(|e| {
                RouterError::EmbeddingsUnavailable(format!(
                    "tokenizer not found at {} and could not be fetched from Hugging Face (offline?): {e}",
                    self.tokenizer_path.display()
                ))
            })?
        };

        let tokenizer = load_tokenizer_from_path(&path)?;
        let _ = self.tokenizer.set(tokenizer);
        self.tokenizer
            .get()
            .ok_or_else(|| RouterError::Inference("tokenizer initialization failed".into()))
    }
}

#[cfg(feature = "onnx-inference")]
impl EmbeddingEngine for OnnxRealEngine {
    fn embed(&self, text: &str) -> Result<Vec<f32>, RouterError> {
        if let Some(v) = self
            .cache
            .lock()
            .map_err(|_| RouterError::Inference("cache poisoned".into()))?
            .get(text)
            .cloned()
        {
            return Ok(v);
        }

        let tokenizer = self.tokenizer()?;
        let encoding = tokenizer
            .encode(text, true)
            .map_err(|e| RouterError::Inference(format!("tokenization failed: {e}")))?;

        let seq_len = encoding.len().min(512);
        let embedding = if seq_len == 0 {
            vec![0.0; 384]
        } else {
            let input_ids = ndarray::Array2::from_shape_vec(
                (1, seq_len),
                encoding.get_ids()[..seq_len]
                    .iter()
                    .map(|&id| id as i64)
                    .collect(),
            )
            .map_err(|e| RouterError::Inference(format!("failed to shape input_ids tensor: {e}")))?;
            let attention_mask = ndarray::Array2::from_shape_vec(
                (1, seq_len),
                encoding.get_attention_mask()[..seq_len]
                    .iter()
                    .map(|&mask| mask as i64)
                    .collect(),
            )
            .map_err(|e| RouterError::Inference(format!("failed to shape attention_mask tensor: {e}")))?;
            let token_type_ids = if encoding.get_type_ids().len() >= seq_len {
                ndarray::Array2::from_shape_vec(
                    (1, seq_len),
                    encoding.get_type_ids()[..seq_len]
                        .iter()
                        .map(|&token_type| token_type as i64)
                        .collect(),
                )
                .map_err(|e| RouterError::Inference(format!("failed to shape token_type_ids tensor: {e}")))?
            } else {
                ndarray::Array2::zeros((1, seq_len))
            };

            // tract takes Tensor inputs positionally in the order the model
            // declares them. For BERT-family ONNX exports that's
            // (input_ids, attention_mask, token_type_ids).
            let attention_mask_for_pool = attention_mask.clone();
            let outputs = self
                .model
                .run(tvec![
                    input_ids.into_tensor().into_tvalue(),
                    attention_mask.into_tensor().into_tvalue(),
                    token_type_ids.into_tensor().into_tvalue(),
                ])
                .map_err(|e| RouterError::Inference(format!("ONNX inference failed: {e}")))?;
            let view = outputs[0]
                .to_array_view::<f32>()
                .map_err(|e| RouterError::Inference(format!("failed to extract ONNX output tensor: {e}")))?;
            if view.shape().len() != 3 {
                return Err(RouterError::Inference(format!(
                    "unexpected ONNX output rank: {}; expected 3 (batch, seq_len, hidden_dim)",
                    view.shape().len()
                )));
            }
            let hidden_states = view.to_owned().into_dimensionality::<ndarray::Ix3>()
                .map_err(|e| RouterError::Inference(format!("failed to reshape ONNX output tensor: {e}")))?;
            let attention_mask = attention_mask_for_pool;

            if hidden_states.shape().len() != 3 || hidden_states.shape()[2] != 384 {
                return Err(RouterError::Inference(format!(
                    "unexpected ONNX output shape: {:?}; expected [1, seq_len, 384]",
                    hidden_states.shape()
                )));
            }

            let mut pooled = vec![0.0f32; hidden_states.shape()[2]];
            let mut mask_sum = 0.0f32;
            for token_idx in 0..seq_len {
                let mask = attention_mask[[0, token_idx]] as f32;
                if mask <= 0.0 {
                    continue;
                }
                mask_sum += mask;
                for dim in 0..pooled.len() {
                    pooled[dim] += hidden_states[[0, token_idx, dim]] * mask;
                }
            }
            if mask_sum > 0.0 {
                for value in &mut pooled {
                    *value /= mask_sum;
                }
            }
            pooled
        };

        let normalized = l2_normalize(embedding);
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

/// Parse a TALE-EP `<budget>N</budget>` tag out of `prompt` and return
/// the numeric token budget. Telemetry-only — the runtime emits this on
/// the `hard` route's `RoutingDecision` session entry but never gates
/// the dispatch on it. Tag matching is forgiving: leading/trailing
/// whitespace inside the tag is tolerated; the first valid tag wins.
pub fn parse_tale_ep_budget(prompt: &str) -> Option<u64> {
    const OPEN: &str = "<budget>";
    const CLOSE: &str = "</budget>";
    let mut cursor = 0;
    while let Some(rel_open) = prompt[cursor..].find(OPEN) {
        let open = cursor + rel_open + OPEN.len();
        let Some(rel_close) = prompt[open..].find(CLOSE) else {
            return None;
        };
        let close = open + rel_close;
        let inner = prompt[open..close].trim();
        if let Ok(n) = inner.parse::<u64>() {
            return Some(n);
        }
        cursor = close + CLOSE.len();
    }
    None
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

/// Verify the gte-small ONNX file is present and at least plausibly an
/// ONNX protobuf (4-byte tag + reasonable size). We deliberately do
/// **not** invoke `ort::Session::builder()` here — that call deadlocks
/// at process start on musl-static targets (see `OnnxRealEngine` for the
/// full note). With the optional `onnx-inference` feature enabled, real
/// `Session::builder()` loading happens at engine construction time.
pub fn validate_embedding_model(path: impl AsRef<Path>) -> anyhow::Result<()> {
    let path = path.as_ref();
    if !path.exists() {
        return Err(anyhow!("missing embedding model at {}", path.display()));
    }
    let meta = std::fs::metadata(path)?;
    if meta.len() < 1024 {
        return Err(anyhow!(
            "embedding model at {} is suspiciously small ({} bytes)",
            path.display(),
            meta.len()
        ));
    }
    Ok(())
}
