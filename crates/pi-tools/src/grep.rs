use async_trait::async_trait;
use ignore::WalkBuilder;
use pi_ai::{ToolResult, ToolSpec};
use regex::Regex;
use serde_json::{json, Value};

use crate::{resolve_path, truncate_for_model, Tool, ToolContext, ToolError};

pub struct GrepTool;

#[async_trait]
impl Tool for GrepTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "grep".into(),
            description: "Recursively search for a regex pattern. Honours .gitignore.".into(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "pattern": {"type": "string"},
                    "path": {"type": "string", "description": "directory or file (default cwd)"},
                    "glob": {"type": "string", "description": "optional path glob filter"},
                    "max_results": {"type": "integer", "default": 200}
                },
                "required": ["pattern"]
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
        let pattern = input
            .get("pattern")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ToolError::InvalidInput("missing `pattern`".into()))?;
        let max = input.get("max_results").and_then(|v| v.as_u64()).unwrap_or(200) as usize;
        let path = match input.get("path").and_then(|v| v.as_str()) {
            Some(p) => resolve_path(ctx, p),
            None => ctx.cwd.clone(),
        };
        let glob = input.get("glob").and_then(|v| v.as_str()).map(|s| s.to_string());
        let re = Regex::new(pattern).map_err(|e| ToolError::InvalidInput(e.to_string()))?;
        let pat = glob.as_deref().map(glob::Pattern::new).transpose().map_err(|e| ToolError::InvalidInput(e.to_string()))?;

        let mut walker = WalkBuilder::new(&path);
        walker.standard_filters(true).hidden(false);
        let mut out = String::new();
        let mut count = 0usize;
        for ent in walker.build().flatten() {
            if !ent.file_type().map(|t| t.is_file()).unwrap_or(false) {
                continue;
            }
            if let Some(p) = &pat {
                if !p.matches_path(ent.path()) {
                    continue;
                }
            }
            if let Ok(text) = std::fs::read_to_string(ent.path()) {
                for (i, line) in text.lines().enumerate() {
                    if re.is_match(line) {
                        out.push_str(&format!(
                            "{}:{}:{}\n",
                            ent.path().display(),
                            i + 1,
                            line.trim_end()
                        ));
                        count += 1;
                        if count >= max {
                            break;
                        }
                    }
                }
            }
            if count >= max {
                break;
            }
        }
        if out.is_empty() {
            out.push_str("(no matches)\n");
        }
        Ok(ToolResult {
            tool_use_id: call_id.into(),
            model_output: truncate_for_model(&out, ctx.max_output_bytes),
            display: Some(json!({"kind": "grep", "pattern": pattern, "matches": count})),
            is_error: false,
        })
    }
}
