use async_trait::async_trait;
use pi_tool_types::{ToolResult, ToolSpec};
use serde_json::{json, Value};
use std::process::Stdio;

use crate::{resolve_path, truncate_for_model, Tool, ToolContext, ToolError};

pub struct BashTool;

#[async_trait]
impl Tool for BashTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "bash".into(),
            description: "Run a shell command synchronously and return stdout, stderr, and exit code. Use `timeout_ms` for a hard limit (default 120000).".into(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "command": {"type": "string"},
                    "timeout_ms": {"type": "integer", "default": 120000},
                    "cwd": {"type": "string"}
                },
                "required": ["command"]
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
        let command = input
            .get("command")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ToolError::InvalidInput("missing `command`".into()))?;
        let timeout_ms = input
            .get("timeout_ms")
            .and_then(|v| v.as_u64())
            .unwrap_or(120_000);
        let cwd = match input.get("cwd").and_then(|v| v.as_str()) {
            Some(p) => resolve_path(ctx, p),
            None => ctx.cwd.clone(),
        };

        let mut cmd = tokio::process::Command::new("bash");
        cmd.arg("-lc")
            .arg(command)
            .current_dir(&cwd)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());

        let child = cmd.spawn().map_err(ToolError::Io)?;
        let output = match tokio::time::timeout(
            std::time::Duration::from_millis(timeout_ms),
            child.wait_with_output(),
        )
        .await
        {
            Ok(res) => res?,
            Err(_) => {
                return Ok(ToolResult {
                    tool_use_id: call_id.into(),
                    model_output: format!(
                        "ERROR: command timed out after {}ms\n$ {}",
                        timeout_ms, command
                    ),
                    display: Some(json!({"kind": "bash", "timeout": true, "command": command})),
                    is_error: true,
                });
            }
        };

        let stdout = String::from_utf8_lossy(&output.stdout).to_string();
        let stderr = String::from_utf8_lossy(&output.stderr).to_string();
        let code = output.status.code().unwrap_or(-1);

        let mut model_output = String::new();
        if !stdout.is_empty() {
            model_output.push_str(&stdout);
        }
        if !stderr.is_empty() {
            if !model_output.is_empty() && !model_output.ends_with('\n') {
                model_output.push('\n');
            }
            model_output.push_str("[stderr]\n");
            model_output.push_str(&stderr);
        }
        if model_output.is_empty() {
            model_output.push_str("(no output)");
        }
        model_output.push_str(&format!("\n\n[exit {}]", code));

        let truncated = truncate_for_model(&model_output, ctx.max_output_bytes);

        Ok(ToolResult {
            tool_use_id: call_id.into(),
            model_output: truncated,
            display: Some(json!({
                "kind": "bash",
                "command": command,
                "exit": code,
                "cwd": cwd.display().to_string(),
            })),
            is_error: code != 0,
        })
    }
}
