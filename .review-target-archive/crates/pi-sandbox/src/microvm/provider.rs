//! `MicroVmProvider` — implements `SandboxProvider` over a
//! `MicroVmLauncher`, routing tool calls through an acquired VM and
//! releasing it on completion.
//!
//! G1 scope (this commit): no contextfs `/work` mount yet; the
//! provider runs `bash` and other guest-bound tools against the
//! rootfs only. Reads/writes to host paths inside the guest will
//! fail until G3 wires contextfs in. This is enough for the model
//! to actually go through the microvm boundary for any tool whose
//! input doesn't reach outside the guest (e.g. `bash 'uname -a'`,
//! `bash 'echo $PATH'`).
//!
//! Tool-disposition-aware routing (`monitor` Unavailable, etc.) is
//! kept simple: anything not implementable in the guest returns a
//! `SandboxError::Provider("...")` for now. The full
//! `tool_disposition()` plan-time API per RFD 0023 v0.41 lands in a
//! later commit alongside the trait reshape to
//! `Result<SandboxOutcome, SandboxFailure>`.

use std::sync::Arc;

use async_trait::async_trait;
use pi_tool_types::ToolResult;
use pi_tools::ToolContext;

use crate::microvm::launcher::MicroVmLauncher;
use crate::microvm::types::{CallLimits, NetworkPolicy, RootfsVersion, VmCeiling, VmSpec};
use crate::provider::{SandboxError, SandboxExecution, SandboxProvider};

/// `SandboxProvider` impl that routes every tool call through a
/// short-lived (or warm-pooled) microVM via the configured
/// `MicroVmLauncher`.
pub struct MicroVmProvider {
    launcher: Arc<dyn MicroVmLauncher>,
    /// Default per-call resource caps. Bash builds want a generous
    /// timeout; quick reads don't need it but the cap is harmless.
    default_call_limits: CallLimits,
    /// Default `VmSpec` used at acquire time. `host_cwd` is set
    /// per-call from `ToolContext::cwd`; everything else (memory,
    /// vCPUs, network policy) comes from this default unless the
    /// host overrides. The default is `NetworkPolicy::Deny` per
    /// RFD 0023 v1 ("guests have no network").
    default_vm_ceiling: VmCeiling,
    default_network_policy: NetworkPolicy,
    rootfs_version: RootfsVersion,
}

impl MicroVmProvider {
    pub fn new(launcher: Arc<dyn MicroVmLauncher>) -> Self {
        Self::with_network_policy(launcher, NetworkPolicy::Deny)
    }

    /// Construct a `MicroVmProvider` with an explicit network policy.
    /// `NetworkPolicy::Deny` is safe-default (`new`); callers that
    /// want host-proxied tools (`web_search`) or selective egress
    /// pass `NetworkPolicy::Allow { ... }` here. Per
    /// `crates/pi-sandbox/docs/NETWORKING.md` §"Vsock-proxied tools
    /// and `NetworkPolicy`", the listener-bind for `web_search` is
    /// gated on this exact value.
    pub fn with_network_policy(
        launcher: Arc<dyn MicroVmLauncher>,
        network_policy: NetworkPolicy,
    ) -> Self {
        Self {
            launcher,
            default_call_limits: CallLimits::default(),
            default_vm_ceiling: VmCeiling::default(),
            default_network_policy: network_policy,
            rootfs_version: RootfsVersion::current(),
        }
    }

    fn spec_for(&self, ctx: &ToolContext) -> VmSpec {
        VmSpec {
            host_cwd: ctx.cwd.clone(),
            host_cwd_writable: true,
            env: Default::default(),
            network_policy: self.default_network_policy.clone(),
            vm_ceiling: self.default_vm_ceiling,
            rootfs_version: self.rootfs_version.clone(),
        }
    }
}

#[async_trait]
impl SandboxProvider for MicroVmProvider {
    fn name(&self) -> &'static str {
        "microvm"
    }

    async fn execute_tool(
        &self,
        ctx: &ToolContext,
        tool_name: &str,
        tool_input: &serde_json::Value,
    ) -> Result<SandboxExecution, SandboxError> {
        // Acquire (warm-pool hit ≈ µs; cold-boot ≈ 1 s on
        // Linux/Firecracker per the warm-bench).
        let spec = self.spec_for(ctx);
        let vm = self.launcher.acquire(&spec).await?;

        // Execute the single tool call in the guest. The launcher
        // owns the vsock RPC; the result lands as a `VmExecution`.
        let exec_result = vm
            .execute(ctx, &self.default_call_limits, tool_name, tool_input)
            .await;

        // Always release. v1 launcher policy: warm-pool return on
        // success, destroy on Err. Until the v0.41 ReleaseOutcome
        // contract lands, we surface release errors as a tracing
        // warn (release is best-effort cleanup).
        let release_result = vm.release().await;
        if let Err(e) = release_result {
            tracing::warn!(error = %e, "MicroVmProvider: release failed");
        }

        let exec = exec_result?;
        let ToolResult {
            tool_use_id: _,
            model_output,
            display: _,
            is_error,
        } = exec.result;

        Ok(SandboxExecution {
            stdout: model_output,
            stderr: String::new(), // raw stderr not surfaced separately; v0.41 fixes this
            exit_status: if is_error { 1 } else { 0 },
        })
    }
}

#[cfg(test)]
mod tests {
    // Real end-to-end coverage lives in
    // `crates/pi-sandbox/tests/firecracker_smoke.rs` and
    // `firecracker_workload.rs`. This module is here as a hook for
    // future unit-test additions (e.g. host-side disposition
    // dispatch when the v0.41 trait reshape lands).
}
