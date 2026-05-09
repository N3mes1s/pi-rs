//! `pi-sandbox` — isolation boundary abstraction for tool execution.
//!
//! The `SandboxProvider` trait lets tool invocations cross a boundary
//! (local subprocess, container, VM, remote service) rather than executing
//! inline in the agent process. Implementations handle the actual execution
//! and return a result tuple (stdout, stderr, exit_status) that the agent
//! converts back into a standard `ToolResult`.

pub mod cache;
#[cfg(target_os = "linux")]
pub mod contextfs;
pub mod local;
pub mod microvm;
pub mod provider;
pub mod remote;

pub use cache::RootfsCache;
pub use local::LocalProcessProvider;
pub use microvm::{
    CallLimits, MicroVmLauncher, MicroVmProvider, NetworkPolicy, ProbeCheck, ProbeReport,
    RootfsVersion, VmCeiling, VmExecution, VmHandle, VmSpec,
};
pub use provider::{SandboxError, SandboxExecution, SandboxProvider};
pub use remote::e2b::E2bProvider;
pub use remote::sprites::SpritesProvider;
