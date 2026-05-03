//! Plain-old-data types for the MicroVmLauncher trait surface.

use serde::Serialize;
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::time::Duration;

use pi_tool_types::ToolResult;

/// Input to `MicroVmLauncher::acquire()`. Describes the VM the
/// caller wants — host_cwd to mount, env to forward, network
/// policy, resource ceiling, rootfs version pin.
#[derive(Debug, Clone)]
pub struct VmSpec {
    /// Host path mounted at /work in the guest (virtio-fs RW).
    pub host_cwd: PathBuf,
    /// Whether /work is writable. v1.0 is always true; the flag
    /// stays so future per-tool policy can mount RO selectively.
    pub host_cwd_writable: bool,
    /// Environment variables forwarded into the guest. The full
    /// host env is NEVER forwarded; only this allowlist.
    pub env: BTreeMap<String, String>,
    /// Network policy for the guest. v1.0 only supports `Deny`.
    pub network_policy: NetworkPolicy,
    /// Per-VM resource ceiling — the absolute cap on what the VM
    /// may consume. Per-call limits in `CallLimits` are evaluated
    /// against this; never exceed it.
    pub vm_ceiling: VmCeiling,
    /// Which rootfs version to boot. Pinned to one image per
    /// `proto_version`; mismatch refuses to boot.
    pub rootfs_version: RootfsVersion,
}

/// Network policy enum. v1.0 only supports `Deny`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NetworkPolicy {
    /// Guest has no network access.
    Deny,
    // Future: AllowList(Vec<DomainPattern>), AllowAll
}

/// VM-level ceiling. Set at acquire(); cannot change without
/// rebooting the VM. Pool partitioning is keyed by this so a
/// pool acquire returns a VM whose ceiling matches the request.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct VmCeiling {
    /// Memory ceiling in MiB. Default 512.
    pub mem_mib: u32,
    /// vCPU count. Default 2.
    pub vcpus: u8,
    /// Ephemeral overlay disk size in MiB. Default 256.
    pub disk_mib: u32,
}

impl Default for VmCeiling {
    fn default() -> Self {
        Self { mem_mib: 512, vcpus: 2, disk_mib: 256 }
    }
}

/// Per-CALL limits. Evaluated against the VM's `VmCeiling` and
/// applied to the single tool execution. A long bash build can
/// raise its `wall_timeout` without forcing a fresh VM boot.
#[derive(Debug, Clone, Copy)]
pub struct CallLimits {
    /// Wall-clock timeout for this single tool call. Default 60s.
    pub wall_timeout: Duration,
    /// Cap on the response's stdout size. Default 256 KiB.
    pub max_output_bytes: u32,
}

impl Default for CallLimits {
    fn default() -> Self {
        Self {
            wall_timeout: Duration::from_secs(60),
            max_output_bytes: 256 * 1024,
        }
    }
}

/// Pinned rootfs version that the launcher should boot. Mismatch
/// → SandboxError::RootfsMismatch.
///
/// **Single source of truth.** Per code-review pass-7 NIT #2: the
/// version literal lives in `pi-sandbox/src/cache.rs::ROOTFS_VERSION`
/// (alongside `ROOTFS_URL`/`ROOTFS_SHA256`/`ROOTFS_SIZE_BYTES` which
/// the maintainer pastes from build.sh's output on each version bump).
/// `microvm::ROOTFS_VERSION` here is a `pub use` re-export so the
/// launcher's runtime check, the cache manifest, and the public
/// re-export at `pi_sandbox::microvm::ROOTFS_VERSION` all agree.
///
/// **Sync requirement:** the literal in `cache.rs:29` MUST match
/// the `VERSION` constant baked into `crates/pi-sandbox-rootfs/build.sh`
/// (line ~28) — build.sh stamps this value into the rootfs artifact's
/// `/etc/pi-sandbox-version` and the launcher rejects boots where the
/// two disagree. Bump both in lockstep when shipping a new rootfs.
pub use crate::cache::ROOTFS_VERSION;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RootfsVersion(pub String);

impl RootfsVersion {
    pub fn current() -> Self {
        Self(ROOTFS_VERSION.to_string())
    }
}

/// Output of `VmHandle::execute()`. Includes the standard
/// ToolResult plus VM-level observability (boot timing, cold-boot
/// flag) so the host's telemetry layer can attribute pool-miss
/// latency.
pub struct VmExecution {
    /// Shape compatible with the inline-path ToolResult.
    pub result: ToolResult,
    /// Time spent inside the guest, in milliseconds.
    pub guest_duration_ms: u32,
    /// Time from `acquire()` to first vsock connection, in
    /// milliseconds. NOT pure boot time — includes guest init,
    /// vsock listen, accept handshake. The host can't see "boot
    /// finished" without guest cooperation, so this is the most
    /// honest end-to-end measurement.
    pub acquire_to_ready_ms: u32,
    /// True when this acquire required a cold boot (pool miss).
    pub cold_boot: bool,
}

/// Probe report — what `pi sandbox doctor` shows.
#[derive(Debug, Clone, Serialize)]
pub struct ProbeReport {
    pub transport: &'static str,
    pub available: bool,
    pub version: Option<String>,
    pub probe_duration_ms: u32,
    pub blockers: Vec<String>,
    pub remediation: Vec<String>,
    /// Per-precondition results so doctor can show what's
    /// actually broken, not just "available=false".
    pub checks: Vec<ProbeCheck>,
}

/// Single precondition check result.
#[derive(Debug, Clone, Serialize)]
pub struct ProbeCheck {
    pub name: &'static str,
    pub passed: bool,
    pub detail: Option<String>,
}
