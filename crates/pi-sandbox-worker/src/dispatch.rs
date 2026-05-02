//! Tool dispatch: take a ToolRequest, run the named tool from
//! pi-tools-core's registry, build a ToolResponse.

use pi_sandbox_protocol::{ToolRequest, ToolResponse};
use pi_tools_core::{ToolContext, ToolRegistry};
use std::path::Path;
use std::sync::OnceLock;

static REGISTRY: OnceLock<ToolRegistry> = OnceLock::new();

fn registry() -> &'static ToolRegistry {
    REGISTRY.get_or_init(ToolRegistry::with_extras)
}

pub async fn dispatch_request(req: ToolRequest, work_dir: &Path) -> ToolResponse {
    let call_id = req.call_id.clone();

    // Look up the tool by name.
    let Some(tool) = registry().get(&req.tool_name) else {
        return ToolResponse {
            call_id,
            stdout: String::new(),
            stderr: format!("unknown tool: {}", req.tool_name),
            exit_status: 1,
            guest_duration_ms: 0, // listener overrides
            is_error: true,
        };
    };

    // Build a ToolContext scoped to /work.
    let ctx = ToolContext {
        cwd: work_dir.to_path_buf(),
        max_output_bytes: req.max_output_bytes as usize,
    };

    // Wrap the tool invocation in the per-call wall timeout.
    let timeout = std::time::Duration::from_millis(req.timeout_ms as u64);
    let invoke_fut = tool.invoke(&ctx, &req.call_id, req.tool_input);
    match tokio::time::timeout(timeout, invoke_fut).await {
        Ok(Ok(result)) => ToolResponse {
            call_id,
            stdout: result.model_output,
            stderr: String::new(),
            exit_status: if result.is_error { 1 } else { 0 },
            guest_duration_ms: 0, // listener overrides
            is_error: result.is_error,
        },
        Ok(Err(e)) => ToolResponse {
            call_id,
            stdout: String::new(),
            stderr: format!("tool error: {e}"),
            exit_status: 1,
            guest_duration_ms: 0,
            is_error: true,
        },
        Err(_) => ToolResponse {
            call_id,
            stdout: String::new(),
            stderr: format!("tool timed out after {} ms", req.timeout_ms),
            exit_status: 124, // standard timeout exit code
            guest_duration_ms: 0,
            is_error: true,
        },
    }
}
