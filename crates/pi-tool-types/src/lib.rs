//! pi-tool-types — shared POD types and traits for the pi-rs tool layer.
//!
//! This crate has NO dependencies on pi-ai, tokio, reqwest, or any async
//! runtime. It exists so that a guest-side worker binary can link only this
//! crate + pi-tools-core without pulling in the full LLM provider universe.
//!
//! The `Tool` trait and `ToolContext` struct live here (not in pi-tools-core)
//! so that pi-tools-net can implement `Tool` without depending on pi-tools-core.

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

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

/// Execution context passed to every tool invocation.
#[derive(Debug, Clone)]
pub struct ToolContext {
    pub cwd: PathBuf,
    pub max_output_bytes: usize,
}

impl Default for ToolContext {
    fn default() -> Self {
        Self {
            cwd: std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")),
            max_output_bytes: 256 * 1024,
        }
    }
}

/// The tool trait every tool must implement.
#[async_trait]
pub trait Tool: Send + Sync {
    fn spec(&self) -> ToolSpec;
    fn read_only(&self) -> bool;
    async fn invoke(
        &self,
        ctx: &ToolContext,
        call_id: &str,
        input: serde_json::Value,
    ) -> Result<ToolResult, ToolError>;
}
