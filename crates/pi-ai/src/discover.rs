//! Top-level live model-discovery orchestration.
//!
//! Walks every provider with credentials present, calls
//! `Provider::discover_models()`, and writes the merged catalogue to a
//! cache file the next pi-rs startup reads.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde::{Deserialize, Serialize};

use crate::auth::AuthStorage;
use crate::provider::{
    AnthropicProvider, AzureOpenAiProvider, BedrockAnthropicProvider, GoogleProvider,
    OpenAiCompatProvider, OpenAiProvider, Provider, ProviderKind,
};
use crate::registry::{ModelInfo, ModelRegistry};
use crate::AiError;

/// On-disk format for `discovered-models.json`. Keyed by provider name.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct DiscoveredCache {
    /// Unix-millis when this snapshot was taken.
    pub generated_at: i64,
    /// `provider_name -> [ModelInfo]`
    pub providers: std::collections::BTreeMap<String, Vec<ModelInfo>>,
}

impl DiscoveredCache {
    pub fn load(path: &Path) -> Self {
        std::fs::read_to_string(path)
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default()
    }

    pub fn save(&self, path: &Path) -> std::io::Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let json = serde_json::to_string_pretty(self).map_err(|e| {
            std::io::Error::new(std::io::ErrorKind::InvalidData, e)
        })?;
        std::fs::write(path, json)
    }

    /// Flatten into a single `Vec<ModelInfo>` for `ModelRegistry::merge_discovered`.
    pub fn flatten(&self) -> Vec<ModelInfo> {
        let mut out = Vec::new();
        for v in self.providers.values() {
            out.extend(v.iter().cloned());
        }
        out
    }
}

/// Outcome of discovery for a single provider.
#[derive(Debug, Clone)]
pub struct ProviderDiscovery {
    pub provider: String,
    pub result: std::result::Result<Vec<ModelInfo>, String>,
}

/// Run discovery for every provider in the registry that has credentials in
/// `auth`. Skips providers with `AuthMethod::None`. Returns one
/// [`ProviderDiscovery`] per attempted provider so the CLI can report
/// per-provider success / failure.
pub async fn refresh_all(
    registry: &ModelRegistry,
    auth: &AuthStorage,
) -> Vec<ProviderDiscovery> {
    let mut out = Vec::new();
    for cfg in registry.providers().cloned().collect::<Vec<_>>() {
        let auth_method = match auth.get(&cfg.name) {
            Some(a) => a,
            None => continue, // no credentials, skip silently
        };
        if matches!(auth_method, crate::auth::AuthMethod::None) {
            continue;
        }
        let provider: Box<dyn Provider> = match cfg.kind {
            ProviderKind::Anthropic => Box::new(AnthropicProvider::new(cfg.clone(), auth_method)),
            ProviderKind::OpenAi => Box::new(OpenAiProvider::new(cfg.clone(), auth_method)),
            ProviderKind::OpenAiCompat => {
                Box::new(OpenAiCompatProvider::new(cfg.clone(), auth_method))
            }
            ProviderKind::Google => Box::new(GoogleProvider::new(cfg.clone(), auth_method)),
            ProviderKind::Bedrock => {
                Box::new(BedrockAnthropicProvider::new(cfg.clone(), auth_method))
            }
            ProviderKind::Azure => Box::new(AzureOpenAiProvider::new(cfg.clone(), auth_method)),
        };
        let result = provider
            .discover_models()
            .await
            .map_err(|e: AiError| e.to_string());
        out.push(ProviderDiscovery {
            provider: cfg.name.clone(),
            result,
        });
    }
    out
}

/// Convenience: refresh, build a fresh cache, and write it to `path`.
/// Returns `(cache, results)` so callers can both persist the cache and
/// report per-provider outcomes.
pub async fn refresh_and_save(
    registry: &ModelRegistry,
    auth: &AuthStorage,
    path: &Path,
) -> std::io::Result<(DiscoveredCache, Vec<ProviderDiscovery>)> {
    let results = refresh_all(registry, auth).await;
    let mut cache = DiscoveredCache {
        generated_at: chrono::Utc::now().timestamp_millis(),
        providers: Default::default(),
    };
    for r in &results {
        if let Ok(models) = &r.result {
            cache
                .providers
                .insert(r.provider.clone(), models.clone());
        }
    }
    cache.save(path)?;
    Ok((cache, results))
}

/// Default cache location: `<agent_dir>/discovered-models.json`.
/// Callers in pi-coding-agent pass through their own `agent_dir()` helper.
pub fn cache_path(agent_dir: &Path) -> PathBuf {
    agent_dir.join("discovered-models.json")
}

// Keep `Arc` re-exportable for pi-coding-agent's tests.
#[allow(dead_code)]
fn _arc_marker() -> Arc<()> {
    Arc::new(())
}
