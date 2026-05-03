//! Convenience builder for `RuntimeConfig`.
//!
//! `BuildConfig` is the convenience wrapper used by the binary,
//! exposed for SDK callers who prefer struct-literal-style
//! construction. (Originally lived as `pi_coding_agent::sdk` per
//! RFD 0027 Commit A; promoted to its own crate as the seed of
//! `pi-sdk`. The legacy shim was removed in Commit K.)
//!
//! At SDK 1.0, `BuildConfig` becomes a deprecated wrapper around
//! `RuntimeConfig::builder()` (which lands in Commit B — already
//! shipped). Embedders pinning `pi-sdk = "0.1"` get the
//! convenience-builder shape today; pinning `pi-sdk = "1"` later
//! adopts the builder API.

use crate::{
    AgentSessionRuntime, AuthStorage, ContextFile, Error, LocalProcessProvider, ModelRegistry,
    RuntimeConfig, SessionManager, Settings, ToolRegistry,
};
use std::path::PathBuf;
use std::sync::Arc;

/// Convenience inputs for `build_runtime_config`. Filling in only the
/// fields you care about and using `..BuildConfig::default()` for the
/// rest is the canonical embedder shape during 0.x.
///
/// Note on safety: `BuildConfig::default()` returns
/// `AuthStorage::in_memory()` — the SAFE default. Embedders wanting
/// auto-discovery from env vars name the providers they trust via
/// `AuthStorage::from_env_explicit(&[(provider, env_key), ...])` and
/// pass the result on `BuildConfig.auth` explicitly. (Per polish-12
/// the unsafe `from_env()` slurp was removed entirely.)
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
        // Per RFD 0027 §4.5 #8 (Hardening H5) + polish-12 cleanup:
        // `BuildConfig::default()` no longer slurps env vars. Embedders
        // wanting auto-discovery name the providers they trust via
        // `AuthStorage::from_env_explicit(...)` and pass the result on
        // `BuildConfig.auth` explicitly. This default is the
        // safe-by-default in-memory storage; embedders setting creds
        // call `auth.set(provider, AuthMethod::ApiKey { ... })`.
        let auth = AuthStorage::in_memory();
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

/// Convenience wiring for first-touch demos and docs.rs examples.
///
/// Per RFD 0027 §1 + §4.5 #8 + Commit H7. Returns an
/// `AgentSessionRuntime` configured with:
///
/// - `AuthStorage::in_memory()` — NO env scan (CWE-526 magnet
///   avoided);
/// - `ToolRegistry::with_readonly_extras()` — no shell, no fs
///   mutation, no network;
/// - `LocalProcessProvider::with_readonly_defaults()` as the
///   sandbox provider;
/// - the `provider` and `model` you specify on `Settings`;
/// - `system_prompt` = the pi-rs default;
/// - `cwd` = `std::env::current_dir()`.
///
/// **The returned runtime has NO credentials.** Embedders MUST call
/// `runtime.config().auth_storage.set(provider, AuthMethod::ApiKey {
/// value })` (or pass a populated `AuthStorage` via the full
/// `RuntimeConfig::builder()` instead) before the first `prompt()`,
/// otherwise the LLM call fails with `Error::Provider(MissingAuth)`.
///
/// **Production embedders should use the full builder, not this
/// function.** `quick_start` exists so the README example fits in
/// 5 lines.
pub fn quick_start(provider: &str, model: &str) -> Result<AgentSessionRuntime, Error> {
    let auth = AuthStorage::in_memory();
    let registry = ModelRegistry::new(auth.clone());
    let cfg = RuntimeConfig::builder()
        .session_manager(SessionManager::in_memory())
        .auth_storage(auth)
        .model_registry(registry)
        .tools(ToolRegistry::with_readonly_extras())
        .settings(Settings {
            provider: provider.to_string(),
            model: model.to_string(),
            ..Settings::default()
        })
        .system_prompt(pi_agent_core::default_system_prompt().to_string())
        .cwd(std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")))
        .with_sandbox_provider(Arc::new(LocalProcessProvider::with_readonly_defaults()))
        .build()?;
    Ok(AgentSessionRuntime::new(cfg))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_build_config_produces_runnable_config() {
        // BuildConfig::default() returns AuthStorage::in_memory() (post-polish-12).
        // The config still builds; it just won't be able to call any provider
        // without auth.
        let cfg = build_runtime_config(BuildConfig::default());
        assert_eq!(cfg.system_prompt, pi_agent_core::default_system_prompt());
        // Default tool set has at least the basic four (read/write/edit/bash).
        // We don't assert exact count here because the underlying defaults can
        // grow; just that it's non-empty.
        assert!(cfg.tools.specs().len() >= 4);
    }

    #[test]
    fn quick_start_produces_runnable_runtime_with_readonly_tools() {
        let runtime = quick_start("anthropic", "claude-haiku-4-5-20251001")
            .expect("quick_start should build cleanly");
        let cfg = runtime.config();
        // Read-only tool surface: read/grep/find/ls (4 tools), no
        // write/edit/bash.
        let names: std::collections::HashSet<String> =
            cfg.tools.specs().iter().map(|s| s.name.clone()).collect();
        assert!(names.contains("read"), "readonly should include `read`");
        assert!(names.contains("grep"), "readonly should include `grep`");
        assert!(names.contains("find"), "readonly should include `find`");
        assert!(names.contains("ls"), "readonly should include `ls`");
        assert!(!names.contains("bash"), "quick_start MUST NOT register `bash`");
        assert!(!names.contains("write"), "quick_start MUST NOT register `write`");
        assert!(!names.contains("edit"), "quick_start MUST NOT register `edit`");
        assert_eq!(cfg.settings.provider, "anthropic");
        assert_eq!(cfg.settings.model, "claude-haiku-4-5-20251001");
        // Sandbox provider is wired (the readonly LocalProcessProvider).
        assert!(cfg.sandbox_provider.is_some(),
                "quick_start should wire LocalProcessProvider::with_readonly_defaults");
    }
}
