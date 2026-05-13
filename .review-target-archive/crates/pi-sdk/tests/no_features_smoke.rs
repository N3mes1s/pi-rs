//! Default-features smoke test for `pi-sdk`. Most pi-sdk integration
//! tests require `--features mocks` (because they need MockProvider /
//! MockSandboxProvider to drive the runtime without a real LLM).
//! This test runs against the SDK with the DEFAULT feature set —
//! provider-anthropic + tools-readonly per the Cargo.toml — to catch
//! regressions that would only surface if the `mocks` feature gets
//! turned off (which is what every embedder ships in production).
//!
//! Per code-review pass-8 polish: prevents a future commit from
//! gating a public-API symbol behind `mocks` accidentally.
//!
//! ## EDITORS — POLICY
//!
//! Per code-review pass-9 NIT #4: do NOT import any `mocks`-feature-
//! gated symbols (MockProvider, MockSandboxProvider, MockProviderFactory,
//! MockSandboxCall) in this file. The whole point is that this test
//! runs without `--features mocks`. If you need MockProvider, add the
//! test to `tests/end_to_end_safe_path.rs` (gated on `mocks`) instead.

use pi_sdk::{
    quick_start, AgentSessionRuntime, AuthMethod, AuthStorage, ConfigBuilder, ContextFile,
    LocalProcessProvider, ModelRegistry, RuntimeConfig, SessionManager, Settings, ToolRegistry,
};
use std::sync::Arc;

#[test]
fn quick_start_works_without_mocks_feature() {
    // The safe-by-default path is reachable without the `mocks`
    // feature. (Mocks gate only embedder testing helpers, not the
    // shipped SDK surface.)
    let runtime = quick_start("anthropic", "claude-haiku-4-5-20251001")
        .expect("quick_start should work without mocks feature");
    assert_eq!(runtime.config().settings.provider, "anthropic");
}

#[test]
fn full_builder_path_works_without_mocks_feature() {
    let auth = AuthStorage::in_memory();
    auth.set("anthropic", AuthMethod::ApiKey { value: "stub".into() });
    let registry = ModelRegistry::new(auth.clone());
    let cfg = RuntimeConfig::builder()
        .session_manager(SessionManager::in_memory())
        .auth_storage(auth)
        .model_registry(registry)
        .tools(ToolRegistry::with_readonly_extras())
        .settings(Settings::default())
        .system_prompt("test")
        .with_context_files(Vec::<ContextFile>::new())
        .with_sandbox_provider(Arc::new(LocalProcessProvider::with_readonly_defaults()))
        .with_max_session_tokens(100_000)
        .with_max_tool_invocations_per_turn(20)
        .with_max_recursion(4)
        .build()
        .expect("full builder should succeed without mocks");
    let _ = AgentSessionRuntime::new(cfg);
}

#[test]
fn config_builder_default_constructs_without_mocks() {
    // ConfigBuilder + ConfigError are public regardless of features;
    // verify they don't accidentally get re-gated.
    let _b = ConfigBuilder::new();
    // Build with no required fields set: should err with Missing.
    let res = ConfigBuilder::new().build();
    match res {
        Err(pi_sdk::ConfigError::Missing { field }) => {
            assert_eq!(field, "session_manager");
        }
        Err(_) => panic!("expected ConfigError::Missing"),
        Ok(_) => panic!("expected ConfigError::Missing, got Ok"),
    }
}

#[test]
fn re_exports_resolve_without_mocks_feature() {
    // Symbol-existence sweep for the most-used types. Catches a
    // future regression that gates a re-export behind `mocks`.
    use pi_sdk::{
        cost::{estimate_cost_usd, sum_session_cost_usd, CostRegistry, Pricing},
        AgentEventKind, ConfigError, DuplicateName, Error, GateContext,
        ProviderKind, Result, RuntimeError, SettingsBuilder, ThinkingLevel, ToolError,
        ToolGateOutcome, WireSerializer,
    };
    // Cost / Pricing surface
    let _: Pricing = Pricing::flat(1.0, 2.0);
    let _: CostRegistry = CostRegistry::with_bundled_defaults();
    let _ = estimate_cost_usd;
    let _ = sum_session_cost_usd::<std::iter::Empty<&pi_sdk::Usage>>;
    // Runtime-config surface
    let _: GateContext = GateContext::top_level("s", 0);
    let _: WireSerializer = WireSerializer::default();
    let _ = ConfigError::Missing { field: "x" };
    let _: SettingsBuilder = SettingsBuilder::new();
    // Error / variants
    let _ = ToolError::NotFound("y".into());
    let _ = DuplicateName("z".into());
    let _ = Error::Other("w".into());
    let _: Result<()> = Ok(());
    // Enums
    let _: AgentEventKind = AgentEventKind::TurnComplete;
    let _: ProviderKind = ProviderKind::Anthropic;
    let _: ThinkingLevel = ThinkingLevel::Off;
    let _: ToolGateOutcome = ToolGateOutcome::Approve;
    let _: RuntimeError = RuntimeError::EmptyTurn;
}

#[test]
fn provider_and_tool_traits_resolve_without_mocks() {
    // Per code-review pass-9 NIT #3: extend the sweep to cover
    // provider-implementor types, tool/sandbox traits, and the
    // session-telemetry surface that `re_exports_resolve_without_mocks_feature`
    // doesn't reach by name. Catches a regression that drops a
    // provider re-export or moves it behind `mocks`.
    //
    // Per pass-10 NIT #5: the helpers below are never CALLED, only
    // DEFINED. That's intentional — Rust type-checks unused-function
    // parameter signatures at compile time, so a missing/renamed
    // re-export fails to resolve and the test (and the whole binary)
    // stops compiling. Calling the helpers would require constructing
    // values for every parameter, defeating the purpose.
    use pi_sdk::{
        AnthropicProvider, AzureOpenAiProvider, BedrockAnthropicProvider, GoogleProvider,
        OpenAiCompatProvider, OpenAiProvider, Provider, ProviderConfig, SandboxError,
        SandboxExecution, SandboxProvider, SessionEntry, SessionEntryKind, SessionMeta,
        SessionTree, Tool, ToolCall, ToolContext, ToolResult, ToolSpec, Usage,
    };
    fn _take_provider_dyn(_: &dyn Provider) {}
    fn _take_sandbox_dyn(_: &dyn SandboxProvider) {}
    fn _take_tool_dyn(_: &dyn Tool) {}
    fn _take_concrete_providers(
        _: AnthropicProvider,
        _: AzureOpenAiProvider,
        _: BedrockAnthropicProvider,
        _: GoogleProvider,
        _: OpenAiCompatProvider,
        _: OpenAiProvider,
    ) {
    }
    fn _take_session_telemetry(
        _: SessionEntry,
        _: SessionEntryKind,
        _: SessionMeta,
        _: SessionTree,
    ) {
    }
    fn _take_tool_types(_: ToolCall, _: ToolContext, _: ToolResult, _: ToolSpec) {}
    fn _take_sandbox_types(_: SandboxError, _: SandboxExecution) {}
    fn _take_pod(_: ProviderConfig, _: Usage) {}
}
