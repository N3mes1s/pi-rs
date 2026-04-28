use async_trait::async_trait;
use pi_ai::{ToolResult, ToolSpec};
use serde_json::{json, Value};

use crate::{resolve_path, Tool, ToolContext, ToolError};

pub struct EditTool;

#[async_trait]
impl Tool for EditTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "edit".into(),
            description:
                "Surgical text replacement in a file. `old_string` must match exactly (whitespace included) and be unique unless replace_all is set."
                    .into(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "path": {"type": "string"},
                    "old_string": {"type": "string"},
                    "new_string": {"type": "string"},
                    "replace_all": {"type": "boolean", "default": false}
                },
                "required": ["path", "old_string", "new_string"]
            }),
        }
    }

    fn read_only(&self) -> bool {
        false
    }

    async fn invoke(
        &self,
        ctx: &ToolContext,
        call_id: &str,
        input: Value,
    ) -> Result<ToolResult, ToolError> {
        let path = input
            .get("path")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ToolError::InvalidInput("missing `path`".into()))?;
        let old = input
            .get("old_string")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ToolError::InvalidInput("missing `old_string`".into()))?;
        let new = input
            .get("new_string")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ToolError::InvalidInput("missing `new_string`".into()))?;
        let replace_all = input
            .get("replace_all")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);

        let resolved = resolve_path(ctx, path);
        let txt = tokio::fs::read_to_string(&resolved).await?;
        let count = txt.matches(old).count();
        if count == 0 {
            return Ok(ToolResult {
                tool_use_id: call_id.into(),
                model_output: format!("ERROR: old_string not found in {}", resolved.display()),
                display: Some(
                    json!({"kind": "edit-error", "path": resolved.display().to_string()}),
                ),
                is_error: true,
            });
        }
        if !replace_all && count > 1 {
            return Ok(ToolResult {
                tool_use_id: call_id.into(),
                model_output: format!(
                    "ERROR: old_string is not unique ({} matches in {}); pass replace_all=true to replace every occurrence",
                    count,
                    resolved.display()
                ),
                display: Some(json!({"kind": "edit-error", "path": resolved.display().to_string()})),
                is_error: true,
            });
        }
        let updated = if replace_all {
            txt.replace(old, new)
        } else {
            txt.replacen(old, new, 1)
        };
        tokio::fs::write(&resolved, &updated).await?;
        Ok(ToolResult {
            tool_use_id: call_id.into(),
            model_output: format!(
                "Edited {}: replaced {} occurrence{}",
                resolved.display(),
                count.min(if replace_all { count } else { 1 }),
                if replace_all && count > 1 { "s" } else { "" }
            ),
            display: Some(json!({
                "kind": "edit",
                "path": resolved.display().to_string(),
                "occurrences": if replace_all { count } else { 1 }
            })),
            is_error: false,
        })
    }
}
