//! Real BPE tokenizer for token-count estimation (RFD 0014).
//!
//! Replaces the bytes/4 heuristic from RFD 0012. Uses `tiktoken-rs`'s
//! baked-in `cl100k_base` (GPT-4 / Claude proxy) and `o200k_base`
//! (GPT-4o / o-series / gpt-5) BPE merges. The init is one-time per
//! process via [`OnceLock`]; subsequent `count()` calls are cheap.
//!
//! If the BPE init fails (e.g. an offline build environment that
//! couldn't bake in the merges), we fall back to `s.len() / 4` so the
//! caller always gets a non-panicking estimate.

use std::sync::OnceLock;

use tiktoken_rs::CoreBPE;

/// Tokenizer family. We curate two: `Cl100kBase` covers GPT-4 +
/// approximation for Claude; `O200kBase` is the GPT-4o /
/// reasoning-model encoding. Anthropic-specific tokenizers aren't
/// open-source; cl100k is the closest practical proxy and is what
/// Anthropic's own cookbook examples use for offline estimation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TokenizerKind {
    Cl100kBase,
    O200kBase,
}

impl TokenizerKind {
    /// Pick a tokenizer family based on a model id. Modern OpenAI
    /// models (gpt-4o, o1/o3/o4, gpt-5) use `o200k_base`; everything
    /// else (including Claude) falls back to `cl100k_base`.
    pub fn for_model(model_id: &str) -> Self {
        if model_id.starts_with("gpt-4o")
            || model_id.starts_with("o1")
            || model_id.starts_with("o3")
            || model_id.starts_with("o4")
            || model_id.starts_with("gpt-5")
        {
            Self::O200kBase
        } else {
            Self::Cl100kBase
        }
    }
}

fn cl100k() -> Option<&'static CoreBPE> {
    static CL100K: OnceLock<Option<CoreBPE>> = OnceLock::new();
    CL100K
        .get_or_init(|| tiktoken_rs::cl100k_base().ok())
        .as_ref()
}

fn o200k() -> Option<&'static CoreBPE> {
    static O200K: OnceLock<Option<CoreBPE>> = OnceLock::new();
    O200K
        .get_or_init(|| tiktoken_rs::o200k_base().ok())
        .as_ref()
}

/// Count tokens with the given BPE family. Falls back to `s.len() / 4`
/// when the encoder can't be initialised (offline test environments).
pub fn count(kind: TokenizerKind, s: &str) -> u64 {
    let bpe = match kind {
        TokenizerKind::Cl100kBase => cl100k(),
        TokenizerKind::O200kBase => o200k(),
    };
    match bpe {
        Some(bpe) => bpe.encode_with_special_tokens(s).len() as u64,
        None => (s.len() as u64) / 4,
    }
}

/// Cheap default when no concrete model is in scope. Used by
/// `ContextLoad.tokens` when the RuntimeConfig hasn't resolved the
/// model yet. Uses `cl100k_base`.
pub fn count_default(s: &str) -> u64 {
    count(TokenizerKind::Cl100kBase, s)
}
