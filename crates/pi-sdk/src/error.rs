//! Top-level `Error` type for `pi-sdk`.
//!
//! Per RFD 0027 §1 + Commit C: a single thiserror-based facade so
//! embedders catch one error type instead of `anyhow`-chaining
//! `pi_ai::AiError`, `pi_sandbox::SandboxError`, `pi_tool_types::ToolError`,
//! and `pi_agent_core::RuntimeError` from different call sites.
//!
//! ## Stability
//!
//! Per RFD 0027 §3: `Error` is `#[non_exhaustive]` (variant-additive
//! is non-breaking) and variant *names* are stable at SDK 1.0.
//! `Display` strings (the `#[error("...")]` formatters) MAY change
//! within a MINOR — embedders should match on variant identity, not
//! parse strings.
//!
//! ## Variants reserved for future commits
//!
//! `BudgetExhausted` and `DepthExceeded` are declared here so the
//! type contract ships in Commit C, but they are *produced* by
//! Hardening Commit H2 (per-session call-depth + token-budget
//! guards from RFD 0027 §4.5 #3). Until H2 lands, these variants
//! are unreachable in practice.

use thiserror::Error;

/// Errors surfaced by the pi-sdk public API.
///
/// `Error: std::error::Error + Send + Sync + 'static` is a stable
/// guarantee at SDK 1.0 — embedders can wrap us in `anyhow`,
/// `eyre`, or any `Box<dyn std::error::Error>` chain.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum Error {
    /// LLM provider transport, decode, or remote-error failure.
    /// Wraps `pi_ai::AiError`. See that type for the exhaustive
    /// list of provider-side failure modes.
    #[error("provider error: {0}")]
    Provider(#[from] pi_ai::AiError),

    /// Sandbox provider failure — microvm boot, vsock transport,
    /// guest tool absence, rootfs version mismatch, etc.
    /// Wraps `pi_sandbox::SandboxError`.
    #[error("sandbox error: {0}")]
    Sandbox(#[from] pi_sandbox::SandboxError),

    /// Tool invocation failure — tool not found, invalid input,
    /// or tool-internal IO error. Wraps `pi_tool_types::ToolError`.
    #[error("tool error: {0}")]
    Tool(#[from] pi_tool_types::ToolError),

    /// Runtime loop failure — abort, unknown model, unsupported
    /// operation, mid-stream provider error, IO. Wraps
    /// `pi_agent_core::RuntimeError`.
    #[error("runtime error: {0}")]
    Runtime(#[from] pi_agent_core::RuntimeError),

    /// `RuntimeConfig::builder().build()` was called with a required
    /// field unset. Wraps `pi_agent_core::ConfigError`.
    #[error("config error: {0}")]
    Config(#[from] pi_agent_core::ConfigError),

    /// I/O failure not attributable to a specific subsystem.
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),

    /// JSON serialisation/deserialisation failure (e.g. when serde
    /// fails to parse a tool input). Wraps `serde_json::Error`.
    #[error("serde_json error: {0}")]
    SerdeJson(#[from] serde_json::Error),

    /// Per-session token-budget cap exceeded (Hardening §4.5 #3).
    /// Produced by Commit H2 once budget guards land. Embedders
    /// should treat this as operator-recoverable: bump
    /// `RuntimeConfig::max_session_tokens` if appropriate, otherwise
    /// abort the session and surface to the caller.
    #[error("session token budget exhausted: used {used}, cap was {cap}")]
    BudgetExhausted { used: u64, cap: u64 },

    /// Tool-recursion depth cap exceeded (Hardening §4.5 #3).
    /// Produced by Commit H2 once depth guards land. Indicates a
    /// custom `Tool::invoke` re-entered `AgentSession::send` more
    /// than `RuntimeConfig::max_recursion` times.
    #[error("tool recursion depth exceeded: depth {depth}, cap was {cap}")]
    DepthExceeded { depth: usize, cap: usize },

    /// Per-turn tool-invocation cap exceeded (Hardening §4.5 #3).
    /// Produced by Commit H2 once per-turn budget guards land.
    #[error(
        "per-turn tool invocation cap exceeded: invoked {invoked}, cap was {cap}"
    )]
    InvocationCapExceeded { invoked: usize, cap: usize },

    /// Stream interceptor injected synthetic-user messages more
    /// than the per-turn cap allows (Hardening §4.5 #10). Produced
    /// by Commit H6.
    #[error("stream interceptor thrash: {aborts} aborts in single turn (cap {cap})")]
    InterceptorThrash { aborts: usize, cap: usize },

    /// Catch-all for failures the embedder cannot meaningfully
    /// dispatch on. Stable variant identity; embedders matching
    /// `_` or `Other(_)` cover all forward-additions.
    #[error("{0}")]
    Other(String),
}

/// Type alias matching `std::result::Result<T, pi_sdk::Error>`.
/// Embedders writing `pi_sdk::Result<T>` get a one-line shorthand
/// for the SDK's standard return shape.
pub type Result<T> = std::result::Result<T, Error>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn error_is_send_sync_static() {
        // Compile-time check: `Error` satisfies the SDK 1.0 stability
        // promise (`Error: std::error::Error + Send + Sync + 'static`).
        fn assert_bounds<T: std::error::Error + Send + Sync + 'static>() {}
        assert_bounds::<Error>();
    }

    #[test]
    fn from_ai_error_works() {
        let ai = pi_ai::AiError::MissingAuth("anthropic".into());
        let sdk: Error = ai.into();
        assert!(matches!(sdk, Error::Provider(_)));
    }

    #[test]
    fn from_sandbox_error_works() {
        let sb = pi_sandbox::SandboxError::Timeout;
        let sdk: Error = sb.into();
        assert!(matches!(sdk, Error::Sandbox(_)));
    }

    #[test]
    fn from_tool_error_works() {
        let te = pi_tool_types::ToolError::NotFound("read".into());
        let sdk: Error = te.into();
        assert!(matches!(sdk, Error::Tool(_)));
    }

    #[test]
    fn from_config_error_works() {
        let ce = pi_agent_core::ConfigError::Missing { field: "tools" };
        let sdk: Error = ce.into();
        assert!(matches!(sdk, Error::Config(_)));
    }

    #[test]
    fn budget_exhausted_displays_used_and_cap() {
        let e = Error::BudgetExhausted { used: 100_000, cap: 50_000 };
        let s = format!("{e}");
        assert!(s.contains("100000") && s.contains("50000"));
    }
}
