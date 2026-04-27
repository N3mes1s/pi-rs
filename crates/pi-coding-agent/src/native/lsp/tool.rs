//! `lsp` tool — agent-facing surface over [`super::engine::LspEngine`]
//! (D1.tool).
//!
//! Input dispatches on `op`, mirroring the 11 entries in
//! [`super::ops::LspOp`]. Most ops carry `path` plus optional
//! `line` / `col` (0-indexed, matching LSP). Output is the raw JSON
//! reply from the language server, shaped like:
//!
//! ```jsonc
//! { "ok": true, "op": "definition", "result": <server reply> }
//! ```
//!
//! Errors come back as `is_error: true` with a textual model_output —
//! we never bubble a transport panic up to the agent.
//!
//! Engine lifetime: the tool keeps an `OnceCell<Arc<LspEngine>>` so the
//! engine is materialised on first call and shared across subsequent
//! invocations. The engine's pinned workspace root is taken from
//! `ToolContext::cwd` at first-call time. Per-process LSP config is
//! loaded by the constructor (defaults to `LspConfig::default()`,
//! which has the master switch *off* — the user opts in).

use async_trait::async_trait;
use pi_ai::{ToolResult, ToolSpec};
use pi_tools::{Tool, ToolContext, ToolError};
use serde_json::{json, Value};
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::OnceCell;

use super::config::LspConfig;
use super::engine::{EngineError, LspEngine};
use super::ops::LspOp;

pub struct LspTool {
    config: LspConfig,
    engine: OnceCell<Arc<LspEngine>>,
}

impl LspTool {
    pub fn new(config: LspConfig) -> Self {
        Self {
            config,
            engine: OnceCell::new(),
        }
    }

    async fn engine_for(&self, ctx: &ToolContext) -> &Arc<LspEngine> {
        self.engine
            .get_or_init(|| async {
                Arc::new(LspEngine::new(self.config.clone(), ctx.cwd.clone()))
            })
            .await
    }
}

#[async_trait]
impl Tool for LspTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "lsp".into(),
            description:
                "Query a language server (rust-analyzer, pyright, gopls, …) over its \
                 standard JSON-RPC stdio interface. Set `op` to one of: `diagnostics`, \
                 `definition`, `type_definition`, `implementation`, `references`, \
                 `hover`, `symbols`, `rename`, `code_actions`, `status`, `reload`. \
                 Most ops want `path` (absolute) plus 0-indexed `line` / `col`. \
                 Returns the raw LSP reply as JSON; `is_error: true` if the server \
                 isn't running, the language is disabled in config, or the request \
                 failed."
                    .into(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "op":     { "type": "string", "enum": [
                        "diagnostics", "definition", "type_definition",
                        "implementation", "references", "hover", "symbols",
                        "rename", "code_actions", "status", "reload",
                    ]},
                    "path":     { "type": "string", "description": "Absolute file path. Required for everything except `status` and `reload`." },
                    "line":     { "type": "integer", "minimum": 0, "description": "0-indexed line number (LSP semantics)." },
                    "col":      { "type": "integer", "minimum": 0, "description": "0-indexed character offset within the line." },
                    "language": { "type": "string", "description": "For `reload`: which server to drop. Optional otherwise." },
                    "new_name": { "type": "string", "description": "For `rename`: the replacement identifier." }
                },
                "required": ["op"]
            }),
        }
    }

    fn read_only(&self) -> bool {
        // The transport itself never mutates files; `rename` returns
        // edits the agent then chooses to apply via existing file
        // tools, so even that op is observation-only at this layer.
        true
    }

    async fn invoke(
        &self,
        ctx: &ToolContext,
        call_id: &str,
        input: Value,
    ) -> Result<ToolResult, ToolError> {
        let op_s = input
            .get("op")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ToolError::InvalidInput("missing `op`".into()))?
            .to_string();
        let op = LspOp::parse(&op_s)
            .ok_or_else(|| ToolError::InvalidInput(format!("unknown op `{op_s}`")))?;

        let engine = self.engine_for(ctx).await.clone();

        let result: std::result::Result<Value, EngineError> = match op {
            LspOp::Status => Ok(json!({ "running": engine.running_languages().await })),
            LspOp::Reload => {
                let lang = input
                    .get("language")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| ToolError::InvalidInput("reload: `language` required".into()))?;
                let dropped = engine.reload(lang).await;
                Ok(json!({ "language": lang, "dropped": dropped }))
            }
            other => {
                let path = require_path(&input)?;
                match other {
                    LspOp::Diagnostics => engine.diagnostics(&path).await,
                    LspOp::Definition => {
                        let (line, col) = require_pos(&input)?;
                        engine.definition(&path, line, col).await
                    }
                    LspOp::TypeDefinition | LspOp::Implementation => {
                        // Same wire shape as `definition`; engine doesn't
                        // expose those yet, so report not-implemented
                        // cleanly rather than silently doing the wrong
                        // thing.
                        return Ok(tool_error(call_id, &op_s, "not yet wired through the engine — track at lsp/engine.rs"));
                    }
                    LspOp::References => {
                        let (line, col) = require_pos(&input)?;
                        engine.references(&path, line, col).await
                    }
                    LspOp::Hover => {
                        let (line, col) = require_pos(&input)?;
                        engine.hover(&path, line, col).await
                    }
                    LspOp::Symbols => engine.symbols(&path).await,
                    LspOp::Rename | LspOp::CodeActions => {
                        return Ok(tool_error(call_id, &op_s, "not yet wired through the engine — track at lsp/engine.rs"));
                    }
                    LspOp::Status | LspOp::Reload => unreachable!(),
                }
            }
        };

        match result {
            Ok(v) => Ok(ToolResult {
                tool_use_id: call_id.into(),
                model_output: format!("{op_s}: ok"),
                display: Some(json!({ "ok": true, "op": op_s, "result": v })),
                is_error: false,
            }),
            Err(e) => Ok(tool_error(call_id, &op_s, &e.to_string())),
        }
    }
}

fn require_path(input: &Value) -> Result<PathBuf, ToolError> {
    let s = input
        .get("path")
        .and_then(|v| v.as_str())
        .ok_or_else(|| ToolError::InvalidInput("missing `path`".into()))?;
    Ok(PathBuf::from(s))
}

fn require_pos(input: &Value) -> Result<(u32, u32), ToolError> {
    let line = input
        .get("line")
        .and_then(|v| v.as_u64())
        .ok_or_else(|| ToolError::InvalidInput("missing `line`".into()))?;
    let col = input
        .get("col")
        .and_then(|v| v.as_u64())
        .ok_or_else(|| ToolError::InvalidInput("missing `col`".into()))?;
    Ok((line as u32, col as u32))
}

fn tool_error(call_id: &str, op: &str, msg: &str) -> ToolResult {
    ToolResult {
        tool_use_id: call_id.into(),
        model_output: format!("{op}: {msg}"),
        display: Some(json!({ "ok": false, "op": op, "error": msg })),
        is_error: true,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn ctx() -> ToolContext {
        ToolContext::default()
    }

    #[tokio::test]
    async fn missing_op_is_invalid_input() {
        let tool = LspTool::new(LspConfig::default());
        let err = tool.invoke(&ctx(), "c1", json!({})).await.unwrap_err();
        assert!(matches!(err, ToolError::InvalidInput(_)));
    }

    #[tokio::test]
    async fn unknown_op_is_invalid_input() {
        let tool = LspTool::new(LspConfig::default());
        let err = tool
            .invoke(&ctx(), "c1", json!({"op": "explode"}))
            .await
            .unwrap_err();
        assert!(matches!(err, ToolError::InvalidInput(_)));
    }

    #[tokio::test]
    async fn status_op_works_without_any_servers_running() {
        let mut cfg = LspConfig::default();
        cfg.enabled = true;
        let tool = LspTool::new(cfg);
        let res = tool
            .invoke(&ctx(), "c1", json!({"op": "status"}))
            .await
            .unwrap();
        assert!(!res.is_error);
        let display = res.display.unwrap();
        assert_eq!(display["ok"], json!(true));
        assert_eq!(display["op"], json!("status"));
        assert_eq!(display["result"]["running"], json!([]));
    }

    #[tokio::test]
    async fn disabled_language_yields_clean_is_error() {
        // master off; ask for diagnostics on a .rs file.
        let tool = LspTool::new(LspConfig::default());
        let res = tool
            .invoke(
                &ctx(),
                "c1",
                json!({"op": "diagnostics", "path": "/tmp/x.rs"}),
            )
            .await
            .unwrap();
        assert!(res.is_error, "disabled config should be a clean error");
        let d = res.display.unwrap();
        assert_eq!(d["ok"], json!(false));
        assert!(d["error"].as_str().unwrap().contains("disabled"));
    }

    #[tokio::test]
    async fn relative_path_is_a_clean_is_error() {
        let mut cfg = LspConfig::default();
        cfg.enabled = true;
        let tool = LspTool::new(cfg);
        let res = tool
            .invoke(
                &ctx(),
                "c1",
                json!({"op": "diagnostics", "path": "rel/path.rs"}),
            )
            .await
            .unwrap();
        assert!(res.is_error);
    }

    #[tokio::test]
    async fn definition_requires_line_and_col() {
        let mut cfg = LspConfig::default();
        cfg.enabled = true;
        let tool = LspTool::new(cfg);
        let err = tool
            .invoke(
                &ctx(),
                "c1",
                json!({"op": "definition", "path": "/tmp/x.rs"}),
            )
            .await
            .unwrap_err();
        assert!(matches!(err, ToolError::InvalidInput(_)));
    }

    #[tokio::test]
    async fn reload_op_requires_language_field() {
        let mut cfg = LspConfig::default();
        cfg.enabled = true;
        let tool = LspTool::new(cfg);
        let err = tool
            .invoke(&ctx(), "c1", json!({"op": "reload"}))
            .await
            .unwrap_err();
        assert!(matches!(err, ToolError::InvalidInput(_)));
    }

    #[tokio::test]
    async fn type_definition_reports_not_yet_wired_cleanly() {
        let mut cfg = LspConfig::default();
        cfg.enabled = true;
        let tool = LspTool::new(cfg);
        let res = tool
            .invoke(
                &ctx(),
                "c1",
                json!({"op": "type_definition", "path": "/tmp/x.rs", "line": 0, "col": 0}),
            )
            .await
            .unwrap();
        assert!(res.is_error);
        let d = res.display.unwrap();
        assert!(d["error"].as_str().unwrap().contains("not yet wired"));
    }

    #[test]
    fn spec_input_schema_lists_all_eleven_ops() {
        let tool = LspTool::new(LspConfig::default());
        let spec = tool.spec();
        let ops = spec.input_schema["properties"]["op"]["enum"]
            .as_array()
            .unwrap();
        assert_eq!(ops.len(), 11);
    }
}
