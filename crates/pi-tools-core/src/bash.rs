use async_trait::async_trait;
use pi_tool_types::{ToolResult, ToolSpec};
use serde_json::{json, Value};
use std::process::Stdio;

use crate::{resolve_path, truncate_for_model, Tool, ToolContext, ToolError};

/// Hard cap on `bash.timeout_ms` per RFD 0027 §4.5 #7 (Hardening H4).
/// 10 minutes. Stops a model from passing `timeout_ms = u64::MAX` and
/// holding a worker indefinitely.
pub const BASH_MAX_TIMEOUT_MS: u64 = 600_000;

/// Per RFD 0027 §4.5 #7 (Hardening H4): clamp a model-supplied
/// `timeout_ms` to [`BASH_MAX_TIMEOUT_MS`]. Extracted from the invoke
/// body so the arithmetic is unit-testable without spinning up a real
/// 10-minute bash process (per code-review finding #2, pass-2).
pub fn clamp_timeout_ms(requested: u64) -> u64 {
    requested.min(BASH_MAX_TIMEOUT_MS)
}

/// Default per-tool input size cap (RFD 0027 §4.5 #7). Applies before
/// validation; an oversized input is rejected outright. 64 KiB is large
/// enough for any reasonable bash command but blocks adversarial mega-
/// payloads that would balloon log output and provider context.
pub const BASH_MAX_INPUT_BYTES: usize = 64 * 1024;

pub struct BashTool;

#[async_trait]
impl Tool for BashTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "bash".into(),
            description: "Run a shell command synchronously and return stdout, stderr, and exit code. Use `timeout_ms` for a hard limit (default 120000, max 600000). The `cwd` argument is jailed to the agent's working directory; NOTE: only the cwd argument is checked, the shell command body itself is unrestricted (a model can still `cd /etc; cat shadow` once bash starts). For real isolation use a microvm or remote sandbox provider.".into(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "command": {"type": "string"},
                    "timeout_ms": {"type": "integer", "default": 120000, "maximum": 600000},
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
        // Per RFD 0027 §4.5 #7 (Hardening H4): reject oversized
        // inputs at the gate so we don't allocate a multi-megabyte
        // command string before validation.
        let input_str = serde_json::to_string(&input)
            .map_err(|e| ToolError::InvalidInput(format!("input not serialisable: {e}")))?;
        if input_str.len() > BASH_MAX_INPUT_BYTES {
            return Err(ToolError::InvalidInput(format!(
                "bash input exceeds per-tool cap: {} bytes > {} bytes",
                input_str.len(),
                BASH_MAX_INPUT_BYTES
            )));
        }

        let command = input
            .get("command")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ToolError::InvalidInput("missing `command`".into()))?;

        // Per RFD 0027 §4.5 #7 (Hardening H4): clamp timeout_ms to
        // 600_000 (10 minutes). Pre-H4 a model could pass
        // `timeout_ms: u64::MAX` and the worker would block indefinitely.
        let timeout_ms = clamp_timeout_ms(
            input.get("timeout_ms").and_then(|v| v.as_u64()).unwrap_or(120_000),
        );

        // Per RFD 0027 §4.5 #6 (Hardening H4): cwd jail check.
        // Pre-H4 the model could pass `cwd: "../../../"` and run
        // commands outside the embedder's intended directory. Now we
        // canonicalize the requested cwd against the canonicalized
        // ctx.cwd and reject anything that escapes.
        let cwd = match input.get("cwd").and_then(|v| v.as_str()) {
            Some(p) => {
                let requested = resolve_path(ctx, p);
                jail_check(&requested, &ctx.cwd)?
            }
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

/// Per RFD 0027 §4.5 #6 (Hardening H4): canonicalize `requested`
/// against `jail_root` and reject if `requested` does not have
/// `jail_root` as a prefix.
///
/// Both paths are canonicalized first to defeat `..` segments,
/// symlink escapes, and case-insensitive shenanigans on macOS.
/// If either canonicalization fails (e.g. because the path doesn't
/// exist), we treat that as a jail violation rather than silently
/// permitting an unverifiable cwd.
fn jail_check(
    requested: &std::path::Path,
    jail_root: &std::path::Path,
) -> Result<std::path::PathBuf, ToolError> {
    let canon_jail = jail_root.canonicalize().map_err(|e| {
        ToolError::InvalidInput(format!(
            "ctx.cwd `{}` does not canonicalize: {e}",
            jail_root.display()
        ))
    })?;
    let canon_requested = requested.canonicalize().map_err(|e| {
        ToolError::InvalidInput(format!(
            "requested cwd `{}` does not canonicalize (does it exist?): {e}",
            requested.display()
        ))
    })?;
    if !canon_requested.starts_with(&canon_jail) {
        return Err(ToolError::InvalidInput(format!(
            "requested cwd `{}` escapes the agent's working directory `{}`",
            canon_requested.display(),
            canon_jail.display()
        )));
    }
    Ok(canon_requested)
}

#[cfg(test)]
mod h4_tests {
    use super::*;

    #[test]
    fn jail_check_accepts_subdirectory() {
        let tmp = tempfile::tempdir().unwrap();
        let sub = tmp.path().join("subdir");
        std::fs::create_dir_all(&sub).unwrap();
        let result = jail_check(&sub, tmp.path()).expect("subdir should be allowed");
        assert!(result.starts_with(tmp.path().canonicalize().unwrap()));
    }

    #[test]
    fn jail_check_accepts_jail_root_itself() {
        let tmp = tempfile::tempdir().unwrap();
        let result = jail_check(tmp.path(), tmp.path()).expect("root itself should be allowed");
        assert_eq!(result, tmp.path().canonicalize().unwrap());
    }

    #[test]
    fn jail_check_rejects_parent_traversal() {
        let tmp = tempfile::tempdir().unwrap();
        let sub = tmp.path().join("inside");
        std::fs::create_dir_all(&sub).unwrap();
        // Build "inside/../../" which canonicalizes outside the jail.
        let escape = sub.join("..").join("..");
        let result = jail_check(&escape, &sub);
        assert!(result.is_err(), "parent traversal must be rejected");
        let msg = result.unwrap_err().to_string();
        assert!(msg.contains("escapes"), "got error: {msg}");
    }

    #[test]
    fn jail_check_rejects_nonexistent_path() {
        let tmp = tempfile::tempdir().unwrap();
        let bogus = tmp.path().join("does-not-exist");
        let result = jail_check(&bogus, tmp.path());
        assert!(result.is_err(), "nonexistent paths must be rejected");
    }

    #[tokio::test]
    async fn bash_rejects_oversized_input() {
        let tool = BashTool;
        let huge = "X".repeat(BASH_MAX_INPUT_BYTES + 1);
        let input = json!({"command": huge});
        let ctx = ToolContext {
            cwd: std::env::current_dir().unwrap(),
            max_output_bytes: 256 * 1024,
        };
        let res = tool.invoke(&ctx, "id", input).await;
        assert!(res.is_err(), "oversized input must be rejected");
        let msg = format!("{}", res.unwrap_err());
        assert!(msg.contains("exceeds per-tool cap"), "got: {msg}");
    }

    #[tokio::test]
    async fn bash_clamps_timeout_to_max_smoke() {
        // Smoke check: u64::MAX + `true` doesn't overflow / panic.
        let tool = BashTool;
        let input = json!({"command": "true", "timeout_ms": u64::MAX});
        let ctx = ToolContext {
            cwd: std::env::current_dir().unwrap(),
            max_output_bytes: 256 * 1024,
        };
        let res = tool.invoke(&ctx, "id", input).await.expect("should run");
        assert!(!res.is_error, "true should exit 0");
    }

    #[test]
    fn clamp_timeout_arithmetic_caps_at_max() {
        // Per code-review finding #2 (pass-2): exercise the clamp
        // arithmetic directly. A future refactor that drops `.min()`
        // ships red here without paying the 10-minute integration-test
        // cost of running an actual `sleep 600` bash command.
        assert_eq!(clamp_timeout_ms(0), 0);
        assert_eq!(clamp_timeout_ms(1), 1);
        assert_eq!(clamp_timeout_ms(120_000), 120_000);
        assert_eq!(clamp_timeout_ms(BASH_MAX_TIMEOUT_MS), BASH_MAX_TIMEOUT_MS);
        assert_eq!(clamp_timeout_ms(BASH_MAX_TIMEOUT_MS + 1), BASH_MAX_TIMEOUT_MS);
        assert_eq!(clamp_timeout_ms(u64::MAX), BASH_MAX_TIMEOUT_MS);
    }

    #[tokio::test]
    async fn bash_rejects_cwd_outside_ctx() {
        let tmp = tempfile::tempdir().unwrap();
        let escape_root = tempfile::tempdir().unwrap();
        let tool = BashTool;
        let input = json!({
            "command": "true",
            "cwd": escape_root.path().to_string_lossy(),
        });
        let ctx = ToolContext {
            cwd: tmp.path().to_path_buf(),
            max_output_bytes: 256 * 1024,
        };
        let res = tool.invoke(&ctx, "id", input).await;
        assert!(res.is_err(), "cwd outside ctx.cwd must be rejected");
        let msg = format!("{}", res.unwrap_err());
        assert!(msg.contains("escapes"), "got: {msg}");
    }

    #[tokio::test]
    async fn bash_accepts_cwd_inside_ctx() {
        let tmp = tempfile::tempdir().unwrap();
        let sub = tmp.path().join("subdir");
        std::fs::create_dir_all(&sub).unwrap();
        let tool = BashTool;
        let input = json!({
            "command": "pwd",
            "cwd": "subdir",
        });
        let ctx = ToolContext {
            cwd: tmp.path().to_path_buf(),
            max_output_bytes: 256 * 1024,
        };
        let res = tool.invoke(&ctx, "id", input).await.expect("should run");
        assert!(!res.is_error, "subdir cwd should run successfully");
        // pwd output should reflect the subdir (canonicalized).
        let canon_sub = sub.canonicalize().unwrap();
        let out = res.model_output.clone();
        assert!(
            out.contains(canon_sub.to_str().unwrap()),
            "pwd output should mention subdir, got: {out}"
        );
    }

}
