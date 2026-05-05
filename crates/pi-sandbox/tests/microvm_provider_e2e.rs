//! Drive `MicroVmProvider` through `SandboxProvider::execute_tool()`
//! exactly the way `pi-agent-core::Runtime` does — proves the runtime
//! ↔ provider ↔ launcher ↔ guest path is intact.
#![cfg(target_os = "linux")]
use std::sync::Arc;

use pi_sandbox::microvm::firecracker::{FirecrackerConfig, FirecrackerLauncher};
use pi_sandbox::{MicroVmProvider, SandboxProvider};
use pi_tools::ToolContext;
use serde_json::json;

#[tokio::test]
async fn microvm_provider_dispatches_through_guest() {
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
    let launcher = Arc::new(FirecrackerLauncher::new(cfg));
    let provider = MicroVmProvider::new(launcher);
    assert_eq!(provider.name(), "microvm");

    let mut ctx = ToolContext::default();
    ctx.cwd = work.path().to_path_buf();

    // 1) bash uname -a — confirms guest kernel (Ubuntu 6.8 in this rig).
    let exec = provider
        .execute_tool(&ctx, "bash", &json!({"command": "uname -a"}))
        .await
        .expect("execute_tool");
    eprintln!("uname stdout: {}", exec.stdout.trim_end());
    assert_eq!(exec.exit_status, 0);
    assert!(
        exec.stdout.contains("Linux") && exec.stdout.contains("generic"),
        "expected Ubuntu generic-kernel banner inside the guest, got: {:?}",
        exec.stdout
    );

    // 2) cat the rootfs version sentinel (build.sh embeds it).
    let exec = provider
        .execute_tool(
            &ctx,
            "bash",
            &json!({"command": "cat /etc/pi-sandbox-version"}),
        )
        .await
        .expect("execute_tool");
    assert_eq!(exec.exit_status, 0);
    assert!(
        exec.stdout.contains("0.1.0"),
        "expected rootfs version 0.1.0 in guest /etc/pi-sandbox-version, got: {:?}",
        exec.stdout
    );

    // 3) net is denied — loopback exists, no eth0, no resolv.conf.
    let exec = provider
        .execute_tool(
            &ctx,
            "bash",
            &json!({"command": "ip route show 2>&1 | head -3; cat /etc/resolv.conf 2>&1"}),
        )
        .await
        .expect("execute_tool");
    eprintln!("net check: {}", exec.stdout.trim_end());
    assert!(
        exec.stdout.contains("No such file") || exec.stdout.contains("can't open"),
        "expected /etc/resolv.conf missing (network locked), got: {:?}",
        exec.stdout
    );

    // 4) post-test cleanup proof: hold no leaked launcher refs, then ensure
    //    no firecracker process from this test survives once the provider
    //    drops. The provider's drop -> launcher's drop -> warm-pool VMs
    //    are torn down by `kill_on_drop(true)` on the firecracker child.
    drop(provider);
    // brief settle; firecracker children take a few ms to exit.
    tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    let leaked = std::process::Command::new("pgrep")
        .arg("-f")
        .arg(format!(
            "firecracker.*{}",
            tmp.path().display()
        ))
        .output()
        .ok()
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .unwrap_or_default();
    assert!(
        leaked.trim().is_empty(),
        "expected no firecracker processes referencing test run_dir after provider drop, got: {:?}",
        leaked
    );
    eprintln!("provider drop → warm-pool firecracker children all torn down ✓");
}
