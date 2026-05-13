//! Firecracker integration smoke test (gated on `PI_SANDBOX_FC_TEST=1`).
//!
//! # Prerequisites for running this test
//!
//! Set ALL of the following env vars before running:
//!
//! ```sh
//! export PI_SANDBOX_FC_TEST=1           # gates the test
//! export PI_SANDBOX_KERNEL=/path/to/vmlinux  # Firecracker-compatible vmlinux
//! export PI_SANDBOX_ROOTFS=/path/to/rootfs.img  # built by crates/pi-sandbox-rootfs/build.sh
//! ```
//!
//! When any prerequisite is absent the test prints a skip message and exits 0.
//! This means `cargo test -p pi-sandbox` without the gate set always passes.
//!
//! # What this tests
//!
//! 1. `probe()` returns `available=true`.
//! 2. `acquire()` succeeds (cold boot path).
//! 3. `execute()` of the "bash" tool runs `cat /etc/pi-sandbox-version`
//!    inside the guest and returns the rootfs version string.
//!    This validates the full end-to-end path (boot → vsock → worker →
//!    tool execution → response).
//! 4. `release()` returns the VM to the pool.
//!
//! # No virtio-fs in v1
//!
//! Firecracker (≤ v1.15.0) silently drops the `fs` device config block
//! (upstream issue #1180). The smoke test does NOT exercise `/work`
//! sharing — `host_cwd` from the VmSpec is informational only in v1.
//! Cross-namespace file sharing returns under contextfs (Commit G3).

#![cfg(target_os = "linux")]

use std::path::PathBuf;

use pi_sandbox::microvm::{CallLimits, NetworkPolicy, RootfsVersion, VmCeiling, VmSpec};
use pi_sandbox::microvm::firecracker::{FirecrackerConfig, FirecrackerLauncher};
use pi_sandbox::microvm::launcher::MicroVmLauncher;
use pi_tools::ToolContext;
use serde_json::json;

fn skip(reason: &str) {
    eprintln!("SKIP: {reason}");
}

/// Return the value of an env var, or skip with a message.
macro_rules! require_env {
    ($var:expr) => {
        match std::env::var($var) {
            Ok(v) if !v.is_empty() => v,
            _ => {
                skip(&format!("env var {} not set — skipping firecracker smoke test", $var));
                return;
            }
        }
    };
}

#[tokio::test]
async fn firecracker_smoke_boot_read_release() {
    // Gate: must opt-in.
    require_env!("PI_SANDBOX_FC_TEST");

    // Check individual prerequisites with clear skip messages.
    if which::which("firecracker").is_err() {
        skip("firecracker binary not on $PATH");
        return;
    }

    let kernel_path = match std::env::var("PI_SANDBOX_KERNEL") {
        Ok(p) if !p.is_empty() => PathBuf::from(p),
        _ => {
            skip("PI_SANDBOX_KERNEL not set — kernel artifact is a maintainer-side prereq");
            return;
        }
    };
    if !kernel_path.exists() {
        skip(&format!(
            "PI_SANDBOX_KERNEL={} does not exist",
            kernel_path.display()
        ));
        return;
    }

    let rootfs_path = match std::env::var("PI_SANDBOX_ROOTFS") {
        Ok(p) if !p.is_empty() => PathBuf::from(p),
        _ => {
            skip(
                "PI_SANDBOX_ROOTFS not set — run crates/pi-sandbox-rootfs/build.sh first",
            );
            return;
        }
    };
    if !rootfs_path.exists() {
        skip(&format!(
            "PI_SANDBOX_ROOTFS={} does not exist",
            rootfs_path.display()
        ));
        return;
    }

    if std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open("/dev/kvm")
        .is_err()
    {
        skip("/dev/kvm not openable RW — add user to 'kvm' group");
        return;
    }

    // Build launcher.
    let tmp = tempfile::tempdir().expect("tempdir");
    let config = FirecrackerConfig {
        kernel_path: Some(kernel_path.clone()),
        rootfs_path: Some(rootfs_path.clone()),
        run_dir: tmp.path().join("run"),
        pool_size: 1,
        ..Default::default()
    };
    let launcher = FirecrackerLauncher::new(config);

    // Probe.
    let report = launcher
        .probe()
        .await
        .expect("probe() should not return Err");
    assert!(
        report.available,
        "probe returned available=false: {:?}",
        report
    );
    eprintln!("probe OK: {:?}", report.version);

    // host_cwd is informational in v1 (Firecracker silently drops
    // the virtio-fs `fs` device — see RFD 0023 §"Filesystem
    // semantics"). The smoke test reads a rootfs-embedded file
    // (`/etc/pi-sandbox-version`) instead of anything host-shared.
    let work_dir = tempfile::tempdir().expect("work_dir tempdir");
    let spec = VmSpec {
        host_cwd: work_dir.path().to_path_buf(),
        host_cwd_writable: true,
        env: Default::default(),
        network_policy: NetworkPolicy::Deny,
        vm_ceiling: VmCeiling::default(),
        rootfs_version: RootfsVersion::current(),
    };

    // Acquire (cold boot).
    let handle = launcher
        .acquire(&spec)
        .await
        .expect("acquire() should succeed");

    // Execute: run `cat /etc/pi-sandbox-version` via bash.
    // This validates the full vsock → worker → tool-execution path
    // without requiring virtio-fs. /etc/pi-sandbox-version is embedded
    // in the rootfs by build.sh and always contains the version string.
    // The bash tool is not subject to the /work path boundary check, so
    // it can read any file in the guest filesystem.
    let ctx = ToolContext::default();
    let limits = CallLimits::default();
    let exec = handle
        .execute(
            &ctx,
            &limits,
            "bash",
            &json!({ "command": "cat /etc/pi-sandbox-version" }),
        )
        .await
        .expect("execute() should succeed");

    eprintln!(
        "exec: is_error={} guest_duration_ms={} cold_boot={}",
        exec.result.is_error, exec.guest_duration_ms, exec.cold_boot
    );

    assert!(
        !exec.result.is_error,
        "tool returned is_error=true: {}",
        exec.result.model_output
    );
    // The rootfs version is embedded as the first line of /etc/pi-sandbox-version.
    let expected_version = pi_sandbox::microvm::RootfsVersion::current().0;
    assert!(
        exec.result.model_output.trim().contains(expected_version.trim()),
        "expected rootfs version {:?} in bash output, got: {:?}",
        expected_version,
        exec.result.model_output
    );

    // Release (returns to pool).
    handle.release().await.expect("release() should succeed");

    eprintln!("smoke test PASSED");
}
