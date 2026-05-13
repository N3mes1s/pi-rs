//! pi-tool-types — shared POD types for the pi-rs tool layer.
//!
//! This crate intentionally has NO dependencies on pi-ai, tokio, reqwest,
//! or any async runtime. It exists so that a guest-side worker binary can
//! link only this crate + pi-tools-core without pulling in the full LLM
//! provider universe.

use serde::{Deserialize, Serialize};

/// JSON-schema description of a tool surfaced to the model.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolSpec {
    pub name: String,
    pub description: String,
    /// JSON Schema (draft 2020-12) describing the tool input.
    pub input_schema: serde_json::Value,
}

/// The result of executing a tool.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolResult {
    pub tool_use_id: String,
    /// Output sent back to the model.
    pub model_output: String,
    /// Optional UI-facing summary or rich payload (separate from model_output
    /// so we can render diffs / file previews without polluting the prompt).
    #[serde(default)]
    pub display: Option<serde_json::Value>,
    #[serde(default)]
    pub is_error: bool,
}

/// Plan-time classification of a tool's dispatch class. Returned by
/// `Tool::dispatch()` (default `Guest`); the runtime consults it
/// before forwarding a tool call to a sandbox provider so it can
/// short-circuit cleanly for tools that fundamentally don't run in
/// the chosen sandbox shape.
///
/// Per RFD 0023 §"Tool dispatch boundary":
///   - `Guest` — runs inside the sandbox provider's execution
///     environment. The vast majority of tools are Guest.
///   - `Unavailable { reason }` — incompatible with the active
///     provider (e.g. `lsp` under microvm: language servers are
///     host-process state with absolute host paths; `monitor`
///     under microvm: streaming protocol won't fit one-shot RPC).
///     The runtime returns the reason to the agent without
///     dispatching, so the model gets a structured "this tool
///     isn't available here" instead of a mysterious failure.
///
/// `Unavailable` is provider-aware in spirit but provider-agnostic
/// at the wire level; tools that work under `local-process` but
/// not under `microvm:firecracker` mark themselves `Unavailable`
/// and rely on the operator picking a compatible provider (or the
/// runtime steering accordingly when policy permits).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ToolDispatch {
    /// Tool runs inside the sandbox provider's execution environment.
    Guest,
    /// Tool is not implementable under the current sandbox shape.
    /// `reason` is shown to the operator.
    Unavailable { reason: &'static str },
}

impl Default for ToolDispatch {
    fn default() -> Self {
        ToolDispatch::Guest
    }
}

/// Errors produced by tool execution.
#[derive(Debug, thiserror::Error)]
pub enum ToolError {
    #[error("tool not found: {0}")]
    NotFound(String),
    #[error("invalid input: {0}")]
    InvalidInput(String),
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("{0}")]
    Other(String),
}
