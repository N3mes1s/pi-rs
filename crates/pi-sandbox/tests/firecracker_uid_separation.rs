//! UID-separation regression test — RFD 0023 §6 Layer 1.
//!
//! Inside the guest, the worker runs as root (PID 1's child) but
//! every `bash` tool invocation drops to UID 1001 (pi-tool) before
//! exec via setuid/setgid in `pre_exec`. This test asserts:
//!
//! 1. `bash id` reports uid=1001(pi-tool) — the drop happened
//! 2. `bash whoami` reports `pi-tool` — passwd entry resolves
//! 3. `bash echo > /etc/foo` fails with EACCES — bash can't
//!    write to root-owned files
//! 4. `bash echo > /tmp/foo` succeeds — /tmp is world-writable
//! 5. `bash echo > /opt/foo` succeeds — /opt is chowned to pi-tool
//!    by the init script for persistent scratch across calls
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
async fn bash_runs_as_pi_tool_not_root() {
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

    // 1+2: identity check.
    let r = h
        .execute(
            &ToolContext::default(),
            &CallLimits {
                wall_timeout: Duration::from_secs(15),
                ..Default::default()
            },
            "bash",
            &json!({"command": "id; whoami"}),
        )
        .await
        .expect("id");
    eprintln!("id output:\n{}", r.result.model_output);
    assert!(
        r.result.model_output.contains("uid=1001"),
        "bash should run as uid=1001 (pi-tool); got: {}",
        r.result.model_output
    );
    assert!(
        r.result.model_output.contains("pi-tool"),
        "bash whoami should resolve to pi-tool; got: {}",
        r.result.model_output
    );
    assert!(
        !r.result.model_output.contains("uid=0"),
        "bash should NOT run as root; got: {}",
        r.result.model_output
    );

    // 3: /etc write must fail with EACCES (or "permission denied" /
    // "read-only" — the overlay layer might surface either).
    let r = h
        .execute(
            &ToolContext::default(),
            &CallLimits {
                wall_timeout: Duration::from_secs(10),
                ..Default::default()
            },
            "bash",
            &json!({"command": "echo bypass > /etc/should-not-exist 2>&1; ls /etc/should-not-exist 2>&1; echo \"rc=$?\""}),
        )
        .await
        .expect("/etc write");
    eprintln!("/etc write:\n{}", r.result.model_output);
    let body = r.result.model_output.to_lowercase();
    assert!(
        body.contains("permission denied")
            || body.contains("read-only")
            || body.contains("not permitted"),
        "bash writing /etc/* should fail with permission-denied; got: {}",
        r.result.model_output
    );
    assert!(
        !body.contains("/etc/should-not-exist\n") || body.contains("no such file"),
        "the file should NOT exist after the failed write; got: {}",
        r.result.model_output
    );

    // 4: /tmp write must succeed (world-writable).
    let r = h
        .execute(
            &ToolContext::default(),
            &CallLimits::default(),
            "bash",
            &json!({"command": "echo hello > /tmp/uid-test; cat /tmp/uid-test"}),
        )
        .await
        .expect("/tmp write");
    assert!(
        r.result.model_output.contains("hello"),
        "bash should be able to write /tmp; got: {}",
        r.result.model_output
    );

    // 5: /opt write must succeed (chowned to pi-tool by init).
    let r = h
        .execute(
            &ToolContext::default(),
            &CallLimits::default(),
            "bash",
            &json!({"command": "echo persist > /opt/uid-test; cat /opt/uid-test; ls -ln /opt/uid-test"}),
        )
        .await
        .expect("/opt write");
    eprintln!("/opt write:\n{}", r.result.model_output);
    assert!(
        r.result.model_output.contains("persist"),
        "bash should be able to write /opt (chowned to pi-tool); got: {}",
        r.result.model_output
    );

    h.release().await.expect("release");
}
