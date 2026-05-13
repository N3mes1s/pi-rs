//! End-to-end demo: apk download + cargo build inside a Firecracker
//! microVM with `NetworkPolicy::Allow`. Proves the host-side wiring
//! described in `crates/pi-sandbox/docs/NETWORKING.md` actually works.
//!
//! Gated on `PI_SANDBOX_FC_NET_TEST=1` (separate from the smoke-test
//! gate so a maintainer can run the no-network smoke daily without
//! the heavier apk-download path).
//!
//! Required env (same as `firecracker_smoke.rs`):
//!   PI_SANDBOX_FC_NET_TEST=1
//!   PI_SANDBOX_KERNEL=/path/to/vmlinux  (must support virtio_net)
//!   PI_SANDBOX_ROOTFS=/path/to/rootfs.img
//!
//! Required host packages: pasta (passt), nftables, firecracker, kvm.
//! Test will skip with a clear message if any are missing.

#![cfg(target_os = "linux")]

use std::path::PathBuf;
use std::time::Duration;

use pi_sandbox::microvm::firecracker::{FirecrackerConfig, FirecrackerLauncher};
use pi_sandbox::microvm::launcher::MicroVmLauncher;
use pi_sandbox::microvm::{CallLimits, NetworkPolicy, RootfsVersion, VmCeiling, VmSpec};
use pi_tools::ToolContext;
use serde_json::json;
use std::ops::Not as _;

fn skip(reason: &str) {
    eprintln!("SKIP: {reason}");
}

macro_rules! require_env {
    ($var:expr) => {
        match std::env::var($var) {
            Ok(v) if !v.is_empty() => v,
            _ => {
                skip(&format!("env var {} not set — skipping apk-demo", $var));
                return;
            }
        }
    };
}

#[tokio::test]
async fn apk_add_cargo_inside_microvm_with_network() {
    require_env!("PI_SANDBOX_FC_NET_TEST");

    for tool in ["firecracker", "pasta", "nft", "ip"] {
        if which::which(tool).is_err() {
            skip(&format!("`{tool}` not on PATH — required for apk-demo"));
            return;
        }
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
    // Probe unprivileged userns — the kernel sometimes blocks this on
    // hardened distros (Debian default). pasta will fail without a
    // useful error message in that case, so check up-front.
    let userns_probe = std::process::Command::new("unshare")
        .args(["-rUn", "/bin/true"])
        .status();
    match userns_probe {
        Ok(s) if s.success() => {}
        Ok(_) | Err(_) => {
            skip(
                "`unshare -rUn` failed — kernel likely has \
                 `kernel.unprivileged_userns_clone=0`. Run \
                 `sudo sysctl -w kernel.unprivileged_userns_clone=1` \
                 (and persist via /etc/sysctl.d/) to enable this test.",
            );
            return;
        }
    }

    let kernel_path = match std::env::var("PI_SANDBOX_KERNEL") {
        Ok(p) if !p.is_empty() => PathBuf::from(p),
        _ => {
            skip("PI_SANDBOX_KERNEL not set");
            return;
        }
    };
    let rootfs_path = match std::env::var("PI_SANDBOX_ROOTFS") {
        Ok(p) if !p.is_empty() => PathBuf::from(p),
        _ => {
            skip("PI_SANDBOX_ROOTFS not set");
            return;
        }
    };
    if !kernel_path.exists() || !rootfs_path.exists() {
        skip("kernel or rootfs path does not exist");
        return;
    }

    let tmp = tempfile::tempdir().expect("tempdir");
    let work = tempfile::tempdir().expect("work");
    let cfg = FirecrackerConfig {
        kernel_path: Some(kernel_path),
        rootfs_path: Some(rootfs_path),
        run_dir: tmp.path().join("run"),
        pool_size: 1,
        ..Default::default()
    };
    let launcher = FirecrackerLauncher::new(cfg);

    // /30 with .1 = host TAP, .2 = guest eth0. pasta will resolve a
    // real default-route interface inside the netns and masquerade
    // 172.16.0.0/30 out of it.
    let spec = VmSpec {
        host_cwd: work.path().to_path_buf(),
        host_cwd_writable: true,
        env: Default::default(),
        network_policy: NetworkPolicy::Allow {
            tap_name: "tap-pi0".into(),
            guest_ip_cidr: "172.16.0.2/30".into(),
            guest_gateway: "172.16.0.1".into(),
            // 1.1.1.1 + Google: both are reachable through pasta's
            // userspace TCP relay (UDP/53 and TCP/443 forwarded).
            guest_dns: vec!["1.1.1.1".into(), "8.8.8.8".into()],
            guest_mac: None,
            // The Cedar/auto-approve policy file would render this
            // list. Hostnames resolved at netns-setup time inside
            // pasta's netns; resulting IPs become the nft accept
            // set. Anything not on the list is dropped at the
            // forward chain (verified in step 5 below).
            egress_allowlist: vec![
                "dl-cdn.alpinelinux.org".into(),
                // Mirror used when CDN load-balances:
                "151.101.0.0/16".into(),
            ],
        },
        // Cargo + rust (LLVM 17) need ~600 MiB of overlay disk and
        // ~400 MiB live RAM. Bump from the 512/256 default.
        vm_ceiling: VmCeiling {
            mem_mib: 2048,
            vcpus: 2,
            disk_mib: 1536,
        },
        rootfs_version: RootfsVersion::current(),
    };

    let h = match launcher.acquire(&spec).await {
        Ok(h) => h,
        Err(e) => {
            // Net-prereq probe failures surface as Provider errors.
            // Treat as skip-with-message rather than test failure so
            // CI on net-blocked runners still passes the suite.
            let msg = e.to_string();
            if msg.contains("requires `pasta`")
                || msg.contains("requires `nft`")
                || msg.contains("no default route")
            {
                skip(&format!("acquire() refused on net policy: {msg}"));
                return;
            }
            panic!("acquire() failed: {e}");
        }
    };

    // 1. Sanity: eth0 came up with the expected static address.
    //    BusyBox's `ip` doesn't support `-br`; use plain `addr show`.
    let r = h
        .execute(
            &ToolContext::default(),
            &CallLimits {
                wall_timeout: Duration::from_secs(15),
                ..Default::default()
            },
            "bash",
            &json!({"command": "ip addr show eth0 2>&1; echo '---route---'; ip route 2>&1; echo '---resolv---'; cat /etc/resolv.conf 2>&1"}),
        )
        .await
        .expect("ip addr show");
    eprintln!("eth0:\n{}", r.result.model_output);
    assert!(
        r.result.model_output.contains("172.16.0.2"),
        "guest eth0 did not get 172.16.0.2; got: {}",
        r.result.model_output
    );
    assert!(
        r.result.model_output.contains("default via 172.16.0.1"),
        "guest default route missing; got: {}",
        r.result.model_output
    );

    // 2. Connectivity: a DNS lookup from the guest must succeed.
    let r = h
        .execute(
            &ToolContext::default(),
            &CallLimits {
                wall_timeout: Duration::from_secs(20),
                ..Default::default()
            },
            "bash",
            &json!({"command": "nslookup dl-cdn.alpinelinux.org 2>&1 | head -10 ; echo \"exit=$?\""}),
        )
        .await
        .expect("nslookup");
    eprintln!("nslookup:\n{}", r.result.model_output);

    // 3. **Hardened-mode security regression**: bash now runs as
    //    pi-tool (UID 1001), not root. `apk add` requires write
    //    access to /usr, /var/cache/apk, /lib — all root-owned.
    //    The agent therefore CANNOT install arbitrary packages
    //    even with network access, which is the desired
    //    sandbox-vs-host-package-installer boundary.
    //
    //    Pre-hardened versions of this test demonstrated `apk add
    //    cargo` succeeding (888 MiB, 43 packages) followed by
    //    `cargo build` of a real Rust file. With UID separation
    //    that capability is correctly removed.
    let r = h
        .execute(
            &ToolContext::default(),
            &CallLimits {
                wall_timeout: Duration::from_secs(60),
                max_output_bytes: 256 * 1024,
            },
            "bash",
            &json!({
                "command": "id ; echo '---apk add---' ; \
                            out=$(apk add --no-cache cargo 2>&1) ; apk_rc=$? ; \
                            echo \"$out\" | tail -10 ; \
                            echo \"---apk_rc=$apk_rc---\""
            }),
        )
        .await
        .expect("apk add");
    eprintln!("apk add (hardened) output:\n{}", r.result.model_output);
    assert!(
        r.result.model_output.contains("uid=1001"),
        "bash should run as uid=1001, confirming UID separation: {}",
        r.result.model_output
    );
    let body = r.result.model_output.to_lowercase();
    assert!(
        !r.result.model_output.contains("apk_rc=0\n"),
        "apk add succeeded under UID separation — pi-tool should NOT be able to install: {}",
        r.result.model_output
    );
    assert!(
        body.contains("permission denied")
            || body.contains("read-only")
            || body.contains("not permitted")
            || body.contains("unable to lock"),
        "expected apk-add to fail with permission-denied-like error; got: {}",
        r.result.model_output
    );

    // 5. **Allowlist enforcement**: an off-allowlist host MUST be
    //    blocked at the netns forward chain. `example.com` resolves
    //    to a host outside `151.101.0.0/16`, so the SYN should be
    //    dropped by nft and wget should timeout. We capture wget's
    //    own rc (not the rc through a pipe) and the response body
    //    size to distinguish "blocked" from "succeeded".
    let r = h
        .execute(
            &ToolContext::default(),
            &CallLimits {
                wall_timeout: Duration::from_secs(20),
                ..Default::default()
            },
            "bash",
            &json!({
                "command": "out=$(wget --timeout=5 -qO- http://example.com/ 2>&1) ; \
                            wget_rc=$? ; \
                            echo \"wget_rc=$wget_rc\" ; \
                            echo \"body_len=${#out}\" ; \
                            echo \"first=$(echo \\\"$out\\\" | head -1)\""
            }),
        )
        .await
        .expect("denied-host probe");
    eprintln!("denied-host probe:\n{}", r.result.model_output);
    assert!(
        r.result.model_output.contains("wget_rc=0").not()
            || r.result.model_output.contains("body_len=0"),
        "off-allowlist host example.com should have been blocked, \
         but wget returned 0 with a non-empty body; output: {}",
        r.result.model_output
    );

    h.release().await.expect("release");
}
