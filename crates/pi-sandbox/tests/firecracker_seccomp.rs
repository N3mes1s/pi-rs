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
//! Gated on `PI_SANDBOX_FC_NET_TEST=1` (uses the Allow path so the
//! search-proxy listener IS bound and the bypass would actually
//! land somewhere if seccomp didn't block it).

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
    if std::env::var("PI_SANDBOX_FC_NET_TEST").is_err() {
        return skip("PI_SANDBOX_FC_NET_TEST not set");
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
    // Allow + an allowlist for the alpine repos so apk add gcc works.
    let spec = VmSpec {
        host_cwd: work.path().to_path_buf(),
        host_cwd_writable: true,
        env: Default::default(),
        network_policy: NetworkPolicy::Allow {
            tap_name: "tap-pi0".into(),
            guest_ip_cidr: "172.16.0.2/30".into(),
            guest_gateway: "172.16.0.1".into(),
            guest_dns: vec!["1.1.1.1".into(), "8.8.8.8".into()],
            guest_mac: None,
            egress_allowlist: vec!["dl-cdn.alpinelinux.org".into(), "151.101.0.0/16".into()],
        },
        // Match the apk-demo's ceiling — gcc + cargo etc. need the
        // bigger overlay tmpfs.
        vm_ceiling: VmCeiling {
            mem_mib: 2048,
            vcpus: 2,
            disk_mib: 1536,
        },
        rootfs_version: RootfsVersion::current(),
    };

    let h = launcher.acquire(&spec).await.expect("acquire");

    // 1. Install a C compiler so we can craft the syscall test.
    //    apk add gcc + musl-dev. This proves the network-allowed
    //    path is intact; a separate test (apk_demo) covers the
    //    same ground but for cargo. We need just gcc + libc
    //    headers here.
    let r = h
        .execute(
            &ToolContext::default(),
            &CallLimits {
                wall_timeout: Duration::from_secs(180),
                max_output_bytes: 256 * 1024,
            },
            "bash",
            &json!({
                "command": "apk add --no-cache gcc musl-dev 2>&1 | tail -3 ; \
                            which gcc; gcc --version | head -1"
            }),
        )
        .await
        .expect("apk add gcc");
    eprintln!("apk add gcc:\n{}", r.result.model_output);
    assert!(
        !r.result.is_error && r.result.model_output.contains("/usr/bin/gcc"),
        "apk add gcc failed: {}",
        r.result.model_output
    );

    // 2. Write a tiny C program that calls socket(AF_VSOCK, ...)
    //    and exits with the errno on failure (or 0 on success).
    //    AF_VSOCK == 40 on Linux. Build it. Run it. Expect EPERM (1)
    //    from seccomp — the syscall is filtered.
    //
    //    Note: we put the source file under /opt/ rather than /tmp
    //    because /tmp is wiped by per-call hygiene between calls.
    //    Within ONE call the source persists, so this test could
    //    use /tmp too — but writing to /opt makes the script
    //    idempotent across re-runs of the test on the same VM.
    let r = h
        .execute(
            &ToolContext::default(),
            &CallLimits {
                wall_timeout: Duration::from_secs(60),
                max_output_bytes: 64 * 1024,
            },
            "bash",
            &json!({
                "command": "set -e ; mkdir -p /opt/vsocktest ; cd /opt/vsocktest ; \
                    cat > probe.c <<'EOF'\n\
                    #include <sys/socket.h>\n\
                    #include <stdio.h>\n\
                    #include <errno.h>\n\
                    #include <string.h>\n\
                    int main(void) {\n\
                      int fd = socket(40 /* AF_VSOCK */, 1 /* SOCK_STREAM */, 0);\n\
                      if (fd < 0) {\n\
                        int e = errno;\n\
                        printf(\"socket failed errno=%d (%s)\\n\", e, strerror(e));\n\
                        return e;\n\
                      }\n\
                      printf(\"socket succeeded fd=%d (THIS IS BAD)\\n\", fd);\n\
                      return 0;\n\
                    }\n\
                    EOF\n\
                    gcc -static probe.c -o probe 2>&1 ; \
                    ./probe ; \
                    echo \"rc=$?\""
            }),
        )
        .await
        .expect("compile + run probe");
    eprintln!("vsock probe:\n{}", r.result.model_output);

    let body = r.result.model_output.to_lowercase();
    // "operation not permitted" (errno 1, EPERM) is the seccomp
    // result. We accept either the literal string or the rc=1.
    assert!(
        body.contains("operation not permitted") || body.contains("rc=1"),
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
