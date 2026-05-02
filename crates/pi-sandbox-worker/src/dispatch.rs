//! Tool dispatch: take a ToolRequest, run the named tool from
//! pi-tools-core's registry, build a ToolResponse.
//!
//! Three worker-side invariants enforced here, independent of
//! pi-tools-core internals:
//!
//! 1. **Error text in `stdout`** — `stdout` is the model-facing field
//!    (protocol doc: "tool's stdout / model-facing output text"). When we
//!    generate an error ourselves (unknown tool, tool error, timeout) we
//!    set `stdout` to the human-readable message and mirror it to `stderr`
//!    for diagnostics.
//!
//! 2. **Path boundary** — every path-valued field in `tool_input` is
//!    checked to remain inside `work_dir` after normalisation. Requests
//!    that escape the sandbox are rejected before the tool runs.
//!
//! 3. **Bash process cleanup** — `BashTool` uses `tokio::process::Command`
//!    without `kill_on_drop`, so dropping the future on timeout leaves the
//!    child running. We work around this by injecting a `timeout_ms` budget
//!    into bash inputs (so BashTool's own timer fires first) **and** by
//!    prepending `timeout <N>s` to the shell command so the Linux `timeout`
//!    utility sends SIGTERM/SIGKILL to the child process tree.

use pi_sandbox_protocol::{ToolRequest, ToolResponse};
use pi_tools_core::{ToolContext, ToolRegistry};
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use tracing::warn;

static REGISTRY: OnceLock<ToolRegistry> = OnceLock::new();

fn registry() -> &'static ToolRegistry {
    REGISTRY.get_or_init(ToolRegistry::with_extras)
}

// --------------------------------------------------------------------------
// Path boundary validation
// --------------------------------------------------------------------------

/// Normalise `p` (resolving `..` without touching the filesystem) and verify
/// it starts with `work_dir`. Returns an error string on violation.
fn check_path_within(p: &str, work_dir: &Path) -> Result<(), String> {
    let raw = if Path::new(p).is_absolute() {
        PathBuf::from(p)
    } else {
        work_dir.join(p)
    };

    // Manually normalise `..` so we can check paths that don't exist yet.
    let mut parts: Vec<std::path::Component<'_>> = Vec::new();
    for component in raw.components() {
        use std::path::Component;
        match component {
            Component::ParentDir => {
                if parts.pop().is_none() {
                    return Err(format!(
                        "path '{}' escapes the sandbox root '{}'",
                        p,
                        work_dir.display()
                    ));
                }
            }
            Component::CurDir => {}
            other => parts.push(other),
        }
    }
    let resolved: PathBuf = parts.iter().collect();

    if !resolved.starts_with(work_dir) {
        return Err(format!(
            "path '{}' (resolved: '{}') is outside the sandbox root '{}'",
            p,
            resolved.display(),
            work_dir.display()
        ));
    }
    Ok(())
}

/// Path-valued input keys we validate for each tool.
const PATH_KEYS: &[&str] = &["path", "cwd"];

/// Check all path-valued keys in `input` against `work_dir`.
fn validate_paths(tool_name: &str, input: &serde_json::Value, work_dir: &Path) -> Result<(), String> {
    for &key in PATH_KEYS {
        if let Some(val) = input.get(key).and_then(|v| v.as_str()) {
            check_path_within(val, work_dir)
                .map_err(|e| format!("[{tool_name}] {e}"))?;
        }
    }
    Ok(())
}

// --------------------------------------------------------------------------
// Bash process-kill hardening
// --------------------------------------------------------------------------

/// Wrap the bash command with the Linux `timeout` utility so the OS kills
/// the child (and its process group) when the budget runs out.
///
/// Strategy: let the OS `timeout` command fire at `timeout_ms`, which kills
/// the child and exits with code 124. BashTool's internal tokio timer is set
/// 500 ms longer so it doesn't race with the OS kill — BashTool will see the
/// 124 exit from the `timeout` wrapper and surface it correctly.
fn harden_bash_input(input: serde_json::Value, timeout_ms: u32) -> serde_json::Value {
    let mut obj = match input {
        serde_json::Value::Object(m) => m,
        other => return other, // not an object — let BashTool reject it
    };

    // `timeout(1)` needs whole seconds; round up to at least 1 s.
    let secs = (timeout_ms / 1000).max(1);

    // Wrap original command: `timeout --kill-after=<secs+1>s <secs>s <cmd>`.
    // `--kill-after` sends SIGKILL if SIGTERM alone doesn't stop the process,
    // ensuring cleanup even for signal-ignoring children.
    if let Some(cmd_val) = obj.get("command").cloned() {
        if let Some(cmd) = cmd_val.as_str() {
            let wrapped = format!("timeout --kill-after={}s {}s {}", secs + 1, secs, cmd);
            obj.insert("command".to_string(), serde_json::Value::String(wrapped));
        }
    }

    // Set BashTool's internal tokio timeout 500 ms *after* the OS `timeout`
    // command fires. This lets the OS kill the child and return exit 124,
    // which BashTool then reports correctly via output.status.code().
    let inner_ms = timeout_ms.saturating_add(500);
    obj.entry("timeout_ms".to_string())
        .or_insert(serde_json::Value::Number(inner_ms.into()));

    serde_json::Value::Object(obj)
}

// --------------------------------------------------------------------------
// Main entry point
// --------------------------------------------------------------------------

pub async fn dispatch_request(req: ToolRequest, work_dir: &Path) -> ToolResponse {
    let call_id = req.call_id.clone();

    // Look up the tool by name.
    let Some(tool) = registry().get(&req.tool_name) else {
        let msg = format!("unknown tool: {}", req.tool_name);
        return ToolResponse {
            call_id,
            stdout: msg.clone(), // model-facing
            stderr: msg,         // diagnostic mirror
            exit_status: 1,
            guest_duration_ms: 0,
            is_error: true,
        };
    };

    // Validate all path-valued inputs before running anything.
    if let Err(e) = validate_paths(&req.tool_name, &req.tool_input, work_dir) {
        warn!(tool = %req.tool_name, err = %e, "path boundary violation; rejecting request");
        let msg = format!("sandbox error: {e}");
        return ToolResponse {
            call_id,
            stdout: msg.clone(),
            stderr: msg,
            exit_status: 1,
            guest_duration_ms: 0,
            is_error: true,
        };
    }

    // For bash, harden the input so child processes are reliably killed on
    // timeout (Linux `timeout` utility + BashTool's own internal timer).
    let tool_input = if req.tool_name == "bash" {
        harden_bash_input(req.tool_input, req.timeout_ms)
    } else {
        req.tool_input
    };

    // Build a ToolContext scoped to work_dir.
    let ctx = ToolContext {
        cwd: work_dir.to_path_buf(),
        max_output_bytes: req.max_output_bytes as usize,
    };

    // Outer worker-level timeout as a safety net (bash is handled above;
    // this catches non-bash tools that run too long).
    let timeout = std::time::Duration::from_millis(req.timeout_ms as u64);
    let invoke_fut = tool.invoke(&ctx, &req.call_id, tool_input);
    match tokio::time::timeout(timeout, invoke_fut).await {
        Ok(Ok(result)) => ToolResponse {
            call_id,
            stdout: result.model_output,
            stderr: String::new(),
            exit_status: if result.is_error { 1 } else { 0 },
            guest_duration_ms: 0, // listener overrides
            is_error: result.is_error,
        },
        Ok(Err(e)) => {
            let msg = format!("tool error: {e}");
            ToolResponse {
                call_id,
                stdout: msg.clone(),
                stderr: msg,
                exit_status: 1,
                guest_duration_ms: 0,
                is_error: true,
            }
        }
        Err(_) => {
            let msg = format!("tool timed out after {} ms", req.timeout_ms);
            ToolResponse {
                call_id,
                stdout: msg.clone(),
                stderr: msg,
                exit_status: 124, // standard timeout exit code
                guest_duration_ms: 0,
                is_error: true,
            }
        }
    }
}
