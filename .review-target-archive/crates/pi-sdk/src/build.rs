//! `quick_start` — first-touch convenience for embedders.
//!
//! Wires the safe defaults (in_memory auth, readonly tools, in-process
//! sandbox) per RFD 0027 §1 + §4.5 #8. Production embedders construct
//! via `RuntimeConfig::builder()` directly.

use crate::{
    AgentSessionRuntime, AuthStorage, Error, LocalProcessProvider, ModelRegistry,
    RuntimeConfig, SessionManager, Settings, ToolRegistry,
};
use std::sync::Arc;

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
        .settings(
            Settings::builder()
                .provider(provider)
                .model(model)
                .build(),
        )
        .system_prompt(pi_agent_core::default_system_prompt().to_string())
        // `cwd` defaults to `current_dir()` inside `ConfigBuilder::build()`
        // (per polish-1) — no need to set it explicitly here.
        .with_sandbox_provider(Arc::new(LocalProcessProvider::with_readonly_defaults()))
        .build()?;
    Ok(AgentSessionRuntime::new(cfg))
}

#[cfg(test)]
mod tests {
    use super::*;

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
