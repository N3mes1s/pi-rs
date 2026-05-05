//! Cold vs warm acquire latency measurement (gated like firecracker_smoke).
#![cfg(target_os = "linux")]
use std::time::Instant;
use pi_sandbox::microvm::firecracker::{FirecrackerConfig, FirecrackerLauncher};
use pi_sandbox::microvm::{
    CallLimits, MicroVmLauncher, NetworkPolicy, RootfsVersion, VmCeiling, VmSpec,
};
use pi_tools::ToolContext;
use serde_json::json;

#[tokio::test]
async fn warm_bench() {
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
    for i in 0..5 {
        let t0 = Instant::now();
        let h = launcher.acquire(&spec).await.unwrap();
        let t_acq = t0.elapsed();
        let t1 = Instant::now();
        let exec = h
            .execute(
                &ToolContext::default(),
                &CallLimits::default(),
                "bash",
                &json!({"command": "echo ok"}),
            )
            .await
            .unwrap();
        let t_exec = t1.elapsed();
        let t2 = Instant::now();
        h.release().await.unwrap();
        let t_rel = t2.elapsed();
        eprintln!(
            "run {}: acquire={:?} cold_boot={} exec={:?} release={:?} guest_ms={}",
            i, t_acq, exec.cold_boot, t_exec, t_rel, exec.guest_duration_ms
        );
    }
}
