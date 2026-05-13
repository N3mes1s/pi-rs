//! Core `SandboxProvider` trait and associated types.
//!
//! Every sandbox implementation fulfills this contract. The trait is
//! intentionally minimal: the agent loop owns the decision (which tool
//! to call, with what input) and delegates *execution* to the sandbox.
//! Results come back as raw (stdout, stderr, exit_status), and the
//! caller converts them into a standard `pi_ai::ToolResult`.

use async_trait::async_trait;
use pi_tools::ToolContext;
use serde::{Deserialize, Serialize};

/// The outcome of executing one tool decision in a sandbox.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SandboxExecution {
    /// Output produced by the tool (stdout or model-facing text).
    pub stdout: String,
    /// Diagnostic output (stderr or error details — never fed to the LLM context).
    pub stderr: String,
    /// 0 on success, non-zero on failure.
    pub exit_status: i32,
}

/// Error types produced by a `SandboxProvider`.
#[derive(Debug, thiserror::Error)]
pub enum SandboxError {
    #[error("sandbox provider error: {0}")]
    Provider(String),
    #[error("tool not found in sandbox: {0}")]
    ToolNotFound(String),
    #[error("sandbox execution timed out")]
    Timeout,
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("microvm unavailable: {0}")]
    Unavailable(String),
    #[error("guest tool error: {0}")]
    Tool(String),
    #[error("vsock io: {0}")]
    Vsock(String),
    #[error("rootfs version mismatch: expected {expected}, got {found}")]
    RootfsMismatch { expected: String, found: String },
    #[error("tool '{tool}' unavailable in sandbox: {reason}")]
    ToolUnavailable { tool: String, reason: &'static str },
}

/// An isolation boundary for tool execution.
///
/// The agent loop calls [`SandboxProvider::execute_tool`] instead of
/// `Tool::invoke()` when a provider is configured. The provider
/// handles spawning, environment setup, stdin/stdout forwarding, etc.
///
/// ### Thread safety
/// `SandboxProvider` is `Send + Sync` so it can be held behind an
/// `Arc` and shared across async tasks (including parallel subagents).
///
/// ### Implementations
/// * [`crate::LocalProcessProvider`] — calls `Tool::invoke()` inside a
///   temporary working directory. No process isolation; available in MVP.
/// * Future: `ContainerProvider` (Docker), `VmProvider` (E2B), etc.
#[async_trait]
pub trait SandboxProvider: Send + Sync {
    /// Short slug that identifies this provider in telemetry rows, e.g.
    /// `"local-process"`, `"docker"`, `"e2b"`.
    fn name(&self) -> &'static str;

    /// Execute a single tool decision in the sandbox.
    ///
    /// - `ctx`: agent's current tool context (cwd, output limit, etc.)
    /// - `tool_name`: which tool the model chose
    /// - `tool_input`: the JSON input the model supplied
    async fn execute_tool(
        &self,
        ctx: &ToolContext,
        tool_name: &str,
        tool_input: &serde_json::Value,
    ) -> Result<SandboxExecution, SandboxError>;

    /// Clean up any persistent state (e.g. stop a container, close a
    /// remote session). Called once at session end, or on user request.
    /// The default implementation is a no-op.
    async fn cleanup(&self) -> Result<(), SandboxError> {
        Ok(())
    }

    /// Whether the runtime should consult `Tool::dispatch()` and
    /// short-circuit on `ToolDispatch::Unavailable` BEFORE
    /// `execute_tool()`. True for true sandbox providers (microvm,
    /// docker, remote VMs) where tools like `lsp` and `monitor`
    /// fundamentally don't fit. False for thin in-process wrappers
    /// (`local-process`) where the same tools do work, since the
    /// dispatch is just "run the tool object directly."
    /// Default: `true`. Per RFD 0023 §"Tool dispatch boundary".
    fn honors_tool_dispatch(&self) -> bool {
        true
    }
}
