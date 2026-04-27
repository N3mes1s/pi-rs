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
    }
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
                m("anthropic", "claude-opus-4-7", Some("opus"), 200_000, 32_000, true, true, 15.0, 75.0),
                m("anthropic", "claude-sonnet-4-6", Some("sonnet"), 1_000_000, 64_000, true, true, 3.0, 15.0),
                m("anthropic", "claude-haiku-4-5-20251001", Some("haiku"), 200_000, 16_000, true, true, 0.8, 4.0),
            ],
        },
        ProviderConfig {
            name: "openai".into(),
            kind: ProviderKind::OpenAi,
            base_url: "https://api.openai.com/v1".into(),
            auth_header: "Authorization".into(),
            auth_format: "Bearer {token}".into(),
            models: vec![
                m("openai", "gpt-4o", Some("gpt-4o"), 128_000, 16_384, false, true, 2.5, 10.0),
                m("openai", "gpt-4o-mini", Some("gpt-4o-mini"), 128_000, 16_384, false, true, 0.15, 0.60),
                m("openai", "o1", Some("o1"), 200_000, 100_000, true, true, 15.0, 60.0),
                m("openai", "o1-mini", Some("o1-mini"), 128_000, 65_536, true, false, 3.0, 12.0),
                m("openai", "o3-mini", Some("o3-mini"), 200_000, 100_000, true, false, 1.10, 4.40),
            ],
        },
        ProviderConfig {
            name: "google".into(),
            kind: ProviderKind::Google,
            base_url: "https://generativelanguage.googleapis.com".into(),
            auth_header: "x-goog-api-key".into(),
            auth_format: "{token}".into(),
            models: vec![
                m("google", "gemini-2.5-pro", Some("gemini-pro"), 1_000_000, 8_192, true, true, 1.25, 5.0),
                m("google", "gemini-2.5-flash", Some("gemini"), 1_000_000, 8_192, false, true, 0.075, 0.30),
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
                m(
                    "bedrock",
                    "anthropic.claude-opus-4-7",
                    Some("bedrock-opus"),
                    200_000,
                    32_000,
                    true,
                    true,
                    15.0,
                    75.0,
                ),
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
                m(
                    "bedrock",
                    "anthropic.claude-haiku-4-5",
                    Some("bedrock-haiku"),
                    200_000,
                    16_000,
                    true,
                    true,
                    0.8,
                    4.0,
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
    ]
}
