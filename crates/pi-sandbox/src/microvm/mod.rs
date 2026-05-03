//! Local microVM sandbox traits + types (RFD 0023 §2).
//!
//! `MicroVmLauncher` is the per-OS abstraction (Firecracker on
//! Linux, vfkit on macOS, cloud-hypervisor on Windows — all
//! land in subsequent commits). `VmHandle` is the single-VM
//! handle surface used per tool call. `VmSpec` is the input,
//! `VmExecution` is the output.

pub mod launcher;
pub mod types;

#[cfg(target_os = "linux")]
pub mod firecracker;
#[cfg(target_os = "linux")]
pub use firecracker::FirecrackerLauncher;

pub use launcher::{MicroVmLauncher, VmHandle};
pub use types::{
    CallLimits, NetworkPolicy, ProbeCheck, ProbeReport, RootfsVersion,
    VmCeiling, VmExecution, VmSpec, ROOTFS_VERSION,
};
