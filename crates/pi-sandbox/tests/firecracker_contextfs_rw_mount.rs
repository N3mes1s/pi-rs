//! Firecracker + contextfs `/work` RW mount integration test
//! (RFD 0023 §3.5 / Commit G3 step 3 — Cedar broker phase).
//!
//! Exercises the WRITE half of the host_cwd ↔ /work share:
//!
//! 1. Pi-rs spawns `cfs-fs-server` rooted at host_cwd (no
//!    `--read-only` flag because broker is the policy gate).
//! 2. Pi-rs spawns `contextfs-broker` with a Cedar policy that
//!    permits Agent::"pi-sandbox" → Action::"write" / "create" /
//!    "commit" / "delete" / "rename".
//! 3. Pi-rs binds two host-side vsock bridges:
//!      - `<vsock_path>_5005` → cfs-fs-server UDS (file ops)
//!      - `<vsock_path>_5006` → broker UDS (verify_write)
//! 4. Pi-rs writes the same 32-byte tenant secret on the host
//!    (for the broker) and on the guest (decoded from the
//!    `pi.contextfs.tenant_secret_hex=…` kernel cmdline knob by
//!    the rootfs init).
//! 5. The rootfs init starts both guest-side bridges
//!    (pi-cfs-vsock-bridge → /run/cfs.sock, and
//!    pi-cfs-broker-vsock-bridge → /run/contextfs/broker.sock)
//!    BEFORE contextfsd, so the daemon's first dial of either
//!    UDS finds an accept()ing peer.
//! 6. The init writes contextfsd.toml with `read_only = false`
//!    and a `[broker]` block, then starts contextfsd which
//!    FUSE-mounts /work RW.
//! 7. The test runs `echo … > /work/<sentinel>` from inside the
//!    sandbox and asserts the host can read those bytes back
//!    from host_cwd. End-to-end RW round-trip.
//!
//! Gate: PI_SANDBOX_FC_TEST=1 + PI_SANDBOX_CONTEXTFS_RW=1 +
//! cfs-fs-server + contextfs-broker on PATH (or env overrides).
//! Skipped cleanly when any prerequisite is missing.

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
async fn firecracker_contextfs_work_mount_write_then_read_back() {
    require_env!("PI_SANDBOX_FC_TEST");
    if std::env::var("PI_SANDBOX_CONTEXTFS_RW").ok().as_deref() != Some("1") {
        skip("PI_SANDBOX_CONTEXTFS_RW=1 not set — RW path opt-in");
        return;
    }

    if which::which("firecracker").is_err() {
        skip("firecracker not on PATH");
        return;
    }

    // cfs-fs-server + contextfs-broker are hard prereqs for RW.
    let cfs_explicit = std::env::var("PI_SANDBOX_CFS_FS_SERVER_BIN")
        .ok()
        .filter(|s| !s.is_empty())
        .map(PathBuf::from)
        .filter(|p| p.exists());
    if cfs_explicit.is_none() && which::which("cfs-fs-server").is_err() {
        skip("cfs-fs-server not resolvable");
        return;
    }
    let broker_explicit = std::env::var("PI_SANDBOX_CONTEXTFS_BROKER_BIN")
        .ok()
        .filter(|s| !s.is_empty())
        .map(PathBuf::from)
        .filter(|p| p.exists());
    if broker_explicit.is_none() && which::which("contextfs-broker").is_err() {
        skip(
            "contextfs-broker not resolvable (no PI_SANDBOX_CONTEXTFS_BROKER_BIN, none on PATH)",
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
        skip("/dev/kvm not openable RW");
        return;
    }

    // Stage host_cwd with mode 0777 so the bash tool subprocess
    // (UID 1001 / pi-tool) can write through the FUSE mount.
    // contextfs preserves host UIDs across the wire, so files
    // appear in the guest owned by the host user (UID 1000 in
    // dev). default_permissions on the mount enforces mode bits;
    // 1001 is "other", so the directory needs `o+w` for the
    // create. Cedar remains the authoritative policy gate;
    // mode-bits are belt-and-braces.
    let host_cwd = tempfile::tempdir().expect("host_cwd tempdir");
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(
            host_cwd.path(),
            std::fs::Permissions::from_mode(0o777),
        );
    }

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
        host_cwd_writable: true,
        env: Default::default(),
        network_policy: NetworkPolicy::Deny,
        vm_ceiling: VmCeiling::default(),
        rootfs_version: RootfsVersion::current(),
    };

    let handle = launcher.acquire(&spec).await.expect("acquire");
    let ctx = ToolContext::default();
    let limits = CallLimits::default();

    // Diagnostic FIRST so we always see the logs even when the
    // mount assertion fails.
    let early_diag = handle
        .execute(
            &ctx,
            &limits,
            "bash",
            &json!({ "command": "echo '== /proc/mounts (work) ==';                 grep ' /work ' /proc/mounts || echo NO_WORK_MOUNT;                 echo '== /etc/contextfs/contextfsd.toml ==';                 cat /etc/contextfs/contextfsd.toml 2>/dev/null;                 echo '== contextfsd.log ==';                 tail -40 /var/log/contextfsd.log 2>/dev/null;                 echo '== cfs-broker-vsock-bridge.log ==';                 tail -10 /var/log/cfs-broker-vsock-bridge.log 2>/dev/null;                 echo '== /run/contextfs ==';                 ls -la /run/contextfs/ 2>&1" }),
        )
        .await
        .expect("read early diag");
    eprintln!("[early diag]:\n{}", early_diag.result.model_output);

    // Sanity: /work is FUSE-mounted RW.
    let mounts_out = &early_diag.result.model_output;
    assert!(
        !mounts_out.contains("NO_WORK_MOUNT"),
        "/work not mounted; check guest logs above"
    );
    assert!(
        mounts_out.contains(" rw,") ||
        mounts_out.contains(",rw,") ||
        mounts_out.contains("fuse rw"),
        "/work mount option line missing 'rw,' — broker/RW config didn't take effect"
    );

    // The real assertion: write a sentinel file from inside the
    // sandbox and verify the host can read those bytes back from
    // host_cwd. Goes through:
    //   guest bash → FUSE write → contextfsd verify_write → broker
    //   over /run/contextfs/broker.sock → vsock(2,5006) → host
    //   broker → Cedar permit → contextfsd commits to remote-fs
    //   wire → cfs-fs-server → host_cwd file.
    let sentinel_name = "pi-cfs-rw-sentinel.txt";
    let sentinel_payload = "wrote-from-guest: 0xfeedbeef";
    let write_cmd = format!(
        "printf '{sentinel_payload}' > /work/{sentinel_name} && echo OK"
    );
    let written = handle
        .execute(
            &ctx,
            &limits,
            "bash",
            &json!({ "command": write_cmd }),
        )
        .await
        .expect("execute write to /work");
    eprintln!(
        "exec write: is_error={} duration_ms={} output={:?}",
        written.result.is_error,
        written.guest_duration_ms,
        written.result.model_output
    );
    assert!(
        !written.result.is_error,
        "write to /work/{sentinel_name} returned is_error: {}",
        written.result.model_output
    );

    // Now read back from the host's view.
    let host_view_path = host_cwd.path().join(sentinel_name);
    let host_bytes = std::fs::read_to_string(&host_view_path)
        .expect("host can read the sentinel file written by guest");
    assert_eq!(
        host_bytes, sentinel_payload,
        "host sees {:?}, expected {:?}",
        host_bytes, sentinel_payload
    );

    handle.release().await.expect("release");
    eprintln!("contextfs /work RW mount test PASSED");
}
