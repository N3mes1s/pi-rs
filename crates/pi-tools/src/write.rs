use async_trait::async_trait;
use pi_ai::{ToolResult, ToolSpec};
use serde_json::{json, Value};

use crate::{resolve_path, Tool, ToolContext, ToolError};

pub struct WriteTool;

#[async_trait]
impl Tool for WriteTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "write".into(),
            description:
                "Write a file, creating parent directories if needed. Overwrites existing files."
                    .into(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "path": {"type": "string"},
                    "content": {"type": "string"}
                },
                "required": ["path", "content"]
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
        let content = input
            .get("content")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ToolError::InvalidInput("missing `content`".into()))?;
        let resolved = resolve_path(ctx, path);
        if let Some(parent) = resolved.parent() {
            tokio::fs::create_dir_all(parent).await?;
        }
        let pre_existed = resolved.exists();
        tokio::fs::write(&resolved, content).await?;
        Ok(ToolResult {
            tool_use_id: call_id.into(),
            model_output: format!(
                "{} {} ({} bytes)",
                if pre_existed { "Updated" } else { "Created" },
                resolved.display(),
                content.len()
            ),
            display: Some(json!({
                "kind": "write",
                "path": resolved.display().to_string(),
                "bytes": content.len(),
                "created": !pre_existed,
            })),
            is_error: false,
        })
    }
}
