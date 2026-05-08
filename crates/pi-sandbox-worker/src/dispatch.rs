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

use pi_sandbox_protocol::{FileWrite, ToolRequest, ToolResponse};
use pi_search_proto::{
    self as search_proto, FramingError, WebSearchRequest, WebSearchResponse, CURRENT_PROTO_VERSION,
    DEFAULT_MAX_LINE_BYTES, HOST_CID, VSOCK_SEARCH_PORT,
};
use pi_tools_core::{ToolContext, ToolRegistry};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::OnceLock;
use std::time::Instant;
use tokio::io::BufReader;
use tracing::warn;

static REGISTRY: OnceLock<ToolRegistry> = OnceLock::new();

/// Set to true when the worker is running in one-shot stdin/stdout mode
/// (--transport stdin, for the E2B remote sandbox path). Disables the
/// vsock-based web_search proxy which is incompatible with the remote path.
static IS_STDIN_TRANSPORT: AtomicBool = AtomicBool::new(false);

/// Called once from main() before the first dispatch (when --transport stdin).
pub fn set_stdin_transport(v: bool) {
    IS_STDIN_TRANSPORT.store(v, Ordering::Relaxed);
}

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
// web_search vsock proxy (RFD 0023 §"web_search via vsock proxy")
// --------------------------------------------------------------------------

/// Proxy a `web_search` ToolRequest out to the host over vsock so the
/// upstream API key (EXA / Brave / …) never enters the guest. The
/// host's listener at `(HOST_CID, VSOCK_SEARCH_PORT)` invokes the real
/// `WebSearchTool` with its own `AuthStorage`, returns the result.
///
/// Wire shape per `pi_search_proto`: one newline-delimited
/// `WebSearchRequest` out, one newline-delimited `WebSearchResponse`
/// in, channel closed. No long-lived fd in the worker.
async fn proxy_web_search(req: ToolRequest) -> ToolResponse {
    let started = Instant::now();
    let call_id = req.call_id.clone();

    // Translate ToolRequest input → WebSearchRequest. Tool input is a
    // serde_json::Value mirroring the host WebSearchTool's schema:
    // `{ "query": "...", "provider": "exa", "max_results": 5 }`.
    let query = req
        .tool_input
        .get("query")
        .and_then(|v| v.as_str())
        .map(str::to_string);
    let Some(query) = query else {
        let msg = "web_search input missing required `query` field".to_string();
        return ToolResponse {
            call_id,
            stdout: msg.clone(),
            stderr: msg,
            exit_status: 1,
            guest_duration_ms: started.elapsed().as_millis() as u32,
            is_error: true,
            file_writes: vec![],
        };
    };
    let provider = req
        .tool_input
        .get("provider")
        .and_then(|v| v.as_str())
        .map(str::to_string);
    let max_results = req
        .tool_input
        .get("max_results")
        .and_then(|v| v.as_u64())
        .map(|n| n as u32);
    let proxy_req = WebSearchRequest {
        proto_version: CURRENT_PROTO_VERSION,
        call_id: call_id.clone(),
        query,
        provider,
        max_results,
    };

    match do_proxy_call(&proxy_req).await {
        Ok(resp) if resp.error.is_none() => ToolResponse {
            call_id,
            stdout: resp.result_text,
            stderr: String::new(),
            exit_status: 0,
            guest_duration_ms: started.elapsed().as_millis() as u32,
            is_error: false,
            file_writes: vec![],
        },
        Ok(resp) => {
            let err = resp.error.unwrap_or_default();
            let msg = if resp.result_text.is_empty() {
                format!("web_search failed: {err}")
            } else {
                format!("web_search failed: {err}\n{}", resp.result_text)
            };
            ToolResponse {
                call_id,
                stdout: msg.clone(),
                stderr: msg,
                exit_status: 1,
                guest_duration_ms: started.elapsed().as_millis() as u32,
                is_error: true,
                file_writes: vec![],
            }
        }
        Err(e) => {
            let msg = format!("web_search vsock proxy error: {e}");
            warn!(err = %e, "web_search proxy failed");
            ToolResponse {
                call_id,
                stdout: msg.clone(),
                stderr: msg,
                exit_status: 1,
                guest_duration_ms: started.elapsed().as_millis() as u32,
                is_error: true,
                file_writes: vec![],
            }
        }
    }
}

#[cfg(target_os = "linux")]
async fn do_proxy_call(req: &WebSearchRequest) -> Result<WebSearchResponse, ProxyError> {
    use tokio_vsock::{VsockAddr, VsockStream};

    let addr = VsockAddr::new(HOST_CID, VSOCK_SEARCH_PORT);
    let mut stream = VsockStream::connect(addr).await.map_err(ProxyError::Io)?;
    search_proto::write_request(&mut stream, req)
        .await
        .map_err(ProxyError::Frame)?;
    // Newline framing makes a half-close unnecessary; the host's
    // reader stops at `\n`. Just read the response on the same
    // connection.
    let mut reader = BufReader::new(stream);
    let resp = search_proto::read_response(&mut reader, DEFAULT_MAX_LINE_BYTES)
        .await
        .map_err(ProxyError::Frame)?;
    if resp.proto_version != CURRENT_PROTO_VERSION {
        return Err(ProxyError::ProtoMismatch {
            expected: CURRENT_PROTO_VERSION,
            got: resp.proto_version,
        });
    }
    if resp.call_id != req.call_id {
        return Err(ProxyError::CallIdMismatch {
            expected: req.call_id.clone(),
            got: resp.call_id,
        });
    }
    Ok(resp)
}

#[cfg(not(target_os = "linux"))]
async fn do_proxy_call(_req: &WebSearchRequest) -> Result<WebSearchResponse, ProxyError> {
    Err(ProxyError::Unsupported)
}

#[derive(Debug, thiserror::Error)]
enum ProxyError {
    #[error("vsock io: {0}")]
    Io(std::io::Error),
    #[error("framing: {0}")]
    Frame(FramingError),
    #[error("proto version mismatch: expected {expected}, got {got}")]
    ProtoMismatch { expected: u32, got: u32 },
    #[error("call_id mismatch: sent {expected}, got {got}")]
    CallIdMismatch { expected: String, got: String },
    #[cfg(not(target_os = "linux"))]
    #[error("vsock proxy not supported on this platform (Linux only)")]
    Unsupported,
}

// --------------------------------------------------------------------------
// Per-call hygiene
// --------------------------------------------------------------------------

/// Best-effort scratch-path wipe between tool calls — RFD 0023
/// §"Post-call hygiene".
///
/// **What this does:** removes every entry under `/tmp`, `/var/tmp`,
/// and `/root` (keeping the directories themselves so subsequent
/// calls don't ENOENT). Errors are swallowed; a tmpfs-only rootfs
/// makes these unlinks reliably cheap (no journal, no fsync).
///
/// **What this does NOT do:**
/// - Does not reset writes elsewhere in the overlay upper (e.g.
///   `/etc/foo` written by call N stays visible to call N+1). The
///   overlay can't be unmounted and re-mounted while the worker
///   itself runs from it; full reset needs the v1.1 RFD plan with
///   a separate reset agent + `move_mount` survival list +
///   `pivot_root` into a fresh overlay.
/// - Does not kill background processes started via `bash` (e.g.
///   `nohup ... &`). The bash tool already wraps its commands in
///   BusyBox `timeout` which sends SIGTERM to the process group, so
///   well-formed calls clean up; pathological cases require the
///   reset agent.
/// - Does not reset environment variables — bash tool calls each
///   get a fresh subprocess, so env doesn't actually persist
///   across calls today.
///
/// **Pool-level fallback:** the warm pool already retires VMs
/// after `MAX_CALLS=50` invocations or `MAX_AGE=5min`, capping the
/// blast radius of anything this best-effort wipe misses.
async fn pre_call_hygiene() {
    let started = std::time::Instant::now();
    for path in &["/tmp", "/var/tmp", "/root"] {
        let path = std::path::Path::new(path);
        let Ok(entries) = std::fs::read_dir(path) else { continue };
        for entry in entries.flatten() {
            let p = entry.path();
            // Try as-directory first; fall through to file unlink.
            if std::fs::remove_dir_all(&p).is_err() {
                let _ = std::fs::remove_file(&p);
            }
        }
    }
    tracing::debug!(elapsed_us = %started.elapsed().as_micros(), "pre_call_hygiene done");
}

// --------------------------------------------------------------------------
// Main entry point
// --------------------------------------------------------------------------

pub async fn dispatch_request(req: ToolRequest, work_dir: &Path) -> ToolResponse {
    let call_id = req.call_id.clone();

    // Per-call hygiene: wipe writable scratch paths before running
    // the next tool, so files written in call N aren't visible in
    // call N+1. Cheap (a few ms in the empty case); imperfect — see
    // `pre_call_hygiene` doc-comment for what's NOT cleaned and why.
    pre_call_hygiene().await;

    // `web_search` is special: not a guest-side tool. In vsock mode it
    // proxies the call out to the host (RFD 0023 §"web_search via vsock proxy").
    // In stdin/stdout mode (E2B remote sandbox) vsock is not available, so
    // return a clean "not available" error instead of a cryptic connection failure.
    if req.tool_name == "web_search" {
        if IS_STDIN_TRANSPORT.load(Ordering::Relaxed) {
            let msg = "web_search is not available in remote sandbox mode".to_string();
            return ToolResponse {
                call_id,
                stdout: msg.clone(),
                stderr: msg,
                exit_status: 1,
                guest_duration_ms: 0,
                is_error: true,
                file_writes: vec![],
            };
        }
        return proxy_web_search(req).await;
    }

    // Remember tool name and path input for file_writes flushback (write/edit).
    let tool_name = req.tool_name.clone();
    let path_input = req.tool_input.get("path").and_then(|v| v.as_str()).map(str::to_string);

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
            file_writes: vec![],
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
            file_writes: vec![],
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

            // For write/edit tools in stdin-transport mode, populate file_writes
            // so the host can flush the mutation back to the local directory.
            // Per RFD 0026: max 32 KiB per file; bash always returns empty vec.
            // collect_file_writes returns Err(msg) when the file exceeds the cap,
            // which we convert to an is_error ToolResponse here (the enforcement
            // point per RFD 0026 §"Size cap and fallback").
            if IS_STDIN_TRANSPORT.load(Ordering::Relaxed)
                && !result.is_error
                && matches!(tool_name.as_str(), "write" | "edit")
            {
                match collect_file_writes(path_input.as_deref(), work_dir) {
                    Ok(file_writes) => ToolResponse {
                        call_id,
                        stdout: result.model_output,
                        stderr: String::new(),
                        exit_status,
                        guest_duration_ms: 0, // listener overrides
                        is_error: result.is_error,
                        file_writes,
                    },
                    Err(too_large_msg) => ToolResponse {
                        call_id,
                        stdout: too_large_msg.clone(),
                        stderr: too_large_msg,
                        exit_status: 1,
                        guest_duration_ms: 0,
                        is_error: true,
                        file_writes: vec![],
                    },
                }
            } else {
                ToolResponse {
                    call_id,
                    stdout: result.model_output,
                    stderr: String::new(),
                    exit_status,
                    guest_duration_ms: 0, // listener overrides
                    is_error: result.is_error,
                    file_writes: vec![],
                }
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
                file_writes: vec![],
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
                file_writes: vec![],
            }
        }
    }
}

// --------------------------------------------------------------------------
// file_writes helpers (RFD 0026 proto v2)
// --------------------------------------------------------------------------

/// Per-file size cap for inline flushback (32 KiB unencoded, ~43 KiB base64).
const FILE_WRITE_MAX_BYTES: u64 = 32 * 1024;

/// Collect file_writes for a write/edit tool call.
///
/// Returns `Ok(vec)` with the inline base64 payload for the output path, or
/// `Ok(vec![])` when the file doesn't exist / can't be read (silent skip).
/// Returns `Err(message)` when the file exceeds the 32 KiB size cap so the
/// caller can convert that into an `is_error = true` ToolResponse per RFD 0026
/// §"Size cap and fallback". No sentinel `FileWrite` is ever returned in the
/// `Ok` case.
fn collect_file_writes(
    path_input: Option<&str>,
    work_dir: &Path,
) -> Result<Vec<FileWrite>, String> {
    use base64::Engine as _;
    use pi_sandbox_protocol::FileWrite;

    let Some(path_str) = path_input else {
        return Ok(vec![]);
    };

    // Resolve path the same way validate_paths does: relative to work_dir.
    let expanded = shellexpand::tilde(path_str).into_owned();
    let raw = if std::path::Path::new(&expanded).is_absolute() {
        PathBuf::from(expanded)
    } else {
        work_dir.join(expanded)
    };

    // Normalize `..` and `.` components (without filesystem access) so that
    // the relative path serialized into `file_writes.path` is clean and passes
    // the host-side `validate_flushback_path` check.  A path like
    // `src/../foo.rs` must be serialized as `foo.rs`, not `src/../foo.rs`,
    // otherwise the host rejects it with "contains '..' component" and
    // poisons the session for a valid write/edit call.
    let mut parts: Vec<std::path::Component<'_>> = Vec::new();
    for component in raw.components() {
        use std::path::Component;
        match component {
            Component::ParentDir => {
                parts.pop();
            }
            Component::CurDir => {}
            other => parts.push(other),
        }
    }
    let resolved: PathBuf = parts.iter().collect();

    // Read the file — if it doesn't exist or can't be read, return empty (not an error).
    let bytes = match std::fs::read(&resolved) {
        Ok(b) => b,
        Err(_) => return Ok(vec![]),
    };

    // Enforce 32 KiB size cap (per RFD 0026 §"Size cap and fallback").
    // Return Err so the call site can emit is_error=true rather than a
    // fake FileWrite sentinel that the host would misinterpret.
    if bytes.len() as u64 > FILE_WRITE_MAX_BYTES {
        return Err(format!(
            "remote sync error: file too large for inline flushback ({} bytes); \
             use bash to split or compress first",
            bytes.len()
        ));
    }

    // Compute the relative path (strip work_dir prefix).
    let rel = resolved.strip_prefix(work_dir).unwrap_or(&resolved);

    // Read Unix permissions (default 0o644 if unavailable).
    let mode = {
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::metadata(&resolved)
                .map(|m| m.permissions().mode())
                .unwrap_or(0o644)
        }
        #[cfg(not(unix))]
        {
            0o644
        }
    };

    let contents_b64 = base64::engine::general_purpose::STANDARD.encode(&bytes);
    Ok(vec![FileWrite {
        path: rel.to_string_lossy().to_string(),
        contents_b64,
        mode,
    }])
}
