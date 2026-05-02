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
fn rootfs_version_current_matches_rootfs_crate() {
    let v = RootfsVersion::current();
    assert_eq!(v.0, pi_sandbox_rootfs::ROOTFS_VERSION);
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
