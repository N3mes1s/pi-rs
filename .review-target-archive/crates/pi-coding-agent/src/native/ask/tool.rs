//! `ask` tool: pose a structured multiple-choice question.
//!
//! Input schema:
//! ```text
//! {
//!   "question":     string,
//!   "options":      string[],
//!   "allow_multi":  bool,            (default false)
//!   "descriptions": string[]?,       (per-option help text)
//! }
//! ```
//!
//! Today the tool always returns
//! `is_error: true, model_output: "ASK requires interactive mode"` —
//! that's the exact contract for print/json/rpc modes from the spec,
//! and the interactive picker integration is a planned follow-up
//! (see crate-level docs in `super::mod.rs`).

use async_trait::async_trait;
use pi_ai::{ToolResult, ToolSpec};
use pi_tools::{Tool, ToolContext, ToolError};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

pub struct AskTool;

/// Parsed input of the `ask` tool. Exposed for the (future) TUI picker
/// glue and for unit tests.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AskInput {
    pub question: String,
    pub options: Vec<String>,
    #[serde(default)]
    pub allow_multi: bool,
    #[serde(default)]
    pub descriptions: Option<Vec<String>>,
}

impl AskInput {
    pub fn parse(v: &Value) -> Result<Self, ToolError> {
        let question = v
            .get("question")
            .and_then(|x| x.as_str())
            .ok_or_else(|| ToolError::InvalidInput("missing `question`".into()))?
            .to_string();
        let opts_v = v
            .get("options")
            .and_then(|x| x.as_array())
            .ok_or_else(|| ToolError::InvalidInput("missing `options` array".into()))?;
        if opts_v.is_empty() {
            return Err(ToolError::InvalidInput(
                "`options` must contain at least one entry".into(),
            ));
        }
        let mut options = Vec::with_capacity(opts_v.len());
        for o in opts_v {
            let s = o
                .as_str()
                .ok_or_else(|| ToolError::InvalidInput("each option must be a string".into()))?;
            options.push(s.to_string());
        }
        let allow_multi = v
            .get("allow_multi")
            .and_then(|x| x.as_bool())
            .unwrap_or(false);
        let descriptions = v.get("descriptions").and_then(|x| x.as_array()).map(|arr| {
            arr.iter()
                .map(|x| x.as_str().unwrap_or("").to_string())
                .collect::<Vec<_>>()
        });
        Ok(AskInput {
            question,
            options,
            allow_multi,
            descriptions,
        })
    }
}

#[async_trait]
impl Tool for AskTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "ask".into(),
            description: "Pose a structured multiple-choice question to the user. The TUI \
                 renders an inline picker; the user selects with arrow keys + Enter. \
                 Returns {answers: [string, …]}. In non-interactive modes (print, json, rpc) \
                 this tool is unavailable and returns an error."
                .into(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "question":     { "type": "string" },
                    "options":      { "type": "array", "items": { "type": "string" } },
                    "allow_multi":  { "type": "boolean" },
                    "descriptions": { "type": "array", "items": { "type": "string" } }
                },
                "required": ["question", "options"]
            }),
        }
    }

    fn read_only(&self) -> bool {
        true
    }

    async fn invoke(
        &self,
        _ctx: &ToolContext,
        call_id: &str,
        input: Value,
    ) -> Result<ToolResult, ToolError> {
        // Validate the input — even if we can't satisfy it yet, the agent
        // should learn that its request was well-formed. Bad inputs still
        // surface as ToolError.
        let parsed = AskInput::parse(&input)?;
        Ok(ToolResult {
            tool_use_id: call_id.into(),
            model_output: "ASK requires interactive mode".to_string(),
            display: Some(json!({
                "kind": "ask",
                "question": parsed.question,
                "options": parsed.options,
                "allow_multi": parsed.allow_multi,
                "descriptions": parsed.descriptions,
            })),
            is_error: true,
        })
    }
}
