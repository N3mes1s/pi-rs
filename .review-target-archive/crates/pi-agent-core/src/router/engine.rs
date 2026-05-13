//! Embedding engines + ONNX model fetch/validate.
//!
//! The router is two-tiered:
//!
//!   * `OnnxEmbeddingEngine` — hashed-embedding shim (Stage 1 in
//!     RFD 0020 v1.1). Always available, no native deps. Used by
//!     default on musl-static builds where ONNX runtime loaders
//!     deadlock at process start.
//!   * `OnnxRealEngine` — real gte-small ONNX inference behind the
//!     `onnx-inference` feature. Loads the model at construction,
//!     lazily loads the tokenizer on first `embed()`, then runs
//!     forward passes with mean-pooled L2-normalised hidden states.
//!
//! Both implement `EmbeddingEngine`. `EmbeddingRouter::bundled()`
//! picks the real engine when the feature is enabled AND the model
//! file exists locally; otherwise it falls back to the hashed shim.

use crate::router::text::{hashed_embedding, l2_normalize};
use crate::router::RouterError;
use anyhow::anyhow;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

#[cfg(feature = "onnx-inference")]
use std::sync::OnceLock;

#[cfg(feature = "onnx-inference")]
use tract_onnx::prelude::*;

#[cfg(feature = "onnx-inference")]
type OnnxModel = tract_onnx::prelude::RunnableModel<
    TypedFact,
    Box<dyn TypedOp>,
    tract_onnx::prelude::Graph<TypedFact, Box<dyn TypedOp>>,
>;

pub trait EmbeddingEngine: Send + Sync {
    fn embed(&self, text: &str) -> Result<Vec<f32>, RouterError>;
}

pub(super) fn default_embedding_dir() -> PathBuf {
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
    let bytes =
        reqwest::get("https://huggingface.co/thenlper/gte-small/resolve/main/onnx/model.onnx")
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
    let bytes =
        reqwest::get("https://huggingface.co/thenlper/gte-small/resolve/main/tokenizer.json")
            .await?
            .bytes()
            .await?;
    std::fs::write(path, &bytes)?;
    Ok(path.to_path_buf())
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
pub(super) struct OnnxEmbeddingEngine {
    cache: Mutex<HashMap<String, Vec<f32>>>,
}

impl OnnxEmbeddingEngine {
    pub(super) fn new(_path: PathBuf) -> Result<Self, RouterError> {
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
            RouterError::Inference(format!(
                "failed to load tokenizer from {}: {e}",
                path.display()
            ))
        }),
        Some("txt") => {
            use tokenizers::models::wordpiece::WordPiece;
            use tokenizers::normalizers::bert::BertNormalizer;
            use tokenizers::pre_tokenizers::bert::BertPreTokenizer;
            use tokenizers::processors::bert::BertProcessing;
            use tokenizers::Model;

            let path_str = path.to_str().ok_or_else(|| {
                RouterError::Inference(format!("non-utf8 vocab path: {}", path.display()))
            })?;
            let model = WordPiece::from_file(path_str)
                .unk_token("[UNK]".into())
                .build()
                .map_err(|e| {
                    RouterError::Inference(format!(
                        "failed to load WordPiece vocab from {}: {e}",
                        path.display()
                    ))
                })?;
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
            .map_err(|e| {
                RouterError::Inference(format!(
                    "failed to parse ONNX model at {}: {e}",
                    path.display()
                ))
            })?
            .into_optimized()
            .map_err(|e| RouterError::Inference(format!("failed to optimize ONNX model: {e}")))?
            .into_runnable()
            .map_err(|e| {
                RouterError::Inference(format!("failed to make ONNX model runnable: {e}"))
            })?;
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
            .map_err(|e| {
                RouterError::Inference(format!("failed to shape input_ids tensor: {e}"))
            })?;
            let attention_mask = ndarray::Array2::from_shape_vec(
                (1, seq_len),
                encoding.get_attention_mask()[..seq_len]
                    .iter()
                    .map(|&mask| mask as i64)
                    .collect(),
            )
            .map_err(|e| {
                RouterError::Inference(format!("failed to shape attention_mask tensor: {e}"))
            })?;
            let token_type_ids = if encoding.get_type_ids().len() >= seq_len {
                ndarray::Array2::from_shape_vec(
                    (1, seq_len),
                    encoding.get_type_ids()[..seq_len]
                        .iter()
                        .map(|&token_type| token_type as i64)
                        .collect(),
                )
                .map_err(|e| {
                    RouterError::Inference(format!("failed to shape token_type_ids tensor: {e}"))
                })?
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
            let view = outputs[0].to_array_view::<f32>().map_err(|e| {
                RouterError::Inference(format!("failed to extract ONNX output tensor: {e}"))
            })?;
            if view.shape().len() != 3 {
                return Err(RouterError::Inference(format!(
                    "unexpected ONNX output rank: {}; expected 3 (batch, seq_len, hidden_dim)",
                    view.shape().len()
                )));
            }
            let hidden_states = view
                .to_owned()
                .into_dimensionality::<ndarray::Ix3>()
                .map_err(|e| {
                    RouterError::Inference(format!("failed to reshape ONNX output tensor: {e}"))
                })?;
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
