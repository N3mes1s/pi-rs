//! Host-side `web_search` proxy listener — RFD 0023 §"web_search via vsock proxy".
//!
//! Per-VM, the launcher binds a UNIX socket at `<vsock_path>_5003`
//! (firecracker's convention for guest→host vsock port mapping) and
//! spawns this listener. When the guest worker proxies a `web_search`
//! ToolRequest out via vsock(2, 5003), firecracker forwards the
//! connection to our UDS; we read the framed `WebSearchRequest`,
//! invoke the host-side `WebSearchTool` (with the host's
//! `AuthStorage`), and ship back a `WebSearchResponse`.
//!
//! v1 simplifications (deferred to v1.1+):
//! - listener task lives until pi process exit (no JoinHandle tracking)
//! - one provider override (`WebSearchProvider::default()`) — no per-VM
//!   policy gate yet
//! - no signed responses; the listener trusts firecracker's vsock
//!   isolation (guests can't reach each other's `<vsock_path>_5003`)

use std::path::Path;

use pi_search_proto::{
    self as search_proto, WebSearchRequest, WebSearchResponse, CURRENT_PROTO_VERSION,
    DEFAULT_MAX_LINE_BYTES,
};
use pi_tools::{Tool as _, ToolContext, WebSearchTool};
use tokio::io::BufReader;
use tokio::net::UnixListener;
use tracing::{debug, warn};

/// Bind the per-VM `web_search` proxy UDS at `<vsock_path>_5003` and
/// spawn the accept loop. The listener self-terminates on accept
/// errors (firecracker tearing the device down), which makes the
/// task safe to leave un-tracked in v1.
///
/// Returns the bound UDS path on success so the caller can clean up
/// on VM teardown.
pub(crate) fn spawn_search_proxy_listener(
    vsock_path: &Path,
) -> Result<std::path::PathBuf, std::io::Error> {
    // firecracker convention: <vsock_path>_<port>
    let mut p = vsock_path.as_os_str().to_owned();
    p.push("_5003");
    let proxy_path = std::path::PathBuf::from(p);

    // Remove any leftover from a previous run before binding.
    let _ = std::fs::remove_file(&proxy_path);
    let listener = UnixListener::bind(&proxy_path)?;
    debug!(path = %proxy_path.display(), "search proxy listener bound");

    let path_for_log = proxy_path.clone();
    tokio::spawn(async move {
        loop {
            match listener.accept().await {
                Ok((stream, _addr)) => {
                    tokio::spawn(handle_one(stream));
                }
                Err(e) => {
                    debug!(
                        path = %path_for_log.display(),
                        err = %e,
                        "search proxy listener exiting"
                    );
                    break;
                }
            }
        }
    });

    Ok(proxy_path)
}

async fn handle_one(stream: tokio::net::UnixStream) {
    let (read_half, mut write_half) = stream.into_split();
    let mut reader = BufReader::new(read_half);

    let req = match search_proto::read_request(&mut reader, DEFAULT_MAX_LINE_BYTES).await {
        Ok(r) => r,
        Err(e) => {
            warn!(err = %e, "search proxy: failed to read request frame");
            return;
        }
    };

    let resp = match dispatch_to_host_tool(&req).await {
        Ok(r) => r,
        Err(e) => WebSearchResponse {
            proto_version: CURRENT_PROTO_VERSION,
            call_id: req.call_id.clone(),
            result_text: String::new(),
            error: Some(e),
        },
    };

    if let Err(e) = search_proto::write_response(&mut write_half, &resp).await {
        warn!(err = %e, "search proxy: failed to write response frame");
    }
}

async fn dispatch_to_host_tool(req: &WebSearchRequest) -> Result<WebSearchResponse, String> {
    if req.proto_version != CURRENT_PROTO_VERSION {
        return Err(format!(
            "proto-version-mismatch: host={CURRENT_PROTO_VERSION}, guest={}",
            req.proto_version
        ));
    }

    // Build the `WebSearchTool` input value. The host tool's schema is
    // `{ "query": "...", "provider": "...", "max_results": N }` — we
    // pass through the optional fields as-is and let the host's
    // `WebSearchTool::default()` resolve provider via env.
    let mut input = serde_json::Map::new();
    input.insert("query".into(), serde_json::Value::String(req.query.clone()));
    if let Some(p) = &req.provider {
        input.insert("provider".into(), serde_json::Value::String(p.clone()));
    }
    if let Some(n) = req.max_results {
        input.insert(
            "max_results".into(),
            serde_json::Value::Number((n as u64).into()),
        );
    }
    let input = serde_json::Value::Object(input);

    let tool = WebSearchTool::default();
    let ctx = ToolContext {
        cwd: std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from(".")),
        max_output_bytes: 256 * 1024,
    };

    match tool.invoke(&ctx, &req.call_id, input).await {
        Ok(result) => Ok(WebSearchResponse {
            proto_version: CURRENT_PROTO_VERSION,
            call_id: req.call_id.clone(),
            result_text: result.model_output,
            error: if result.is_error {
                Some("upstream tool returned is_error=true".into())
            } else {
                None
            },
        }),
        Err(e) => Err(format!("WebSearchTool: {e}")),
    }
}
