use async_trait::async_trait;
use pi_ai::{ToolResult, ToolSpec};
use serde_json::{json, Value};

use crate::{resolve_path, truncate_for_model, Tool, ToolContext, ToolError};

pub struct LsTool;

#[async_trait]
impl Tool for LsTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "ls".into(),
            description: "List directory contents (non-recursive).".into(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "path": {"type": "string", "description": "directory (default cwd)"}
                }
            }),
        }
    }

    fn read_only(&self) -> bool {
        true
    }

    async fn invoke(
        &self,
        ctx: &ToolContext,
        call_id: &str,
        input: Value,
    ) -> Result<ToolResult, ToolError> {
        let path = match input.get("path").and_then(|v| v.as_str()) {
            Some(p) => resolve_path(ctx, p),
            None => ctx.cwd.clone(),
        };
        let mut entries = match std::fs::read_dir(&path) {
            Ok(it) => it
                .flatten()
                .map(|e| {
                    let kind = if e.file_type().map(|t| t.is_dir()).unwrap_or(false) {
                        "/"
                    } else {
                        ""
                    };
                    format!("{}{}", e.file_name().to_string_lossy(), kind)
                })
                .collect::<Vec<_>>(),
            Err(e) => {
                return Ok(ToolResult {
                    tool_use_id: call_id.into(),
                    model_output: format!("ERROR: {}", e),
                    display: None,
                    is_error: true,
                });
            }
        };
        entries.sort();
        let model_output = truncate_for_model(&entries.join("\n"), ctx.max_output_bytes);
        Ok(ToolResult {
            tool_use_id: call_id.into(),
            model_output,
            display: Some(json!({"kind": "ls", "path": path.display().to_string(), "count": entries.len()})),
            is_error: false,
        })
    }
}
