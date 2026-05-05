//! Per-call hygiene regression test — RFD 0023 §"Post-call hygiene".
//!
//! After every tool call, the worker wipes writable scratch paths
//! (`/tmp`, `/var/tmp`, `/root`) so call N+1 never sees files left
//! by call N. This test issues two `bash` calls on the SAME warm
//! VM:
//!
//!   1. `echo secret-N1 > /tmp/leak; ls /tmp` — confirms the file
//!      is visible IN the same call (the wipe runs BEFORE the next
//!      dispatch, not after).
//!   2. `ls /tmp; cat /tmp/leak 2>&1` — confirms /tmp/leak is GONE
//!      and the cat returns "No such file or directory".
//!
//! Gated on `PI_SANDBOX_FC_TEST=1`.

#![cfg(target_os = "linux")]

use std::path::PathBuf;
use std::time::Duration;

use pi_sandbox::microvm::firecracker::{FirecrackerConfig, FirecrackerLauncher};
use pi_sandbox::microvm::launcher::MicroVmLauncher;
use pi_sandbox::microvm::{CallLimits, NetworkPolicy, RootfsVersion, VmCeiling, VmSpec};
use pi_tools::ToolContext;
use serde_json::json;

fn skip(reason: &str) {
    eprintln!("SKIP: {reason}");
}

#[tokio::test]
async fn tmp_is_wiped_between_tool_calls_in_same_vm() {
    if std::env::var("PI_SANDBOX_FC_TEST").is_err() {
        return skip("PI_SANDBOX_FC_TEST not set");
    }
    if which::which("firecracker").is_err() {
        return skip("firecracker not on PATH");
    }
    let kernel_path = match std::env::var("PI_SANDBOX_KERNEL") {
        Ok(p) if !p.is_empty() => PathBuf::from(p),
        _ => return skip("PI_SANDBOX_KERNEL not set"),
    };
    let rootfs_path = match std::env::var("PI_SANDBOX_ROOTFS") {
        Ok(p) if !p.is_empty() => PathBuf::from(p),
        _ => return skip("PI_SANDBOX_ROOTFS not set"),
    };

    let tmp = tempfile::tempdir().unwrap();
    let work = tempfile::tempdir().unwrap();
    let cfg = FirecrackerConfig {
        kernel_path: Some(kernel_path),
        rootfs_path: Some(rootfs_path),
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
    let h = launcher.acquire(&spec).await.expect("acquire");

    // Call 1: write a sentinel into /tmp and prove it's there.
    let r = h
        .execute(
            &ToolContext::default(),
            &CallLimits {
                wall_timeout: Duration::from_secs(15),
                ..Default::default()
            },
            "bash",
            &json!({"command": "echo secret-from-call-1 > /tmp/leak; ls /tmp; cat /tmp/leak"}),
        )
        .await
        .expect("call 1");
    eprintln!("call 1 output:\n{}", r.result.model_output);
    assert!(
        r.result.model_output.contains("secret-from-call-1"),
        "call 1 should still see its own write within the call: {}",
        r.result.model_output
    );

    // Call 2: same VM. /tmp should be empty, cat /tmp/leak should fail.
    let r = h
        .execute(
            &ToolContext::default(),
            &CallLimits {
                wall_timeout: Duration::from_secs(15),
                ..Default::default()
            },
            "bash",
            &json!({"command": "ls /tmp 2>&1; echo '---'; cat /tmp/leak 2>&1; echo \"exit=$?\""}),
        )
        .await
        .expect("call 2");
    eprintln!("call 2 output:\n{}", r.result.model_output);
    assert!(
        !r.result.model_output.contains("secret-from-call-1"),
        "call 2 should NOT see call 1's /tmp/leak content: {}",
        r.result.model_output
    );
    let lower = r.result.model_output.to_lowercase();
    assert!(
        lower.contains("no such file") || lower.contains("not found"),
        "call 2's `cat /tmp/leak` should fail with no-such-file: {}",
        r.result.model_output
    );

    // Bonus: write to /root in call 1, check it's gone in call 3.
    let _ = h
        .execute(
            &ToolContext::default(),
            &CallLimits::default(),
            "bash",
            &json!({"command": "echo root-secret > /root/leak"}),
        )
        .await
        .expect("call 3 (write)");
    let r = h
        .execute(
            &ToolContext::default(),
            &CallLimits::default(),
            "bash",
            &json!({"command": "ls /root 2>&1; cat /root/leak 2>&1; echo \"exit=$?\""}),
        )
        .await
        .expect("call 4 (probe)");
    assert!(
        !r.result.model_output.contains("root-secret"),
        "call 4 should NOT see /root/leak content: {}",
        r.result.model_output
    );

    h.release().await.expect("release");
}
