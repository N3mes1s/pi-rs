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
