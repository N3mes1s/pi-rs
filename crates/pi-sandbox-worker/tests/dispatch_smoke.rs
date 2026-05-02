//! Smoke tests for dispatch_request — no vsock needed; pure in-process.

#[cfg(target_os = "linux")]
mod linux_tests {
    use pi_sandbox_protocol::{ToolRequest, CURRENT_PROTOCOL_VERSION};
    use pi_sandbox_worker::dispatch_request;
    use std::io::Write;
    use tempfile::NamedTempFile;

    fn make_req(tool_name: &str, input: serde_json::Value) -> ToolRequest {
        ToolRequest {
            proto_version: CURRENT_PROTOCOL_VERSION,
            call_id: "test-call-1".to_string(),
            tool_name: tool_name.to_string(),
            tool_input: input,
            max_output_bytes: 256 * 1024,
            timeout_ms: 5_000,
        }
    }

    #[tokio::test]
    async fn read_tool_returns_file_contents() {
        let mut file = NamedTempFile::new().unwrap();
        writeln!(file, "hello sandbox").unwrap();
        let path = file.path().to_str().unwrap().to_string();

        let req = make_req("read", serde_json::json!({ "path": path }));
        let work_dir = std::path::Path::new("/tmp");
        let resp = dispatch_request(req, work_dir).await;

        assert!(
            !resp.is_error,
            "expected no error, got stderr: {}",
            resp.stderr
        );
        assert!(
            resp.stdout.contains("hello sandbox"),
            "stdout should contain file contents, got: {}",
            resp.stdout
        );
    }

    /// BusyBox `timeout 1 sh -c 'sleep 5'` fires after 1 second and returns
    /// exit 124. The worker timeout budget is 100 ms (sub-second), but the
    /// outer guard is set to ceil(100/1000)*1000 + 1000 = 2000 ms so the OS
    /// kill completes before the worker can drop the future. Test takes ~1 s.
    #[tokio::test]
    async fn bash_timeout_returns_exit_124() {
        let req = ToolRequest {
            proto_version: CURRENT_PROTOCOL_VERSION,
            call_id: "test-call-timeout".to_string(),
            tool_name: "bash".to_string(),
            tool_input: serde_json::json!({ "command": "sleep 5" }),
            max_output_bytes: 4096,
            timeout_ms: 100, // sub-second: BusyBox rounds up to 1 s
        };
        let work_dir = std::path::Path::new("/tmp");
        let resp = dispatch_request(req, work_dir).await;

        assert!(resp.is_error, "expected is_error=true for timed-out bash");
        // BusyBox `timeout` exits 124; BashTool surfaces the child exit code.
        assert_eq!(resp.exit_status, 124, "expected exit_status 124 for timeout");
    }

    #[tokio::test]
    async fn unknown_tool_error_goes_to_stdout() {
        let req = make_req("nonexistent", serde_json::Value::Null);
        let work_dir = std::path::Path::new("/tmp");
        let resp = dispatch_request(req, work_dir).await;

        assert!(resp.is_error, "expected is_error=true for unknown tool");
        // stdout is the model-facing field — error text must appear there.
        assert!(
            resp.stdout.contains("unknown tool"),
            "stdout (model-facing) should contain 'unknown tool', got: {}",
            resp.stdout
        );
        assert!(
            resp.stderr.contains("unknown tool"),
            "stderr (diagnostic) should also contain 'unknown tool', got: {}",
            resp.stderr
        );
    }

    /// Regression: a request that includes an explicit `timeout_ms` in
    /// `tool_input` must still be clamped so BashTool's internal timer cannot
    /// fire before the BusyBox `timeout` wrapper kills the child process.
    /// Previously, `harden_bash_input` used `or_insert`, which silently kept
    /// the caller-supplied value (e.g. 100 ms) intact, causing the inner tokio
    /// timer to win the race and leave the child alive.
    #[tokio::test]
    async fn bash_explicit_inner_timeout_is_clamped() {
        // timeout_ms = 100 ms on the request.  Without the fix, tool_input would
        // carry `timeout_ms: 100`, BashTool fires after 100 ms, and the child
        // outlives the response.  With the fix, tool_input carries
        // `timeout_ms: 1500` (ceil(100/1000)*1000 + 500), BusyBox kills the
        // child at t=1 s, and BashTool sees exit 124.
        let req = ToolRequest {
            proto_version: CURRENT_PROTOCOL_VERSION,
            call_id: "test-explicit-inner".to_string(),
            tool_name: "bash".to_string(),
            tool_input: serde_json::json!({
                "command": "sleep 5",
                "timeout_ms": 100  // explicit inner timer — must be overwritten
            }),
            max_output_bytes: 4096,
            timeout_ms: 100,
        };
        let work_dir = std::path::Path::new("/tmp");
        let resp = dispatch_request(req, work_dir).await;

        assert!(resp.is_error, "expected is_error=true for timed-out bash");
        assert_eq!(
            resp.exit_status, 124,
            "expected exit_status 124 — BusyBox should have killed the child, got {}",
            resp.exit_status
        );
    }

    #[tokio::test]
    async fn path_escape_is_rejected() {
        // Attempt to read a file outside work_dir via `../` traversal.
        let req = make_req("read", serde_json::json!({ "path": "/tmp/../etc/passwd" }));
        let work_dir = std::path::Path::new("/tmp/sandbox");
        let resp = dispatch_request(req, work_dir).await;

        assert!(resp.is_error, "expected is_error=true for path escape");
        assert!(
            resp.stdout.contains("sandbox error"),
            "stdout should describe the sandbox boundary error, got: {}",
            resp.stdout
        );
    }

    #[tokio::test]
    async fn tilde_path_escape_is_rejected() {
        // A tilde path like "~/x" expands to /home/<user>/x, which is outside
        // /tmp/sandbox. The worker must catch this after tilde-expansion,
        // mirroring pi_tools_core::resolve_path's behaviour.
        let req = make_req("read", serde_json::json!({ "path": "~/secret" }));
        let work_dir = std::path::Path::new("/tmp/sandbox");
        let resp = dispatch_request(req, work_dir).await;

        assert!(resp.is_error, "expected is_error=true for tilde-escaped path");
        assert!(
            resp.stdout.contains("sandbox error"),
            "stdout should describe the sandbox boundary error, got: {}",
            resp.stdout
        );
    }
}

// On non-Linux, provide a no-op test so the file compiles cleanly.
#[cfg(not(target_os = "linux"))]
#[test]
fn non_linux_stub() {
    // Dispatch tests only run on Linux (vsock guest binary is Linux-only).
}
