use async_trait::async_trait;
use ignore::WalkBuilder;
use pi_ai::{ToolResult, ToolSpec};
use serde_json::{json, Value};

use crate::{resolve_path, truncate_for_model, Tool, ToolContext, ToolError};

pub struct FindTool;

#[async_trait]
impl Tool for FindTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "find".into(),
            description: "Find files matching a glob pattern. Honours .gitignore.".into(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "glob": {"type": "string"},
                    "path": {"type": "string", "description": "search root (default cwd)"},
                    "max_results": {"type": "integer", "default": 500}
                },
                "required": ["glob"]
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
        let glob_pat = input
            .get("glob")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ToolError::InvalidInput("missing `glob`".into()))?;
        let max = input
            .get("max_results")
            .and_then(|v| v.as_u64())
            .unwrap_or(500) as usize;
        let path = match input.get("path").and_then(|v| v.as_str()) {
            Some(p) => resolve_path(ctx, p),
            None => ctx.cwd.clone(),
        };
        let pat =
            glob::Pattern::new(glob_pat).map_err(|e| ToolError::InvalidInput(e.to_string()))?;
        let mut out = String::new();
        let mut count = 0;
        for ent in WalkBuilder::new(&path)
            .standard_filters(true)
            .hidden(false)
            .build()
            .flatten()
        {
            if pat.matches_path(ent.path()) {
                out.push_str(&format!("{}\n", ent.path().display()));
                count += 1;
                if count >= max {
                    break;
                }
            }
        }
        if out.is_empty() {
            out.push_str("(no matches)\n");
        }
        Ok(ToolResult {
            tool_use_id: call_id.into(),
            model_output: truncate_for_model(&out, ctx.max_output_bytes),
            display: Some(json!({"kind": "find", "glob": glob_pat, "matches": count})),
            is_error: false,
        })
    }
}
