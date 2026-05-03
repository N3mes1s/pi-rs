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
//!    child running. We work around this by wrapping the bash command with
//!    BusyBox-compatible `timeout SECS sh -c '<cmd>'` (so the OS sends
//!    SIGTERM to the child process group), and by setting **both** BashTool's
//!    internal timer and the outer worker timeout to fire *after* BusyBox's
//!    whole-second boundary. This guarantees the OS kill completes before
//!    either Rust timeout can drop the future.
//!
//!    Concretely: BusyBox `timeout` fires at `ceil(timeout_ms / 1000)`
//!    seconds. We set BashTool's internal timer to that value + 500 ms, and
//!    the outer worker guard timeout to that value + 1000 ms. The overall
//!    worst-case latency is `ceil(timeout_ms / 1000) + 1` seconds.

use pi_sandbox_protocol::{ToolRequest, ToolResponse};
use pi_tools_core::{ToolContext, ToolRegistry};
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use tracing::warn;

static REGISTRY: OnceLock<ToolRegistry> = OnceLock::new();

fn registry() -> &'static ToolRegistry {
    REGISTRY.get_or_init(ToolRegistry::with_unsafe_extras)
}

// --------------------------------------------------------------------------
// Path boundary validation
// --------------------------------------------------------------------------

/// Normalise `p` (resolving `~` and `..` without touching the filesystem)
/// and verify it starts with `work_dir`. Returns an error string on violation.
///
/// We mirror `pi_tools_core::resolve_path`'s tilde expansion so that inputs
/// like `{"path": "~/x"}` are caught before the tool sees them.
fn check_path_within(p: &str, work_dir: &Path) -> Result<(), String> {
    // Mirror pi_tools_core::resolve_path: expand tilde first.
    let expanded = shellexpand::tilde(p).into_owned();
    let raw = if Path::new(expanded.as_str()).is_absolute() {
        PathBuf::from(expanded)
    } else {
        work_dir.join(expanded)
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

/// Compute the BusyBox whole-second timeout ceiling for a given `timeout_ms`.
/// This is the number of seconds after which the OS `timeout` command fires.
fn busybox_secs(timeout_ms: u32) -> u64 {
    ((timeout_ms as u64 + 999) / 1000).max(1)
}

/// Wrap the bash command so child processes are killed when the budget runs out,
/// and set BashTool's internal timer high enough that it never fires before
/// BusyBox's `timeout` command kills the child.
///
/// BusyBox `timeout` (Alpine guest) accepts: `timeout SECS CMD [ARGS…]` where
/// `SECS` is a whole number. The `timeout` utility exits 124 when it kills the
/// process, which `BashTool` surfaces verbatim. We delegate the entire original
/// command string to `sh -c` to preserve shell syntax (builtins, pipes, `&&`):
///
///   timeout SECS sh -c '<original_command>'
///
/// Single-quotes in the original command are escaped as `'\''`.
///
/// **Timer ordering guarantee**: BusyBox fires at `SECS` seconds.  We set
/// BashTool's internal `timeout_ms` to `SECS * 1000 + 500` so it never races
/// with the OS kill. The caller (`dispatch_request`) uses `outer_bash_timeout`
/// for the outer guard.
fn harden_bash_input(input: serde_json::Value, timeout_ms: u32) -> (serde_json::Value, std::time::Duration) {
    let mut obj = match input {
        serde_json::Value::Object(m) => m,
        other => {
            // Not an object — pass through; outer timeout is just req timeout.
            let outer = std::time::Duration::from_millis(timeout_ms as u64);
            return (other, outer);
        }
    };

    let secs = busybox_secs(timeout_ms);

    // Wrap original command via `sh -c` so compound shell syntax is preserved.
    // BusyBox form: `timeout <secs> sh -c '<cmd>'`
    // We use single-quote escaping: replace each `'` in the command with `'\''`.
    if let Some(cmd_val) = obj.get("command").cloned() {
        if let Some(cmd) = cmd_val.as_str() {
            let escaped = cmd.replace('\'', r"'\''");
            let wrapped = format!("timeout {} sh -c '{}'", secs, escaped);
            obj.insert("command".to_string(), serde_json::Value::String(wrapped));
        }
    }

    // BashTool's internal tokio timer: fires secs * 1000 + 500 ms after start.
    // This is always > the BusyBox SECS boundary, so BusyBox kills the child
    // first and BashTool sees exit 124 rather than its own timeout path.
    //
    // We ALWAYS overwrite (not `or_insert`) so that an explicit `timeout_ms`
    // in tool_input cannot sneak in a sub-BusyBox budget and cause the inner
    // tokio timer to fire before the OS kill completes.
    let bash_internal_ms: u64 = secs * 1000 + 500;
    obj.insert(
        "timeout_ms".to_string(),
        serde_json::Value::Number(bash_internal_ms.into()),
    );

    // Outer worker guard: fires secs * 1000 + 1000 ms, still after BusyBox.
    // This ensures the child is already dead before we can ever drop the future.
    let outer = std::time::Duration::from_millis(secs * 1000 + 1000);

    (serde_json::Value::Object(obj), outer)
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
    // timeout. `harden_bash_input` also returns the outer timeout duration
    // that guarantees the OS kill has fired before we can drop the future.
    let (tool_input, outer_timeout) = if req.tool_name == "bash" {
        harden_bash_input(req.tool_input, req.timeout_ms)
    } else {
        (req.tool_input, std::time::Duration::from_millis(req.timeout_ms as u64))
    };

    // Build a ToolContext scoped to work_dir.
    let ctx = ToolContext {
        cwd: work_dir.to_path_buf(),
        max_output_bytes: req.max_output_bytes as usize,
    };

    let invoke_fut = tool.invoke(&ctx, &req.call_id, tool_input);
    match tokio::time::timeout(outer_timeout, invoke_fut).await {
        Ok(Ok(result)) => {
            // Extract the precise exit code if the tool embedded it in `display`
            // (BashTool does: `"exit": code`). Fall back to 0/1 from is_error.
            let exit_status = result
                .display
                .as_ref()
                .and_then(|d| d.get("exit"))
                .and_then(|v| v.as_i64())
                .map(|n| n as i32)
                .unwrap_or(if result.is_error { 1 } else { 0 });
            ToolResponse {
                call_id,
                stdout: result.model_output,
                stderr: String::new(),
                exit_status,
                guest_duration_ms: 0, // listener overrides
                is_error: result.is_error,
            }
        }
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
