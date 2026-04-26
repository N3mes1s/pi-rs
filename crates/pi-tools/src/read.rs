use async_trait::async_trait;
use pi_ai::{ToolResult, ToolSpec};
use serde_json::{json, Value};

use crate::{resolve_path, truncate_for_model, Tool, ToolContext, ToolError};

pub struct ReadTool;

#[async_trait]
impl Tool for ReadTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "read".into(),
            description:
                "Read the contents of a file. Supports text and image files. For large files use offset/limit (in lines)."
                    .into(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "path": {"type": "string", "description": "absolute or cwd-relative path"},
                    "offset": {"type": "integer", "description": "1-based starting line", "default": 1},
                    "limit": {"type": "integer", "description": "max lines to return", "default": 2000}
                },
                "required": ["path"]
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
        let path = input
            .get("path")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ToolError::InvalidInput("missing `path`".into()))?;
        let offset = input.get("offset").and_then(|v| v.as_u64()).unwrap_or(1).max(1) as usize;
        let limit = input
            .get("limit")
            .and_then(|v| v.as_u64())
            .unwrap_or(2000) as usize;
        let resolved = resolve_path(ctx, path);

        // Image attachments are returned base64 in display so the agent loop
        // can re-package them as Anthropic-style image content blocks.
        let ext = resolved
            .extension()
            .and_then(|e| e.to_str())
            .map(|e| e.to_lowercase())
            .unwrap_or_default();
        let is_image = matches!(ext.as_str(), "png" | "jpg" | "jpeg" | "gif" | "webp");
        if is_image {
            let bytes = tokio::fs::read(&resolved).await?;
            use base64::Engine;
            let b64 = base64::engine::general_purpose::STANDARD.encode(&bytes);
            let mime = match ext.as_str() {
                "png" => "image/png",
                "gif" => "image/gif",
                "webp" => "image/webp",
                _ => "image/jpeg",
            };
            return Ok(ToolResult {
                tool_use_id: call_id.into(),
                model_output: format!("[image {} ({} bytes)]", resolved.display(), bytes.len()),
                display: Some(json!({
                    "kind": "image",
                    "mime": mime,
                    "base64": b64,
                    "path": resolved.display().to_string(),
                })),
                is_error: false,
            });
        }

        let txt = tokio::fs::read_to_string(&resolved).await?;
        let total_lines = txt.lines().count();
        let selected: String = txt
            .lines()
            .enumerate()
            .skip(offset - 1)
            .take(limit)
            .map(|(i, line)| format!("{:>6}\t{}\n", i + 1, line))
            .collect();
        let model_output = truncate_for_model(&selected, ctx.max_output_bytes);
        Ok(ToolResult {
            tool_use_id: call_id.into(),
            model_output,
            display: Some(json!({
                "kind": "file",
                "path": resolved.display().to_string(),
                "total_lines": total_lines,
                "offset": offset,
                "limit": limit,
            })),
            is_error: false,
        })
    }
}
