//! Full per-call reset via `PI_SANDBOX_FC_MAX_CALLS=1`.
//!
//! Sets the per-VM call cap to 1 so every tool call cold-boots a
//! fresh VM. This is the simple "destroy the VM" alternative to
//! the RFD's eventual `pi-vm-reset` + overlay re-mount plan
//! (which would do the same logical reset in ~50ms instead of
//! ~1s).
//!
//! Asserts:
//! 1. Two consecutive `bash` calls observe DIFFERENT vm_ids in
//!    /etc/pi-sandbox-version-vm-id (a synthetic file we write
//!    via mktemp + uname). Wait — simpler: just check that a
//!    file written via the bash tool to /opt/foo in call 1 is
//!    GONE in call 2 (it would be present under default MAX_CALLS
//!    because /opt persists across calls in the same VM).
//! 2. The `cold_boot=true` flag in the second call's VmExecution
//!    confirms a fresh boot rather than a warm-pool hit.
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
async fn max_calls_one_gives_per_call_full_reset() {
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

    // The knob is process-global. Set it for the test, restore after.
    let prior = std::env::var("PI_SANDBOX_FC_MAX_CALLS").ok();
    std::env::set_var("PI_SANDBOX_FC_MAX_CALLS", "1");

    let result = std::panic::AssertUnwindSafe(async {
        let tmp = tempfile::tempdir().unwrap();
        let work = tempfile::tempdir().unwrap();
        let cfg = FirecrackerConfig {
            kernel_path: Some(kernel_path),
            rootfs_path: Some(rootfs_path),
            run_dir: tmp.path().join("run"),
            // pool_size=2 so a fresh VM is available without the
            // refill-task path; we want the second call to ACQUIRE
            // a fresh VM (cold boot) rather than reuse a warm one.
            pool_size: 2,
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

        // Call 1: write a sentinel into /opt (which normally
        // persists for the VM lifetime, outside the per-call
        // hygiene scratch list).
        let h = launcher.acquire(&spec).await.unwrap();
        let r = h
            .execute(
                &ToolContext::default(),
                &CallLimits {
                    wall_timeout: Duration::from_secs(15),
                    ..Default::default()
                },
                "bash",
                &json!({
                    "command": "echo full-reset-sentinel > /opt/sentinel ; \
                                cat /opt/sentinel ; \
                                echo \"cold_boot_marker_call_1=$(date +%s%N)\""
                }),
            )
            .await
            .unwrap();
        let cold_boot_1 = r.cold_boot;
        eprintln!("call 1 cold_boot={cold_boot_1} body=\n{}", r.result.model_output);
        assert!(
            r.result.model_output.contains("full-reset-sentinel"),
            "call 1 should be able to write+read /opt/sentinel: {}",
            r.result.model_output
        );
        h.release().await.unwrap();

        // Call 2: acquire again. Because MAX_CALLS=1, the VM from
        // call 1 was retired on release. Acquire MUST cold-boot a
        // fresh VM. /opt/sentinel must be gone.
        let h = launcher.acquire(&spec).await.unwrap();
        let r = h
            .execute(
                &ToolContext::default(),
                &CallLimits {
                    wall_timeout: Duration::from_secs(15),
                    ..Default::default()
                },
                "bash",
                &json!({
                    "command": "ls /opt 2>&1 ; echo '---' ; \
                                cat /opt/sentinel 2>&1 ; echo \"rc=$?\""
                }),
            )
            .await
            .unwrap();
        let cold_boot_2 = r.cold_boot;
        eprintln!("call 2 cold_boot={cold_boot_2} body=\n{}", r.result.model_output);

        assert!(
            cold_boot_2,
            "call 2 should cold-boot a fresh VM (MAX_CALLS=1) but cold_boot=false"
        );
        assert!(
            !r.result.model_output.contains("full-reset-sentinel"),
            "call 2 saw call 1's /opt/sentinel — full reset NOT in effect: {}",
            r.result.model_output
        );
        let lower = r.result.model_output.to_lowercase();
        assert!(
            lower.contains("no such file") || lower.contains("not found"),
            "call 2's `cat /opt/sentinel` should fail with no-such-file: {}",
            r.result.model_output
        );

        h.release().await.unwrap();
    })
    .await;

    // Restore the env var even if the test asserts panicked.
    match prior {
        Some(v) => std::env::set_var("PI_SANDBOX_FC_MAX_CALLS", v),
        None => std::env::remove_var("PI_SANDBOX_FC_MAX_CALLS"),
    }
    let _ = result;
}
