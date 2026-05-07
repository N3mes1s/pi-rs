//! Firecracker + contextfs `/work` mount integration test
//! (RFD 0023 §3.5 / Commit G3).
//!
//! Exercises the full host_cwd → /work path:
//!
//! 1. Host writes a sentinel file into a tempdir (host_cwd).
//! 2. Pi-rs spawns `cfs-fs-server` rooted at host_cwd, listening on
//!    `<run_dir>/cfs-fs.sock` (host-side UDS).
//! 3. Pi-rs binds `<vsock_path>_5005` and bridges to the
//!    cfs-fs-server UDS.
//! 4. Firecracker boots the rootfs. Init starts
//!    `pi-cfs-vsock-bridge` (binds `/run/cfs.sock` in guest,
//!    forwards to `vsock(2,5005)`), then starts `contextfsd`
//!    which dials `/run/cfs.sock`, probes caps, and FUSE-mounts
//!    `/work`.
//! 5. The test runs `cat /work/<sentinel>` in the guest and asserts
//!    the bytes match what the host wrote.
//!
//! All four wire hops are covered: host UDS ↔ host bridge ↔ vsock
//! ↔ guest bridge ↔ contextfsd ↔ FUSE.
//!
//! # Skip conditions
//!
//! Skipped (not failed) when any prerequisite is missing:
//! - `PI_SANDBOX_FC_TEST=1` not set
//! - `firecracker` not on PATH
//! - `PI_SANDBOX_KERNEL` / `PI_SANDBOX_ROOTFS` unset or missing
//! - `/dev/kvm` not openable RW
//! - `cfs-fs-server` not resolvable (no `PI_SANDBOX_CFS_FS_SERVER_BIN`
//!   override and no `cfs-fs-server` on PATH)

#![cfg(target_os = "linux")]

use std::path::PathBuf;

use pi_sandbox::microvm::firecracker::{FirecrackerConfig, FirecrackerLauncher};
use pi_sandbox::microvm::launcher::MicroVmLauncher;
use pi_sandbox::microvm::{CallLimits, NetworkPolicy, RootfsVersion, VmCeiling, VmSpec};
use pi_tools::ToolContext;
use serde_json::json;

fn skip(reason: &str) {
    eprintln!("SKIP: {reason}");
}

macro_rules! require_env {
    ($var:expr) => {
        match std::env::var($var) {
            Ok(v) if !v.is_empty() => v,
            _ => {
                skip(&format!("env var {} not set", $var));
                return;
            }
        }
    };
}

#[tokio::test]
async fn firecracker_contextfs_work_mount_read() {
    require_env!("PI_SANDBOX_FC_TEST");

    if which::which("firecracker").is_err() {
        skip("firecracker not on PATH");
        return;
    }

    // cfs-fs-server is a hard prereq for /work; skip cleanly if absent
    // so non-contextfs CI environments stay green.
    let cfs_explicit = std::env::var("PI_SANDBOX_CFS_FS_SERVER_BIN")
        .ok()
        .filter(|s| !s.is_empty())
        .map(PathBuf::from)
        .filter(|p| p.exists());
    if cfs_explicit.is_none() && which::which("cfs-fs-server").is_err() {
        skip(
            "cfs-fs-server not resolvable (no PI_SANDBOX_CFS_FS_SERVER_BIN, none on PATH)",
        );
        return;
    }

    let kernel_path = match std::env::var("PI_SANDBOX_KERNEL") {
        Ok(p) if !p.is_empty() => PathBuf::from(p),
        _ => {
            skip("PI_SANDBOX_KERNEL not set");
            return;
        }
    };
    if !kernel_path.exists() {
        skip(&format!("PI_SANDBOX_KERNEL={} missing", kernel_path.display()));
        return;
    }

    let rootfs_path = match std::env::var("PI_SANDBOX_ROOTFS") {
        Ok(p) if !p.is_empty() => PathBuf::from(p),
        _ => {
            skip("PI_SANDBOX_ROOTFS not set");
            return;
        }
    };
    if !rootfs_path.exists() {
        skip(&format!("PI_SANDBOX_ROOTFS={} missing", rootfs_path.display()));
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

    // Stage the host_cwd. This is what cfs-fs-server will serve as
    // its root, and what contextfsd in the guest will mount at /work.
    let host_cwd = tempfile::tempdir().expect("host_cwd tempdir");
    let sentinel_name = "pi-cfs-mount-sentinel.txt";
    let sentinel_payload = "hello-from-host: 0xdeadbeef\n";
    std::fs::write(host_cwd.path().join(sentinel_name), sentinel_payload)
        .expect("seed sentinel into host_cwd");

    // Per-test launcher.
    let tmp = tempfile::tempdir().expect("run_dir tempdir");
    let config = FirecrackerConfig {
        kernel_path: Some(kernel_path.clone()),
        rootfs_path: Some(rootfs_path.clone()),
        run_dir: tmp.path().join("run"),
        pool_size: 1,
        ..Default::default()
    };
    let launcher = FirecrackerLauncher::new(config);

    let report = launcher.probe().await.expect("probe() Ok");
    assert!(report.available, "probe not available: {report:?}");

    let spec = VmSpec {
        host_cwd: host_cwd.path().to_path_buf(),
        host_cwd_writable: false,
        env: Default::default(),
        network_policy: NetworkPolicy::Deny,
        vm_ceiling: VmCeiling::default(),
        rootfs_version: RootfsVersion::current(),
    };

    let handle = launcher.acquire(&spec).await.expect("acquire");

    let ctx = ToolContext::default();
    let limits = CallLimits::default();

    // First, sanity-check that /work is actually a mount inside the
    // guest. If contextfsd silently failed (FUSE missing, bridge
    // wedged, etc.) the next assertion will give a useful failure
    // mode beyond "file not found".
    let mounts = handle
        .execute(
            &ctx,
            &limits,
            "bash",
            &json!({ "command": "grep ' /work ' /proc/mounts || echo NO_WORK_MOUNT" }),
        )
        .await
        .expect("execute /proc/mounts");
    eprintln!(
        "[/proc/mounts | grep ' /work ']:\n{}",
        mounts.result.model_output
    );
    assert!(
        !mounts.result.model_output.contains("NO_WORK_MOUNT"),
        "/work is not mounted inside the guest. /proc/mounts excerpt:\n{}\n\
         Check /var/log/contextfsd.log + /var/log/cfs-vsock-bridge.log inside the guest.",
        mounts.result.model_output
    );

    // The real assertion: the sentinel file the host wrote into
    // host_cwd is readable inside the guest at /work/<sentinel> from
    // the bash tool subprocess (which drops to pi-tool / UID 1001
    // per RFD 0023 §6 Layer 1). Requires contextfs commit 9f08ae8 or
    // newer — that's where contextfsd started setting
    // `Config.acl = SessionACL::All`, which is what makes
    // `allow_other` actually toggle in FUSE_INIT (the
    // `MountOption::CUSTOM("allow_other")` path is silently
    // classified as a kernel-options string by fuser 0.17.0 and does
    // NOT trigger the kernel toggle).
    let read = handle
        .execute(
            &ctx,
            &limits,
            "bash",
            &json!({ "command": format!("cat /work/{sentinel_name}") }),
        )
        .await
        .expect("execute cat /work/sentinel");

    eprintln!(
        "exec: is_error={} guest_duration_ms={} cold_boot={}",
        read.result.is_error, read.guest_duration_ms, read.cold_boot
    );

    assert!(
        !read.result.is_error,
        "cat /work/{sentinel_name} returned is_error: {}",
        read.result.model_output
    );
    assert!(
        read.result.model_output.contains("hello-from-host: 0xdeadbeef"),
        "expected sentinel payload in /work output, got: {:?}",
        read.result.model_output
    );

    handle.release().await.expect("release");

    eprintln!("contextfs /work mount test PASSED");
}
