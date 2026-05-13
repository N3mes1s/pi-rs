//! Trait-shape tests — confirm the surface is what RFD 0023 §2
//! specifies. No implementations exercised; just confirms the
//! types compile + serialize as expected.

use pi_sandbox::*;
use std::path::PathBuf;

#[test]
fn vm_ceiling_default_is_512_mib_2_vcpu_256_disk() {
    let c = VmCeiling::default();
    assert_eq!(c.mem_mib, 512);
    assert_eq!(c.vcpus, 2);
    assert_eq!(c.disk_mib, 256);
}

#[test]
fn call_limits_default_is_60s_256kib() {
    let l = CallLimits::default();
    assert_eq!(l.wall_timeout, std::time::Duration::from_secs(60));
    assert_eq!(l.max_output_bytes, 256 * 1024);
}

#[test]
fn rootfs_version_current_matches_inlined_const() {
    // Pre-pass-6 finding #1: the const lived in the `pi-sandbox-rootfs`
    // workspace crate; pi-sandbox depended on it. That crate is
    // `publish = false` (build-recipe scaffolding only), which would
    // have made pi-sandbox unpublishable. Const moved into pi-sandbox
    // itself; this test guards the rename.
    let v = RootfsVersion::current();
    assert_eq!(v.0, pi_sandbox::microvm::ROOTFS_VERSION);

    // Per code-review pass-8 NON-BLOCKING #3: the const ALSO lives in
    // `pi_sandbox::cache::ROOTFS_VERSION` (alongside ROOTFS_URL etc.
    // that maintainers paste from build.sh's output). pass-7 NIT #2
    // unified them via `pub use crate::cache::ROOTFS_VERSION` in
    // microvm/types.rs. A future regression that re-introduces a
    // literal duplicate (say, copy-pasting the const into types.rs)
    // would silently boot the wrong cached image because the
    // launcher's version-check uses cache.rs's const. This assert
    // ensures both names resolve to the same value, so the regression
    // fails immediately.
    assert_eq!(
        pi_sandbox::cache::ROOTFS_VERSION,
        pi_sandbox::microvm::ROOTFS_VERSION,
        "ROOTFS_VERSION must be a single source of truth — \
         microvm::ROOTFS_VERSION should `pub use crate::cache::ROOTFS_VERSION`"
    );
}

#[test]
fn probe_report_serializes_as_json() {
    let r = ProbeReport {
        transport: "firecracker",
        available: true,
        version: Some("v1.15.0".into()),
        probe_duration_ms: 87,
        blockers: vec![],
        remediation: vec![],
        checks: vec![ProbeCheck {
            name: "kvm_open_rw",
            passed: true,
            detail: None,
        }],
    };
    let s = serde_json::to_string(&r).unwrap();
    assert!(s.contains("\"transport\":\"firecracker\""));
    assert!(s.contains("\"kvm_open_rw\""));
}

#[test]
fn vm_spec_default_does_not_compile_must_be_explicit() {
    // We deliberately do NOT impl Default for VmSpec — every
    // caller must supply host_cwd explicitly. This test exists
    // as a marker that future commits should not implement
    // Default for VmSpec without a design discussion.
    let _ = VmSpec {
        host_cwd: PathBuf::from("/tmp"),
        host_cwd_writable: true,
        env: Default::default(),
        network_policy: NetworkPolicy::Deny,
        vm_ceiling: VmCeiling::default(),
        rootfs_version: RootfsVersion::current(),
    };
}

// MicroVmLauncher and VmHandle have no impls in this commit;
// that's intentional. D / E / F land them.
