use serde::{Deserialize, Serialize};

// Re-export the shared types so the old import path `pi_ai::tool::ToolSpec`
// continues to work after they were moved to `pi-tool-types`.
pub use pi_tool_types::{ToolError, ToolResult, ToolSpec};

/// A pending tool call as parsed from the model output.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolCall {
    pub id: String,
    pub name: String,
    pub input: serde_json::Value,
}
