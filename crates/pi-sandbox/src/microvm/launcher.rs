//! `MicroVmLauncher` and `VmHandle` traits.

use async_trait::async_trait;
use pi_tools::ToolContext;

use crate::microvm::types::{CallLimits, ProbeReport, VmExecution, VmSpec};
use crate::SandboxError;

/// Per-OS local microVM launcher abstraction.
///
/// Implementations:
///   * `FirecrackerLauncher` (Linux) — RFD 0023 Commit D
///   * `VfkitLauncher` (macOS) — RFD 0023 Commit E
///   * `CloudHypervisorLauncher` (Windows) — RFD 0023 Commit F
///
/// Each impl is `#[cfg]`-gated to its OS so the workspace builds
/// cross-platform.
#[async_trait]
pub trait MicroVmLauncher: Send + Sync {
    /// Slug used in telemetry; one of "firecracker", "vfkit",
    /// "cloud-hypervisor". Stable across patch releases.
    fn transport_name(&self) -> &'static str;

    /// Probe at construction. Lets `pi sandbox doctor` produce
    /// actionable diagnostics without booting anything.
    async fn probe(&self) -> Result<ProbeReport, SandboxError>;

    /// Acquire a VM ready to execute a tool call. v1.0 launchers
    /// MAY return a pooled+warm-restored VM (FirecrackerLauncher
    /// MUST per RFD 0023 §4); others may cold-boot.
    async fn acquire(&self, spec: &VmSpec) -> Result<Box<dyn VmHandle>, SandboxError>;
}

/// Handle to a single acquired VM. Returned by
/// `MicroVmLauncher::acquire()`, used to send one tool call,
/// then released.
#[async_trait]
pub trait VmHandle: Send + Sync {
    /// Send one ToolRequest, await one ToolResponse over vsock.
    /// Bridges `ToolContext` from the runtime by serialising the
    /// fields the guest needs (cwd, max_output_bytes), plus the
    /// per-call limits (wall_timeout, max_output_bytes). Per-call
    /// is the right scope for limits — a long bash build needs
    /// a generous timeout but is in the same VM as a quick `ls`
    /// the next call over.
    async fn execute(
        &self,
        ctx: &ToolContext,
        limits: &CallLimits,
        tool_name: &str,
        tool_input: &serde_json::Value,
    ) -> Result<VmExecution, SandboxError>;

    /// Release the VM. v1.0 in pooled mode = return to pool;
    /// non-pooled = shutdown.
    async fn release(self: Box<Self>) -> Result<(), SandboxError>;
}
