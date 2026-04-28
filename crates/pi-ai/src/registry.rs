use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

use crate::auth::AuthStorage;
use crate::provider::ProviderKind;

/// Information about a single model offered by a provider.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelInfo {
    pub provider: String,
    pub id: String,
    /// Stable short alias, e.g. `sonnet`, `haiku`, `gpt-4o`.
    pub alias: Option<String>,
    pub context_window: u32,
    pub max_output_tokens: u32,
    pub supports_thinking: bool,
    pub supports_tools: bool,
    pub supports_vision: bool,
    pub input_cost_per_mtok: f64,
    pub output_cost_per_mtok: f64,
    /// Per-million tokens for `cache_read_input_tokens`. Falls back
    /// to `input_cost_per_mtok` when `None`. RFD 0010.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache_read_cost_per_mtok: Option<f64>,
    /// Per-million tokens for `cache_creation_input_tokens` (a.k.a.
    /// "cache write"). Falls back to `input_cost_per_mtok` when
    /// `None`. RFD 0010.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache_write_cost_per_mtok: Option<f64>,
}

/// Configuration for a single provider.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProviderConfig {
    pub name: String,
    pub kind: ProviderKind,
    pub base_url: String,
    /// Header name for the API key (e.g. `x-api-key`, `Authorization`).
    pub auth_header: String,
    /// Format string for the auth header value, with `{token}` placeholder.
    pub auth_format: String,
    pub models: Vec<ModelInfo>,
}

/// Central registry of providers and their models. Comparable to
/// `ModelRegistry.create(authStorage)` in upstream pi.
#[derive(Debug, Clone)]
pub struct ModelRegistry {
    providers: BTreeMap<String, ProviderConfig>,
    auth: AuthStorage,
}

impl ModelRegistry {
    pub fn new(auth: AuthStorage) -> Self {
        let mut me = Self {
            providers: BTreeMap::new(),
            auth,
        };
        me.install_defaults();
        me
    }

    fn install_defaults(&mut self) {
        let defaults = default_providers();
        for p in defaults {
            self.providers.insert(p.name.clone(), p);
        }
    }

    pub fn auth(&self) -> &AuthStorage {
        &self.auth
    }

    pub fn providers(&self) -> impl Iterator<Item = &ProviderConfig> {
        self.providers.values()
    }

    pub fn get(&self, provider: &str) -> Option<&ProviderConfig> {
        self.providers.get(provider)
    }

    /// Look up a model by `provider/id` or by alias across all providers.
    pub fn resolve(&self, target: &str) -> Option<(&ProviderConfig, &ModelInfo)> {
        if let Some((p, m)) = target.split_once('/') {
            let provider = self.providers.get(p)?;
            let model = provider.models.iter().find(|m2| m2.id == m || m2.alias.as_deref() == Some(m))?;
            return Some((provider, model));
        }
        for provider in self.providers.values() {
            if let Some(m) = provider
                .models
                .iter()
                .find(|m| m.id == target || m.alias.as_deref() == Some(target))
            {
                return Some((provider, m));
            }
        }
        None
    }

    pub fn install(&mut self, config: ProviderConfig) {
        self.providers.insert(config.name.clone(), config);
    }

    /// Merge live-discovered models into the registry. Existing static
    /// entries (curated cost / context_window / alias data) WIN over
    /// discovered ones with the same id; new ids are appended.
    pub fn merge_discovered(&mut self, discovered: Vec<ModelInfo>) {
        for m in discovered {
            let Some(provider) = self.providers.get_mut(&m.provider) else {
                continue;
            };
            if provider.models.iter().any(|existing| existing.id == m.id) {
                continue;
            }
            provider.models.push(m);
        }
    }

    /// Number of models registered across all providers.
    pub fn total_models(&self) -> usize {
        self.providers.values().map(|p| p.models.len()).sum()
    }
}

fn m(
    provider: &str,
    id: &str,
    alias: Option<&str>,
    ctx: u32,
    out: u32,
    thinking: bool,
    vision: bool,
    in_cost: f64,
    out_cost: f64,
) -> ModelInfo {
    ModelInfo {
        provider: provider.into(),
        id: id.into(),
        alias: alias.map(|s| s.to_string()),
        context_window: ctx,
        max_output_tokens: out,
        supports_thinking: thinking,
        supports_tools: true,
        supports_vision: vision,
        input_cost_per_mtok: in_cost,
        output_cost_per_mtok: out_cost,
        cache_read_cost_per_mtok: None,
        cache_write_cost_per_mtok: None,
    }
}

/// Wrap an `m(...)` row with explicit cache-rate overrides (RFD 0010).
fn with_cache(mut model: ModelInfo, read: Option<f64>, write: Option<f64>) -> ModelInfo {
    model.cache_read_cost_per_mtok = read;
    model.cache_write_cost_per_mtok = write;
    model
}

pub(crate) fn default_providers() -> Vec<ProviderConfig> {
    vec![
        ProviderConfig {
            name: "anthropic".into(),
            kind: ProviderKind::Anthropic,
            base_url: "https://api.anthropic.com".into(),
            auth_header: "x-api-key".into(),
            auth_format: "{token}".into(),
            models: vec![
                with_cache(
                    m("anthropic", "claude-opus-4-7", Some("opus"), 200_000, 32_000, true, true, 5.0, 25.0),
                    Some(0.50),
                    Some(6.25),
                ),
                with_cache(
                    m("anthropic", "claude-sonnet-4-6", Some("sonnet"), 1_000_000, 64_000, true, true, 3.0, 15.0),
                    Some(0.30),
                    Some(3.75),
                ),
                with_cache(
                    m("anthropic", "claude-haiku-4-5-20251001", Some("haiku"), 200_000, 16_000, true, true, 1.0, 5.0),
                    Some(0.10),
                    Some(1.25),
                ),
            ],
        },
        ProviderConfig {
            name: "openai".into(),
            kind: ProviderKind::OpenAi,
            base_url: "https://api.openai.com/v1".into(),
            auth_header: "Authorization".into(),
            auth_format: "Bearer {token}".into(),
            models: vec![
                with_cache(
                    m("openai", "gpt-4o", Some("gpt-4o"), 128_000, 16_384, false, true, 2.5, 10.0),
                    Some(1.25),
                    None,
                ),
                with_cache(
                    m("openai", "gpt-4o-mini", Some("gpt-4o-mini"), 128_000, 16_384, false, true, 0.15, 0.60),
                    Some(0.075),
                    None,
                ),
                with_cache(
                    m("openai", "o1", Some("o1"), 200_000, 100_000, true, true, 15.0, 60.0),
                    Some(7.5),
                    None,
                ),
                m("openai", "o1-mini", Some("o1-mini"), 128_000, 65_536, true, false, 1.10, 4.40),
                m("openai", "o3-mini", Some("o3-mini"), 200_000, 100_000, true, false, 1.10, 4.40),
                // Reasoning family additions (E1).
                with_cache(
                    m("openai", "o3", Some("o3"), 200_000, 100_000, true, true, 2.0, 8.0),
                    Some(1.0),
                    None,
                ),
                m("openai", "o3-pro", Some("o3-pro"), 200_000, 100_000, true, true, 20.0, 80.0),
                with_cache(
                    m("openai", "o4-mini", Some("o4-mini"), 200_000, 100_000, true, true, 1.10, 4.40),
                    Some(0.55),
                    None,
                ),
                // GPT-5 family (reasoning-capable per OpenAI announcement).
                with_cache(
                    m("openai", "gpt-5", Some("gpt-5"), 400_000, 100_000, true, true, 1.25, 10.0),
                    Some(0.625),
                    None,
                ),
                with_cache(
                    m("openai", "gpt-5-mini", Some("gpt-5-mini"), 400_000, 100_000, true, true, 0.25, 2.0),
                    Some(0.125),
                    None,
                ),
                with_cache(
                    m("openai", "gpt-5-nano", Some("gpt-5-nano"), 400_000, 100_000, true, true, 0.05, 0.40),
                    Some(0.025),
                    None,
                ),
                // gpt-5.4 dot-release (mid-2026). Same 400k/100k window and
                // Chat-Completions endpoint as gpt-5; pricing tracks the gpt-5
                // base rate at launch (RFD 0009 -> evolve when OpenAI publishes
                // the rate card).
                with_cache(
                    m("openai", "gpt-5.4", Some("gpt-5.4"), 400_000, 100_000, true, true, 1.25, 10.0),
                    Some(0.625),
                    None,
                ),
            ],
        },
        ProviderConfig {
            name: "google".into(),
            kind: ProviderKind::Google,
            base_url: "https://generativelanguage.googleapis.com".into(),
            auth_header: "x-goog-api-key".into(),
            auth_format: "{token}".into(),
            models: vec![
                with_cache(
                    m("google", "gemini-2.5-pro", Some("gemini-pro"), 1_000_000, 8_192, true, true, 1.25, 10.0),
                    Some(0.3125),
                    None,
                ),
                with_cache(
                    m("google", "gemini-2.5-flash", Some("gemini"), 1_000_000, 8_192, false, true, 0.30, 2.50),
                    Some(0.075),
                    None,
                ),
            ],
        },
        ProviderConfig {
            name: "fireworks".into(),
            kind: ProviderKind::OpenAiCompat,
            base_url: "https://api.fireworks.ai/inference/v1".into(),
            auth_header: "Authorization".into(),
            auth_format: "Bearer {token}".into(),
            models: vec![
                m(
                    "fireworks",
                    "accounts/fireworks/models/llama-v3p3-70b-instruct",
                    Some("llama-3.3-70b"),
                    131_072,
                    16_384,
                    false,
                    false,
                    0.90,
                    0.90,
                ),
                m(
                    "fireworks",
                    "accounts/fireworks/models/qwen2p5-coder-32b-instruct",
                    Some("qwen-coder"),
                    32_768,
                    16_384,
                    false,
                    false,
                    0.90,
                    0.90,
                ),
                m(
                    "fireworks",
                    "accounts/fireworks/models/deepseek-r1",
                    Some("deepseek-r1"),
                    160_000,
                    32_000,
                    true,
                    false,
                    8.0,
                    8.0,
                ),
            ],
        },
        ProviderConfig {
            name: "bedrock".into(),
            kind: ProviderKind::Bedrock,
            base_url: "https://bedrock-runtime.us-east-1.amazonaws.com".into(),
            auth_header: "Authorization".into(),
            auth_format: "Bearer {token}".into(),
            models: vec![
                with_cache(
                    m(
                        "bedrock",
                        "anthropic.claude-opus-4-7",
                        Some("bedrock-opus"),
                        200_000,
                        32_000,
                        true,
                        true,
                        5.0,
                        25.0,
                    ),
                    Some(0.50),
                    Some(6.25),
                ),
                with_cache(
                    m(
                        "bedrock",
                        "anthropic.claude-sonnet-4-6",
                        Some("bedrock-sonnet"),
                        1_000_000,
                        64_000,
                        true,
                        true,
                        3.0,
                        15.0,
                    ),
                    Some(0.30),
                    Some(3.75),
                ),
                with_cache(
                    m(
                        "bedrock",
                        "anthropic.claude-haiku-4-5",
                        Some("bedrock-haiku"),
                        200_000,
                        16_000,
                        true,
                        true,
                        1.0,
                        5.0,
                    ),
                    Some(0.10),
                    Some(1.25),
                ),
            ],
        },
        ProviderConfig {
            name: "azure-openai".into(),
            kind: ProviderKind::Azure,
            base_url: "https://YOUR_RESOURCE.openai.azure.com".into(),
            auth_header: "api-key".into(),
            auth_format: "{token}".into(),
            // Users configure their own deployment names; no models are
            // pre-registered here.
            models: vec![],
        },
        ProviderConfig {
            name: "cerebras".into(),
            kind: ProviderKind::OpenAiCompat,
            base_url: "https://api.cerebras.ai/v1".into(),
            auth_header: "Authorization".into(),
            auth_format: "Bearer {token}".into(),
            models: vec![
                m("cerebras", "llama3.1-70b", Some("llama3.1-70b"), 131_072, 8_192, false, false, 0.60, 0.60),
            ],
        },
        ProviderConfig {
            name: "groq".into(),
            kind: ProviderKind::OpenAiCompat,
            base_url: "https://api.groq.com/openai/v1".into(),
            auth_header: "Authorization".into(),
            auth_format: "Bearer {token}".into(),
            models: vec![
                m("groq", "llama-3.3-70b-versatile", Some("llama-3.3-70b-versatile"), 131_072, 8_192, false, false, 0.59, 0.79),
            ],
        },
        ProviderConfig {
            name: "xai".into(),
            kind: ProviderKind::OpenAiCompat,
            base_url: "https://api.x.ai/v1".into(),
            auth_header: "Authorization".into(),
            auth_format: "Bearer {token}".into(),
            models: vec![
                m("xai", "grok-2-latest", Some("grok-2-latest"), 131_072, 8_192, false, false, 2.0, 10.0),
            ],
        },
        ProviderConfig {
            name: "openrouter".into(),
            kind: ProviderKind::OpenAiCompat,
            base_url: "https://openrouter.ai/api/v1".into(),
            auth_header: "Authorization".into(),
            auth_format: "Bearer {token}".into(),
            // Users pick their own models on OpenRouter.
            models: vec![],
        },
        ProviderConfig {
            name: "deepseek".into(),
            kind: ProviderKind::OpenAiCompat,
            base_url: "https://api.deepseek.com".into(),
            auth_header: "Authorization".into(),
            auth_format: "Bearer {token}".into(),
            models: vec![
                m("deepseek", "deepseek-chat", Some("deepseek-chat"), 131_072, 8_192, false, false, 0.14, 0.28),
                m("deepseek", "deepseek-reasoner", Some("deepseek-reasoner"), 131_072, 8_192, true, false, 0.14, 0.28),
            ],
        },
        ProviderConfig {
            name: "mistral".into(),
            kind: ProviderKind::OpenAiCompat,
            base_url: "https://api.mistral.ai/v1".into(),
            auth_header: "Authorization".into(),
            auth_format: "Bearer {token}".into(),
            models: vec![
                m("mistral", "mistral-large-latest", Some("mistral-large-latest"), 131_072, 8_192, false, false, 2.0, 6.0),
            ],
        },
        ProviderConfig {
            name: "zai".into(),
            kind: ProviderKind::OpenAiCompat,
            base_url: "https://api.z.ai/api/paas/v4".into(),
            auth_header: "Authorization".into(),
            auth_format: "Bearer {token}".into(),
            models: vec![
                m("zai", "glm-4.6", Some("glm-4.6"), 131_072, 8_192, false, false, 0.60, 2.20),
            ],
        },
        ProviderConfig {
            name: "huggingface".into(),
            kind: ProviderKind::OpenAiCompat,
            base_url: "https://api-inference.huggingface.co/v1".into(),
            auth_header: "Authorization".into(),
            auth_format: "Bearer {token}".into(),
            // Users pick their own models on HuggingFace.
            models: vec![],
        },
        ProviderConfig {
            name: "ollama".into(),
            kind: ProviderKind::OpenAiCompat,
            base_url: "http://localhost:11434/v1".into(),
            auth_header: "Authorization".into(),
            auth_format: "Bearer {token}".into(),
            // Users pick their own locally-installed models.
            models: vec![],
        },
        ProviderConfig {
            name: "kimi".into(),
            kind: ProviderKind::OpenAiCompat,
            base_url: "https://api.moonshot.cn/v1".into(),
            auth_header: "Authorization".into(),
            auth_format: "Bearer {token}".into(),
            models: vec![
                m("kimi", "moonshot-v1-128k", Some("moonshot-v1-128k"), 131_072, 8_192, false, false, 2.0, 5.0),
            ],
        },
        ProviderConfig {
            name: "minimax".into(),
            kind: ProviderKind::OpenAiCompat,
            base_url: "https://api.minimax.chat/v1".into(),
            auth_header: "Authorization".into(),
            auth_format: "Bearer {token}".into(),
            // Users pick their own models on MiniMax.
            models: vec![],
        },
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_model_has_non_zero_pricing() {
        for p in default_providers() {
            for m in &p.models {
                assert!(
                    m.input_cost_per_mtok > 0.0,
                    "{}/{} input zero",
                    p.name,
                    m.id
                );
                assert!(
                    m.output_cost_per_mtok > 0.0,
                    "{}/{} output zero",
                    p.name,
                    m.id
                );
            }
        }
    }

    #[test]
    fn no_row_uses_the_legacy_placeholder_pair() {
        for p in default_providers() {
            for m in &p.models {
                let placeholder = (m.input_cost_per_mtok - 0.5).abs() < 1e-9
                    && (m.output_cost_per_mtok - 1.5).abs() < 1e-9;
                assert!(
                    !placeholder,
                    "{}/{} still has the (0.5, 1.5) placeholder",
                    p.name, m.id
                );
            }
        }
    }
}
