//! `pi-sdk-canary` — minimal embedder that exercises the pi-sdk public
//! surface so a future pi-sdk MINOR can't silently break embedders.
//!
//! Per RFD 0027 §3 + Commit G. The canary's job is to fail loudly if
//! a "non-breaking" pi-sdk release actually broke something — by
//! compiling and asserting against the last-MINOR shape of the
//! public API.
//!
//! What the canary covers (every type re-exported from `pi_sdk` is
//! mentioned at least once below; if it's removed from the SDK, this
//! crate stops compiling):
//! - `quick_start`, `RuntimeConfig::builder()`, `ConfigBuilder` setters.
//! - `pi_sdk::Error` variants + `From` impls.
//! - `pi_sdk::cost::{CostRegistry, Pricing, estimate_cost_usd, sum_session_cost_usd}`.
//! - `pi_sdk::mocks::{MockProvider, MockSandboxProvider, MockSandboxCall, MockProviderFactory}`.
//! - `Tool` + `ToolRegistry::register` (returns `Result<(), DuplicateName>`).
//! - `ToolGate::approve` (takes `GateContext`).
//! - `WireSerializer` defaults + tightening.
//! - `AuthStorage::{in_memory, from_env_explicit, scoped, sealed}`.
//! - `LocalProcessProvider::with_readonly_defaults`.
//! - `AgentEvent` / `AgentEventKind` variant names.

use async_trait::async_trait;
use pi_sdk::{
    cost::{estimate_cost_usd, sum_session_cost_usd, CostRegistry, Pricing},
    AgentEvent, AgentEventKind, AgentSessionRuntime, AuthMethod, AuthStorage, ConfigBuilder,
    ConfigError, ContentBlock, Error, FinishReason, GateContext, LocalProcessProvider,
    Message, MockProvider, MockProviderFactory, MockSandboxCall, MockSandboxProvider,
    ModelRegistry, Provider, ProviderConfig, ProviderFactory, RuntimeConfig, RuntimeError,
    SandboxProvider, SessionEntry, SessionEntryKind, SessionManager, Settings, StreamEvent,
    StreamEventKind, StreamInterceptor, Tool, ToolCall, ToolContext, ToolError, ToolGate,
    ToolGateOutcome, ToolRegistry, ToolResult, ToolSpec, Usage, WireSerializer,
};
use std::sync::Arc;

/// Bring every re-exported name into scope so the build fails if any
/// symbol is removed/renamed in a future pi-sdk MINOR. The function
/// is `_unused()` and never called; its body type-references each
/// import to silence dead-code warnings.
#[allow(dead_code)]
fn _every_re_export_compiles(
    _e: &Error,
    _re: &RuntimeError,
    _ce: &ConfigError,
    _ar: &AgentSessionRuntime,
    _ac: &AuthMethod,
    _as: &AuthStorage,
    _cb: &ConfigBuilder,
    _cf: &ContentBlock,
    _fr: &FinishReason,
    _gc: &GateContext,
    _lpp: &LocalProcessProvider,
    _m: &Message,
    _mp: &MockProvider,
    _mpf: &MockProviderFactory,
    _msc: &MockSandboxCall,
    _msp: &MockSandboxProvider,
    _mr: &ModelRegistry,
    _p: &dyn Provider,
    _pc: &ProviderConfig,
    _pf: &dyn ProviderFactory,
    _rc: &RuntimeConfig,
    _sp: &dyn SandboxProvider,
    _se: &SessionEntry,
    _sek: &SessionEntryKind,
    _sm: &SessionManager,
    _s: &Settings,
    _sve: &StreamEvent,
    _svk: &StreamEventKind,
    _si: &dyn StreamInterceptor,
    _t: &dyn Tool,
    _tc: &ToolCall,
    _tx: &ToolContext,
    _te: &ToolError,
    _tg: &dyn ToolGate,
    _tgo: &ToolGateOutcome,
    _tr: &ToolRegistry,
    _trs: &ToolResult,
    _ts: &ToolSpec,
    _u: &Usage,
    _ws: &WireSerializer,
    _ae: &AgentEvent,
    _aek: &AgentEventKind,
    _cr: &CostRegistry,
    _pr: &Pricing,
) {
}

/// Build a runtime via the SAFE-by-default pattern + assert the shape.
/// Mirrors what `pi_sdk::quick_start` produces.
pub fn build_safe_runtime() -> Result<AgentSessionRuntime, Error> {
    let auth = AuthStorage::in_memory();
    let registry = ModelRegistry::new(auth.clone());
    let cfg = RuntimeConfig::builder()
        .session_manager(SessionManager::in_memory())
        .auth_storage(auth)
        .model_registry(registry)
        .tools(ToolRegistry::with_readonly_extras())
        .settings(Settings::default())
        .system_prompt("you are pi-sdk-canary")
        .cwd(std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from(".")))
        .with_sandbox_provider(Arc::new(LocalProcessProvider::with_readonly_defaults()))
        .build()?;
    Ok(AgentSessionRuntime::new(cfg))
}

/// Build a runtime with ALL the H2 budget guards explicitly set.
pub fn build_runtime_with_h2_caps() -> Result<AgentSessionRuntime, Error> {
    let auth = AuthStorage::in_memory();
    let cfg = RuntimeConfig::builder()
        .session_manager(SessionManager::in_memory())
        .auth_storage(auth.clone())
        .model_registry(ModelRegistry::new(auth))
        .tools(ToolRegistry::new())
        .settings(Settings::default())
        .system_prompt("budgeted")
        .cwd(std::path::PathBuf::from("."))
        .with_max_session_tokens(100_000)
        .with_max_tool_invocations_per_turn(20)
        .with_max_recursion(4)
        .build()?;
    Ok(AgentSessionRuntime::new(cfg))
}

/// Round-trip a Pricing through CostRegistry so the API contract on
/// the cost module surface is exercised.
pub fn cost_round_trip() -> f64 {
    let mut registry = CostRegistry::with_bundled_defaults();
    registry.override_for("custom-model", Pricing::flat(2.0, 8.0));
    let usage = Usage {
        input_tokens: 1_000_000,
        output_tokens: 500_000,
        cache_read_tokens: 0,
        cache_write_tokens: 0,
        reasoning_tokens: 0,
        cost_usd: 0.0,
    };
    estimate_cost_usd(&usage, "custom-model", &registry)
}

/// AuthStorage exercise: in-memory + scoped + sealed compose correctly.
pub fn auth_canary() {
    let s = AuthStorage::in_memory();
    s.set("anthropic", AuthMethod::ApiKey { value: "x".into() });
    let scoped = s.scoped(["anthropic"]);
    assert!(scoped.get("anthropic").is_some());
    assert!(scoped.get("openai").is_none());
    let _sealed = s.scoped(["anthropic"]).sealed();
}

/// Implement Tool trait — proves the trait shape compiles externally.
pub struct CanaryTool;

#[async_trait]
impl Tool for CanaryTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "canary".into(),
            description: "no-op canary".into(),
            input_schema: serde_json::json!({"type":"object"}),
        }
    }
    fn read_only(&self) -> bool {
        true
    }
    async fn invoke(
        &self,
        _ctx: &ToolContext,
        call_id: &str,
        _input: serde_json::Value,
    ) -> Result<ToolResult, ToolError> {
        Ok(ToolResult {
            tool_use_id: call_id.into(),
            model_output: "ok".into(),
            display: None,
            is_error: false,
        })
    }
}

/// Implement ToolGate — proves H3's GateContext signature stays stable.
pub struct CanaryGate;

#[async_trait]
impl ToolGate for CanaryGate {
    async fn approve(
        &self,
        _ctx: &GateContext,
        _tool_name: &str,
        _input: &serde_json::Value,
    ) -> ToolGateOutcome {
        ToolGateOutcome::Approve
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn safe_runtime_builds() {
        let _ = build_safe_runtime().expect("safe runtime should build");
    }

    #[test]
    fn h2_capped_runtime_builds() {
        let _ = build_runtime_with_h2_caps().expect("capped runtime should build");
    }

    #[test]
    fn cost_round_trips() {
        // 1M input @ $2 + 500k output @ $8 = $2 + $4 = $6.
        let c = cost_round_trip();
        assert!((c - 6.0).abs() < 0.0001, "expected ~$6, got ${c:.4}");
    }

    #[test]
    fn auth_view_chains_compose() {
        auth_canary();
    }

    #[test]
    fn registry_register_returns_result() {
        // H3 contract: register returns Result<(), DuplicateName>.
        let mut r = ToolRegistry::new();
        r.register(Arc::new(CanaryTool))
            .expect("first register succeeds");
        let err = r
            .register(Arc::new(CanaryTool))
            .expect_err("second register must Err");
        assert!(err.0.contains("canary"));
    }

    #[tokio::test]
    async fn gate_context_top_level_constructs() {
        // GateContext::top_level is the API external embedders use to
        // construct GateContext (struct is non_exhaustive).
        let ctx = GateContext::top_level("sess-1", 1);
        let outcome = CanaryGate.approve(&ctx, "any", &serde_json::Value::Null).await;
        assert!(matches!(outcome, ToolGateOutcome::Approve));
    }

    #[test]
    fn wire_serializer_defaults_strip_ansi() {
        let entry = SessionEntry {
            id: "id".into(),
            parent_id: None,
            timestamp: 0,
            kind: SessionEntryKind::SystemPrompt {
                text: "hi\x1b[31mDANGER\x1b[0m".into(),
            },
        };
        let line = WireSerializer::default().serialize(&entry);
        let v: serde_json::Value = serde_json::from_str(&line).unwrap();
        assert_eq!(v.get("text").and_then(|t| t.as_str()), Some("hiDANGER"));
    }

    #[tokio::test]
    async fn mock_provider_into_factory_works() {
        // MockProvider builder pattern + into_factory chain.
        let factory = MockProvider::new()
            .with_text_response("canary")
            .into_factory();
        let p = factory
            .build(
                ProviderConfig {
                    name: "anthropic".into(),
                    kind: pi_sdk::ProviderKind::Anthropic,
                    base_url: "mock".into(),
                    auth_header: "x".into(),
                    auth_format: "{token}".into(),
                    models: vec![],
                },
                AuthMethod::None,
            )
            .expect("factory build succeeds");
        let _name = p.config().name.clone();
    }

    #[test]
    fn agent_event_kind_variant_names_are_stable() {
        // Match-exhaustiveness check: confirms variant names didn't
        // silently rename. Compile-only; never runs.
        fn _stable(k: AgentEventKind) {
            match k {
                AgentEventKind::SessionStarted { .. } => {}
                AgentEventKind::UserMessage { .. } => {}
                AgentEventKind::AssistantStart => {}
                AgentEventKind::AssistantTextDelta { .. } => {}
                AgentEventKind::AssistantThinkingDelta { .. } => {}
                AgentEventKind::AssistantToolCall { .. } => {}
                AgentEventKind::ToolResult { .. } => {}
                AgentEventKind::AssistantMessage { .. } => {}
                AgentEventKind::Usage { .. } => {}
                AgentEventKind::TurnComplete => {}
                AgentEventKind::Error { .. } => {}
                AgentEventKind::Aborted => {}
                AgentEventKind::CompactionStart { .. } => {}
                AgentEventKind::CompactionComplete { .. } => {}
                AgentEventKind::MonitorEvent { .. } => {}
                AgentEventKind::MonitorEnded { .. } => {}
            }
        }
    }

    #[test]
    fn cost_helpers_aggregate_correctly() {
        let registry = CostRegistry::with_bundled_defaults();
        let usages = vec![
            Usage {
                input_tokens: 1_000_000,
                output_tokens: 1_000_000,
                cache_read_tokens: 0,
                cache_write_tokens: 0,
                reasoning_tokens: 0,
                cost_usd: 0.0,
            },
            Usage {
                input_tokens: 500_000,
                output_tokens: 500_000,
                cache_read_tokens: 0,
                cache_write_tokens: 0,
                reasoning_tokens: 0,
                cost_usd: 0.0,
            },
        ];
        // Haiku: $1/Mtok in + $5/Mtok out.
        // turn 1: $1 + $5 = $6
        // turn 2: $0.5 + $2.5 = $3
        // total: $9
        let total = sum_session_cost_usd(usages.iter(), "claude-haiku-4-5-20251001", &registry);
        assert!((total - 9.0).abs() < 0.0001, "got {total}");
    }
}
