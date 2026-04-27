//! Per-provider live model discovery via the standard `/v1/models` endpoint.
//!
//! These free helpers are called by each provider's `discover_models()`
//! impl. Kept here so the OpenAI-compat family (Fireworks, Groq, Cerebras,
//! xAI, OpenRouter, DeepSeek, Mistral, ZAI, HuggingFace, Ollama, Kimi,
//! MiniMax) all share one implementation.
//!
//! Returned `ModelInfo` entries have:
//! * `id` — the provider-native model id
//! * `provider` — the [`ProviderConfig::name`]
//! * `alias` — None (live-discovered models don't get short aliases)
//! * `context_window` — the value the API reports, or 8192 if absent
//! * `max_output_tokens` — same, or 4096
//! * `supports_thinking` / `supports_vision` / cost — defaulted to
//!   `false` / `false` / `0.0` because the bare list endpoint doesn't
//!   return them. Curated entries in the static catalog still own the
//!   accurate values (the registry merges live entries on top of the
//!   static catalog with the static one winning on conflict).

use reqwest::Client;
use serde_json::Value;

use crate::auth::AuthMethod;
use crate::registry::{ModelInfo, ProviderConfig};
use crate::{AiError, Result};

/// Build a `ModelInfo` with conservative defaults from a provider name + id.
fn make_info(provider: &str, id: &str, ctx: u32, out: u32) -> ModelInfo {
    ModelInfo {
        provider: provider.to_string(),
        id: id.to_string(),
        alias: None,
        context_window: ctx,
        max_output_tokens: out,
        supports_thinking: false,
        supports_tools: true,
        supports_vision: false,
        input_cost_per_mtok: 0.0,
        output_cost_per_mtok: 0.0,
    }
}

/// `GET {base_url}/models` with `Authorization: Bearer <token>` (or the
/// provider's configured auth header). Used by OpenAI and every
/// OpenAI-compatible provider.
///
/// Returns the parsed `data: [{id, ...}]` list as `ModelInfo`s.
pub async fn openai_compatible(
    client: &Client,
    config: &ProviderConfig,
    auth: &AuthMethod,
) -> Result<Vec<ModelInfo>> {
    let token = match auth {
        AuthMethod::ApiKey { value } => value.clone(),
        AuthMethod::OAuth { access_token, .. } => access_token.clone(),
        AuthMethod::None => return Err(AiError::MissingAuth(config.name.clone())),
    };
    let header_value = config.auth_format.replace("{token}", &token);
    let url = format!("{}/models", config.base_url.trim_end_matches('/'));
    let resp = client
        .get(&url)
        .header(config.auth_header.as_str(), header_value)
        .header("content-type", "application/json")
        .send()
        .await?;
    if !resp.status().is_success() {
        let status = resp.status().as_u16();
        let body = resp.text().await.unwrap_or_default();
        return Err(AiError::Provider { status, body });
    }
    let v: Value = resp.json().await?;
    let mut out = Vec::new();
    if let Some(arr) = v.get("data").and_then(|d| d.as_array()) {
        for m in arr {
            if let Some(id) = m.get("id").and_then(|v| v.as_str()) {
                let ctx = m
                    .get("context_window")
                    .and_then(|v| v.as_u64())
                    .or_else(|| m.get("context_length").and_then(|v| v.as_u64()))
                    .unwrap_or(8192) as u32;
                let max_out = m
                    .get("max_output_tokens")
                    .and_then(|v| v.as_u64())
                    .or_else(|| m.get("max_tokens").and_then(|v| v.as_u64()))
                    .unwrap_or(4096) as u32;
                out.push(make_info(&config.name, id, ctx, max_out));
            }
        }
    }
    Ok(out)
}

/// Anthropic's `/v1/models` (added 2024). Auth is `x-api-key` not
/// `Authorization: Bearer`. Response shape:
/// `{data: [{id, type, display_name, created_at}]}`.
pub async fn anthropic(
    client: &Client,
    config: &ProviderConfig,
    auth: &AuthMethod,
) -> Result<Vec<ModelInfo>> {
    let token = match auth {
        AuthMethod::ApiKey { value } => value.clone(),
        AuthMethod::OAuth { access_token, .. } => access_token.clone(),
        AuthMethod::None => return Err(AiError::MissingAuth(config.name.clone())),
    };
    let url = format!("{}/v1/models", config.base_url.trim_end_matches('/'));
    let resp = client
        .get(&url)
        .header("x-api-key", token)
        .header("anthropic-version", "2023-06-01")
        .send()
        .await?;
    if !resp.status().is_success() {
        let status = resp.status().as_u16();
        let body = resp.text().await.unwrap_or_default();
        return Err(AiError::Provider { status, body });
    }
    let v: Value = resp.json().await?;
    let mut out = Vec::new();
    if let Some(arr) = v.get("data").and_then(|d| d.as_array()) {
        for m in arr {
            if let Some(id) = m.get("id").and_then(|v| v.as_str()) {
                // Anthropic /v1/models doesn't return context_window — use
                // 200_000 as a conservative default that covers Claude 3+.
                out.push(make_info(&config.name, id, 200_000, 8192));
            }
        }
    }
    Ok(out)
}

/// Google Gemini's `/v1beta/models?key=…`. Response shape:
/// `{models: [{name: "models/gemini-…", inputTokenLimit, outputTokenLimit,
/// supportedGenerationMethods}]}`.
pub async fn google(
    client: &Client,
    config: &ProviderConfig,
    auth: &AuthMethod,
) -> Result<Vec<ModelInfo>> {
    let token = match auth {
        AuthMethod::ApiKey { value } => value.clone(),
        AuthMethod::OAuth { access_token, .. } => access_token.clone(),
        AuthMethod::None => return Err(AiError::MissingAuth(config.name.clone())),
    };
    let url = format!(
        "{}/v1beta/models?key={}",
        config.base_url.trim_end_matches('/'),
        token
    );
    let resp = client.get(&url).send().await?;
    if !resp.status().is_success() {
        let status = resp.status().as_u16();
        let body = resp.text().await.unwrap_or_default();
        return Err(AiError::Provider { status, body });
    }
    let v: Value = resp.json().await?;
    let mut out = Vec::new();
    if let Some(arr) = v.get("models").and_then(|d| d.as_array()) {
        for m in arr {
            // `name` is "models/gemini-…" — strip the prefix.
            let id = m
                .get("name")
                .and_then(|v| v.as_str())
                .map(|s| s.strip_prefix("models/").unwrap_or(s).to_string());
            let Some(id) = id else { continue };
            // Only keep entries that support generateContent (not embeddings, tts, etc).
            let supports_gen = m
                .get("supportedGenerationMethods")
                .and_then(|v| v.as_array())
                .map(|a| {
                    a.iter()
                        .any(|s| s.as_str() == Some("generateContent") || s.as_str() == Some("streamGenerateContent"))
                })
                .unwrap_or(true);
            if !supports_gen {
                continue;
            }
            let ctx = m
                .get("inputTokenLimit")
                .and_then(|v| v.as_u64())
                .unwrap_or(8192) as u32;
            let max_out = m
                .get("outputTokenLimit")
                .and_then(|v| v.as_u64())
                .unwrap_or(4096) as u32;
            out.push(make_info(&config.name, &id, ctx, max_out));
        }
    }
    Ok(out)
}
