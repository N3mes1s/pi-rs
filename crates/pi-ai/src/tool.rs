use serde::{Deserialize, Serialize};

/// JSON-schema description of a tool surfaced to the model.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolSpec {
    pub name: String,
    pub description: String,
    /// JSON Schema (draft 2020-12) describing the tool input.
    pub input_schema: serde_json::Value,
}

/// A pending tool call as parsed from the model output.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolCall {
    pub id: String,
    pub name: String,
    pub input: serde_json::Value,
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
