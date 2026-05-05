//! Seccomp regression: a `bash` subprocess inside the guest must
//! not be able to call `socket(AF_VSOCK, ...)` — that's the
//! policy-bypass route that would let an injected bash payload
//! reach the host's `web_search` proxy listener directly.
//!
//! Approach: have bash invoke `python3 -c '... socket(AF_VSOCK,
//! SOCK_STREAM, 0)'` via Alpine's `apk add python3` (one-shot).
//! Easier: invoke the syscall directly from C via a busybox-shipped
//! tool, OR just write a tiny C program at runtime + compile + run.
//!
//! Simpler still: use bash + /dev/tcp-like syntax — no, bash can't
//! open vsock that way. Use `nc -V 40` — no, busybox nc lacks
//! AF_VSOCK.
//!
//! Cleanest: write a small C source file at runtime, compile with
//! `apk add gcc` (already-installed under our apk-demo path or
//! freshly added here), run it. The seccomp filter must make
//! `socket(AF_VSOCK, SOCK_STREAM, 0)` return EPERM.
//!
//! Gated on `PI_SANDBOX_FC_TEST=1`. No network required — the
//! probe binary is precompiled into the rootfs at build time so
//! we don't need apk-add a compiler, and the seccomp check itself
//! happens entirely inside the guest (no host listener involved).

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
async fn bash_cannot_open_vsock_socket() {
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

    // The rootfs ships a precompiled `pi-vsock-probe` at
    // /usr/local/bin/ (see crates/pi-sandbox-rootfs/build.sh §1
    // and §4). It tries `socket(AF_VSOCK, SOCK_STREAM, 0)` and
    // exits with errno on failure. Under our seccomp filter the
    // syscall is blocked → errno=1 (EPERM) → exit 1.
    let r = h
        .execute(
            &ToolContext::default(),
            &CallLimits {
                wall_timeout: Duration::from_secs(15),
                ..Default::default()
            },
            "bash",
            &json!({
                "command": "/usr/local/bin/pi-vsock-probe; echo \"rc=$?\""
            }),
        )
        .await
        .expect("run probe");
    eprintln!("vsock probe:\n{}", r.result.model_output);

    let body = r.result.model_output.to_lowercase();
    assert!(
        body.contains("operation not permitted") && body.contains("rc=1"),
        "bash's `socket(AF_VSOCK, ...)` must be EPERM under seccomp; got: {}",
        r.result.model_output
    );
    assert!(
        !body.contains("this is bad"),
        "bash succeeded in opening AF_VSOCK — seccomp filter NOT applied or too permissive: {}",
        r.result.model_output
    );

    h.release().await.expect("release");
}
