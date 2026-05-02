use serde::{Deserialize, Serialize};

/// A pending tool call as parsed from the model output.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolCall {
    pub id: String,
    pub name: String,
    pub input: serde_json::Value,
}
