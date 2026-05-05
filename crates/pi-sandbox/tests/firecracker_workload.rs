//! Multi-command workload demo: prove tool calls hit the same warm VM
//! and side effects persist across calls.
#![cfg(target_os = "linux")]
use pi_sandbox::microvm::firecracker::{FirecrackerConfig, FirecrackerLauncher};
use pi_sandbox::microvm::{
    CallLimits, MicroVmLauncher, NetworkPolicy, RootfsVersion, VmCeiling, VmSpec,
};
use pi_tools::ToolContext;
use serde_json::json;
use std::time::Instant;

#[tokio::test]
async fn rust_workload_demo() {
    if std::env::var("PI_SANDBOX_FC_TEST").is_err() {
        eprintln!("PI_SANDBOX_FC_TEST not set; skipping");
        return;
    }
    let tmp = tempfile::tempdir().unwrap();
    let work = tempfile::tempdir().unwrap();
    let cfg = FirecrackerConfig {
        kernel_path: Some(std::env::var("PI_SANDBOX_KERNEL").unwrap().into()),
        rootfs_path: Some(std::env::var("PI_SANDBOX_ROOTFS").unwrap().into()),
        run_dir: tmp.path().join("run"),
        pool_size: 1,
        ..Default::default()
    };
    let launcher = FirecrackerLauncher::new(cfg);
    let spec = VmSpec {
        host_cwd: work.path().to_path_buf(),
        host_cwd_writable: true,
        env: Default::default(),
        network_policy: NetworkPolicy::Deny,
        vm_ceiling: VmCeiling::default(),
        rootfs_version: RootfsVersion::current(),
    };

    // Warm up the pool by acquiring + releasing once. After this, follow-up
    // acquires hit the warm path. NOTE: each acquire returns a different VM
    // from the pool — state from VM A doesn't leak to VM B (this is the
    // whole point of the sandbox). To demonstrate state persistence within
    // ONE VM lifetime, we hold a single handle for the whole sequence.
    let h = launcher.acquire(&spec).await.unwrap();

    let cmds = [
        // Overlay sanity: writes outside /tmp must also succeed under overlay.
        // (Under per-path tmpfs this would have failed with Read-only file system.)
        ("echo overlay-test > /etc/pi-overlay-marker && cat /etc/pi-overlay-marker && ls -la /usr/local/bin/", "overlay sanity: write to /etc + read it back"),
        ("uname -a", "kernel inside the guest"),
        ("cat /etc/os-release | head -5", "rootfs identity"),
        ("ls -1 /usr/local/bin/ | head -20", "what binaries the rootfs ships"),
        ("which rustc cargo apk; true", "are rust toolchain or alpine pkg mgr present?"),
        (
            "mkdir -p /tmp/demo && cat > /tmp/demo/main.rs <<'EOF'\nfn main() { println!(\"hello from inside the microvm\"); }\nEOF\nls -la /tmp/demo/",
            "write a Rust file and list it",
        ),
        ("cat /tmp/demo/main.rs", "read it back — proves the file persists across bash calls in the same VM"),
        ("wc -l /tmp/demo/main.rs", "count lines — different command, same VM, same file"),
        (
            "apk add --no-cache cargo rust 2>&1 | head -5; echo \"exit=$?\"",
            "try to install cargo (no network — should fail loudly)",
        ),
        ("ip a 2>&1 | head -5; echo \"---\"; cat /etc/resolv.conf 2>&1", "verify network is locked down"),
    ];

    for (i, (cmd, label)) in cmds.iter().enumerate() {
        let t = Instant::now();
        let exec = h
            .execute(
                &ToolContext::default(),
                &CallLimits::default(),
                "bash",
                &json!({"command": cmd}),
            )
            .await
            .unwrap();
        let dt = t.elapsed();
        eprintln!("\n────────────────────────────────────────────────────────────────");
        eprintln!(
            "call {} ({:?}, guest_ms={}, is_error={})  // {}",
            i, dt, exec.guest_duration_ms, exec.result.is_error, label
        );
        eprintln!("$ {}", cmd);
        eprintln!("{}", exec.result.model_output.trim_end());
    }
    h.release().await.unwrap();
}
