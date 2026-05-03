//! `pi-sdk` — the public Rust API for embedding pi-rs in another application.
//!
//! Pi-sdk is a thin façade over a small set of pi-rs workspace crates
//! (`pi-tool-types`, `pi-ai`, `pi-tools`, `pi-sandbox`, `pi-agent-core`).
//! Embedders depend on this crate and nothing else. The underlying crates
//! remain the source of truth; if a type moves between crates, only
//! `pi-sdk/src/lib.rs` updates and the embedder sees no change.
//!
//! See `RFD 0027 — Pi-rs as a Self-Contained Rust SDK` for the full design
//! contract, stability commitment, and threat model.
//!
//! # ⚠ Pre-1.0
//!
//! pi-sdk is in pre-1.0. Any 0.x → 0.x+1 release MAY break the public API.
//! Pin a fixed version in your Cargo.toml. The 1.0 freeze waits on
//! RFD 0023 (microvm sandbox) + RFD 0026 (remote sandbox) + the hardening
//! contract from RFD 0027 §4.5 landing in pi-rs itself.
//!
//! # Quick start
//!
//! ```no_run
//! use pi_sdk::{
//!     build_runtime_config, AgentEventKind, AgentSessionRuntime, AuthStorage,
//!     BuildConfig, Settings, ToolRegistry,
//! };
//!
//! # async fn run() -> anyhow::Result<()> {
//! let cfg = build_runtime_config(BuildConfig {
//!     auth: AuthStorage::from_env(),
//!     tools: ToolRegistry::with_defaults(),
//!     settings: Settings::default(),
//!     ..BuildConfig::default()
//! });
//! let runtime = AgentSessionRuntime::new(cfg);
//! let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
//! let session = runtime.create_session(Some(tx))?;
//! tokio::spawn(async move { let _ = session.prompt("Hello".into()).await; });
//! while let Some(evt) = rx.recv().await {
//!     match evt.kind {
//!         AgentEventKind::AssistantTextDelta { text } => print!("{text}"),
//!         AgentEventKind::TurnComplete => break,
//!         _ => {}
//!     }
//! }
//! # Ok(()) }
//! ```
//!
//! See `examples/01_minimal.rs` for a runnable end-to-end version.

// ─── Provider / model ─────────────────────────────────────────────
pub use pi_ai::{
    AnthropicProvider, AuthMethod, AuthStorage, AzureOpenAiProvider,
    BedrockAnthropicProvider, ContentBlock, EventStream, FinishReason,
    GenerateRequest, GenerateResponse, GoogleProvider, Message, ModelInfo,
    ModelRegistry, OpenAiCompatProvider, OpenAiProvider, Provider,
    ProviderConfig, ProviderKind, Role, StreamEvent, StreamEventKind,
    ThinkingLevel, ToolCall, Usage,
};

// ─── Tools ────────────────────────────────────────────────────────
pub use pi_tool_types::{ToolError, ToolResult, ToolSpec};
pub use pi_tools::{Tool, ToolContext, ToolRegistry};

// ─── Sandbox ──────────────────────────────────────────────────────
pub use pi_sandbox::{
    LocalProcessProvider, SandboxError, SandboxExecution, SandboxProvider,
};
// Sandbox launcher types (`MicroVmLauncher`, `MicroVmProvider`, `VmHandle`,
// `VmSpec`, `VmCeiling`, `CallLimits`, RFD 0023 §Proposal §2) and remote
// transport types (`RemoteTransport`, `RemoteProvider`, `RemoteSession`,
// `UploadStrategy`, RFD 0026 §Proposal §2) join the public surface
// behind `sandbox-microvm-unstable` / `sandbox-remote-unstable` features
// once those RFDs' implementations land. Trait shapes are already
// specified; only the impl artifacts need to materialise.

// ─── Agent runtime ────────────────────────────────────────────────
pub use pi_agent_core::{
    create_agent_session, default_system_prompt, AgentEvent, AgentEventKind,
    AgentSession, AgentSessionRuntime, Compactor, ConfigBuilder, ConfigError,
    ContextFile, DefaultProviderFactory, EventSender, ProviderFactory,
    RuntimeConfig, SessionEntry, SessionEntryKind, SessionManager, SessionMeta,
    SessionTree, Settings, StreamInterceptor, ToolGate, ToolGateOutcome,
};

// ─── Convenience builder ──────────────────────────────────────────
//
// `BuildConfig` is the convenience wrapper used by the binary, exposed
// for SDK callers. Per RFD 0027 §3, it stays in 0.x for back-compat;
// at 1.0 it becomes a deprecated wrapper around `RuntimeConfig::builder()`
// (which lands in Commit B per RFD 0027 §4).
pub mod build;
pub use build::{build_runtime_config, quick_start, BuildConfig};

// ─── Top-level error type (Commit C per RFD 0027 §1) ─────────────
//
// One thiserror-based facade so embedders catch one error type instead
// of `anyhow`-chaining `pi_ai::AiError`, `pi_sandbox::SandboxError`,
// `pi_tool_types::ToolError`, and `pi_agent_core::RuntimeError` from
// different call sites.
pub mod error;
pub use error::{Error, Result};

// ─── Cost helper (Commit E per RFD 0027 §1) ──────────────────────
//
// Every embedder writes the same per-model price table. Ship one.
// Best-effort numbers, refreshed each MINOR; embedders override via
// `CostRegistry::override_for(model_id, prices)`.
pub mod cost;
pub use cost::{estimate_cost_usd, sum_session_cost_usd, CostRegistry, Pricing};

// ─── Mock provider + sandbox (Commit D, gated on `mocks` feature) ─
//
// Embedder tests should not need to hit a real LLM endpoint or spin
// up a microvm. `mocks::MockProvider` + `mocks::MockSandboxProvider`
// give them stub implementations they can install via the standard
// `RuntimeConfig::builder()` plug-in points. Gated to keep production
// builds free of test-only code.
#[cfg(feature = "mocks")]
pub mod mocks;
#[cfg(feature = "mocks")]
pub use mocks::{MockProvider, MockProviderFactory, MockSandboxProvider, MockSandboxCall};

// ─── Deferred (specified in RFD 0027 §1, ship in later commits) ───
//
// The following surface is part of the SDK contract but lands in
// follow-up commits per RFD 0027 §Implementation schedule:
//
//   - `pi_sdk::cost::{CostRegistry, estimate_cost_usd}` — Commit E.
//   - `pi_sdk::mocks::{MockProvider, MockSandboxProvider}` — Commit D
//     (gated on the `mocks` feature).
//   - `pi_sdk::quick_start(provider, model)` — Commit H7 (Hardening §4.5
//     #8 + UX review). Wires `AuthStorage::in_memory()` (NO env scan) +
//     read-only tools + in-process executor for first-touch demos.
//   - `InProcessExecutor` (renamed from `LocalProcessProvider`) —
//     Commit H7 (Hardening §4.5 #12 default-surface safety renames).
//
// Embedders pinning `pi-sdk = "0.1"` who write code referencing these
// names get a clear "unresolved import" error rather than silent
// behavior drift.
