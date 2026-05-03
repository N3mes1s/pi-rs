//! Convenience builder for `RuntimeConfig`.
//!
//! Moved from `pi_coding_agent::sdk` per RFD 0027 Commit A. The shape
//! is unchanged; only the import path changes from `pi_coding_agent::sdk`
//! to `pi_sdk` (or `pi_sdk::build`).
//!
//! At SDK 1.0, `BuildConfig` becomes a deprecated wrapper around
//! `RuntimeConfig::builder()` (which lands in Commit B). Embedders
//! pinning `pi-sdk = "0.1"` get the convenience-builder shape today;
//! pinning `pi-sdk = "1"` later adopts the builder API.

use crate::{
    AuthStorage, ContextFile, ModelRegistry, RuntimeConfig, SessionManager, Settings, ToolRegistry,
};
use std::path::PathBuf;

/// Convenience inputs for `build_runtime_config`. Filling in only the
/// fields you care about and using `..BuildConfig::default()` for the
/// rest is the canonical embedder shape during 0.x.
///
/// Note on safety: `BuildConfig::default()` calls `AuthStorage::from_env()`,
/// which scans 17 environment variables unconditionally. For production
/// embedders this is a CWE-526 risk; per RFD 0027 §4.5 #8 (Hardening
/// Commit H5), the safer alternative is `AuthStorage::from_env_explicit(&[
/// (provider, env_key), ...])` once H5 lands. SDK 0.x retains the
/// `from_env()` default for parity with the existing seed module.
#[derive(Clone)]
pub struct BuildConfig {
    pub auth: AuthStorage,
    pub registry: ModelRegistry,
    pub session_manager: SessionManager,
    pub tools: ToolRegistry,
    pub settings: Settings,
    pub system_prompt: String,
    pub context_files: Vec<ContextFile>,
    pub cwd: PathBuf,
}

impl Default for BuildConfig {
    fn default() -> Self {
        let auth = AuthStorage::from_env();
        let registry = ModelRegistry::new(auth.clone());
        Self {
            auth,
            registry,
            session_manager: SessionManager::in_memory(),
            tools: ToolRegistry::with_defaults(),
            settings: Settings::default(),
            system_prompt: pi_agent_core::default_system_prompt().to_string(),
            context_files: Vec::new(),
            cwd: std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")),
        }
    }
}

/// Materialise a `RuntimeConfig` from a `BuildConfig`. Plug-in fields
/// (`provider_factory`, `tool_gate`, `stream_interceptor`,
/// `sandbox_provider`) default to `None`; chain them on with the
/// fluent `with_*` methods on the returned `RuntimeConfig`.
///
/// Implementation note: `RuntimeConfig` is `#[non_exhaustive]` (RFD 0027
/// §4), so external crates like `pi-sdk` cannot construct it via struct
/// literal. We use the canonical `RuntimeConfig::builder()` path instead.
pub fn build_runtime_config(b: BuildConfig) -> RuntimeConfig {
    RuntimeConfig::builder()
        .session_manager(b.session_manager)
        .auth_storage(b.auth)
        .model_registry(b.registry)
        .tools(b.tools)
        .settings(b.settings)
        .system_prompt(b.system_prompt)
        .with_context_files(b.context_files)
        .cwd(b.cwd)
        .build_unwrap()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_build_config_produces_runnable_config() {
        // No env vars set in the test → AuthStorage::from_env() returns empty.
        // The config still builds; it just won't be able to call any provider
        // without auth.
        let cfg = build_runtime_config(BuildConfig::default());
        assert_eq!(cfg.system_prompt, pi_agent_core::default_system_prompt());
        // Default tool set has at least the basic four (read/write/edit/bash).
        // We don't assert exact count here because the underlying defaults can
        // grow; just that it's non-empty.
        assert!(cfg.tools.specs().len() >= 4);
    }
}
