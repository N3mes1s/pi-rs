# RFD 0023 — Local MicroVM Sandbox (Linux/macOS/Windows)

- **Status:** Discussion (v0.24)
- **Author:** pi-rs maintainers
- **Created:** 2026-05-02
- **Implemented:** (pending)

## Summary

RFD 0022 shipped the `SandboxProvider` trait but only a passthrough `LocalProcessProvider` whose own docstring admits it doesn't isolate anything: `tool.invoke()` runs inline, same fs / same UID / same network. The wiring is correct, the contents are vapor. End-to-end dogfood produced `duration_ms: 0` per call — proof nothing forks.

This RFD ships the first **real isolation backend**: a `MicroVmProvider` that runs each tool call inside a Linux microVM, with one rootfs and one wire protocol shared by per-OS launchers (Firecracker on Linux, vfkit on macOS, cloud-hypervisor+WHPX on Windows). **Phased rollout:** explicit launcher pins (`--sandbox-provider=microvm:firecracker`, `…:vfkit`, `…:cloud-hypervisor`) ship per platform as each launcher lands and dogfoods clean. The unqualified `--sandbox-provider=microvm` *auto-pick* form ships only after all three OS paths meet the cross-platform GA bar (probe, dogfood, parity tests green on each).

Remote sandbox vendors (E2B, Sprites, Daytona) are split into a sister RFD — see **RFD 0026** — because they share only the `SandboxProvider` surface, none of the rootfs/protocol/launcher infrastructure.

### What's load-bearing

1. **One Linux guest rootfs** (alpine, ~50–80 MB compressed) hosting `pi-sandbox-worker`. Same artifact on every host OS.
2. **One vsock JSON-line wire protocol**, version-negotiated.
3. **Three `MicroVmLauncher` impls** under one trait, each `#[cfg]`-gated to its OS.
4. **Read–write workspace mount** of the user's cwd as `/work` inside the guest. Read-only is **not** acceptable; the agent's two highest-frequency tools (`write`, `edit`) require host fs mutation. v1.0 ships writable. On Linux/Firecracker Stage 1 the mount is **contextfs-mediated** (§3.5: in-guest FUSE backed by `cfs-fs-server` over a Noise-IK channel). On macOS/Windows v1.0 the mount is **virtio-fs RW direct** (no contextfs); a follow-up RFD brings contextfs to those platforms once contextfs ships a non-vsock embedder transport.
5. **Per-launcher pooling** required for `FirecrackerLauncher` in v1.0 (a real coding turn calls 20+ tools; cold-boot-per-call burns 5–10s of perceived "sandbox tax" per turn, sending users back to `local-process`). Other launchers may no-op pooling initially.

### What's deliberately deferred (with reasons)

- **Remote backends** — RFD 0026.
- **Snapshot / restore** beyond simple pooling — v1.0 keeps a warm pool of N pre-booted VMs; snapshot/restore is a v1.x optimization.
- **Per-tool network policy** — guests have no network in v1.0. A future RFD adds selective egress.
- **Custom rootfs per tool** — one rootfs serves all tools in v1.0.

## Background

### What RFD 0022 left vapor

RFD 0022's `LocalProcessProvider` (in `crates/pi-sandbox/src/local.rs`)
deferred tmpdir isolation, sub-process spawning, and resource caps to
"a future commit." That commit never landed. Today every "sandboxed"
tool call under `--sandbox-provider=local-process` is still a function
call in the agent's own address space. The substance — process or VM
boundary — is what RFD 0023 ships.

### The pi-tools dependency problem (the silent killer)

A guest worker that runs pi's tools cannot link `pi-ai` (the LLM-provider crate). It also cannot link `reqwest` (network) or `tokio::net::TcpListener` etc. — the guest has no network, no LLM creds, no DNS. But every file in `crates/pi-tools/src/*.rs` today does:

```rust
use pi_ai::{ToolResult, ToolSpec};
```

Audit of `pi-tools/Cargo.toml` shows `pi-ai.workspace = true` and `reqwest.workspace = true` are unconditional. The host build pulls in the whole world.

**Resolution (Commits A1/A2 — both shipped):** A1 extracts the POD types `ToolResult` / `ToolSpec` / `ToolError` into a tiny `pi-tool-types` crate (deps: `serde`/`serde_json`/`thiserror` only); `pi-ai` re-exports them for back-compat. A2 splits `pi-tools` into `pi-tools-core` (the guest-safe file/process tools — `read`/`write`/`edit`/`bash`/`grep`/`find`/`ls`/`monitor` all live here today; `monitor` is in the source tree but is **not registered** in the guest worker's tool dispatcher under v1 because the one-shot RPC can't carry its streaming output) and `pi-tools-net` (web_search), with `pi-tools` itself becoming a re-export façade. **As of 2026-05-04 both A1 and A2 are merged on `main`** (`crates/pi-tools/Cargo.toml` already pulls `pi-tools-core` + `pi-tools-net`). The remaining A-series work for guest-side completeness: extracting `ToolContext` / a `Tool` impl that doesn't transitively pull `pi-ai` (some shared trait machinery still lives in `pi-tools` re-exporting `pi-ai` types). The guest worker depends on `pi-tool-types` + `pi-tools-core` + `pi-sandbox-protocol`. Compiles statically against musl, links into a ~6–8 MB binary, fits in alpine.

Estimated impact: ~600 LoC moved, no behavior change. Fully reversible.

**`pi-tool-types` becomes a stable public ABI** by virtue of being on the wire protocol AND in the host-side tool API. Field additions to `ToolResult`/`ToolSpec` after Commit A1 are breaking changes that must bump the crate's MAJOR version and the wire-protocol version in lockstep. Acknowledged here so future-us doesn't blunder.

### Tool dispatch boundary — `ToolDispatchClass` (NEW API, Commit G)

Today (`crates/pi-agent-core/src/runtime.rs:1677-1681, 1790-1803`)
the runtime sends every tool through `SandboxProvider::execute_tool`
when a provider is configured; there is no first-class notion of
"runtime-native vs sandbox-managed". `task` is just another tool
(`crates/pi-coding-agent/src/native/task/tool.rs`). That works for
the inline `LocalProcessProvider` path because the provider is a
function call, but it would break under microvm — `task` would try
to spawn a subagent inside the guest, which has no agent loop.

Commit G adds a first-class `ToolDispatchClass` to the tool registry
metadata:

```rust
// `ToolDispatchClass` is a small POD enum and CAN live in
// `pi-tool-types` (next to `ToolResult` / `ToolSpec`); it's
// just data with no behavior. The corresponding `dispatch_class()`
// method lives on the existing `Tool` trait in `pi-tools`
// (the trait crate that already exposes `Tool::name()` /
// `Tool::invoke()`). pi-tool-types stays POD-only; pi-tools
// gains one trait method.

// pi-tool-types/src/lib.rs:
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ToolDispatchClass {
    /// Runtime-owned: bypasses SandboxProvider entirely. Examples:
    /// `task` (RFD 0005), future `apply_plan` / `evolve_tick`.
    RuntimeNative,
    /// Sandbox-managed: routed through the active SandboxProvider.
    /// SandboxProvider further partitions these into `guest` /
    /// `host-direct` / `unavailable` (the §"Tool availability under
    /// microvm" matrix).
    SandboxManaged,
}

// pi-tools/src/lib.rs (existing trait — gains one method):
pub trait Tool {
    // ... existing fields: name(), schema(), invoke(), etc. ...

    /// Default is `SandboxManaged` because every existing tool is
    /// sandbox-managed today. Runtime-native tools (currently just
    /// `task`) override.
    fn dispatch_class(&self) -> pi_tool_types::ToolDispatchClass {
        pi_tool_types::ToolDispatchClass::SandboxManaged
    }
}
```

The runtime's tool dispatch site (`runtime.rs:1677` today) becomes:

```rust
match tool.dispatch_class() {
    ToolDispatchClass::RuntimeNative =>
        tool.invoke(ctx, input).await,    // bypass sandbox
    ToolDispatchClass::SandboxManaged =>
        match &self.sandbox_provider {
            Some(p) => p.execute_tool(ctx, &call_id, name, input).await,
            None    => tool.invoke(ctx, input).await,
        },
}
```

`task` overrides `dispatch_class()` to return `RuntimeNative`. Future
runtime-native tools do the same. `MicroVmProvider::execute_tool`
adds a `debug_assert!` that the tool isn't `RuntimeNative` — a
defense-in-depth check; reaching that branch would be a runtime
bug.

This is a **NEW API** Commit G ships; it does not exist in the
codebase today. The same change benefits `LocalProcessProvider`
(which currently fakes `task` execution by relying on the in-process
side effects). The RFD 0022 trait gains `tool_use_id` (§3 mapping
table); the tool trait gains `dispatch_class()`. Both are non-default
trait changes that ripple through provider impls.

The §"Tool availability under `microvm`" matrix below covers only
the `SandboxManaged` slice of the tool surface: the `pi-tools-core`
guest tools, `web_search` (host-direct), `monitor` (unavailable),
`lsp` (unavailable in v1). `task` is `RuntimeNative` and never
appears in that matrix.

#### Plan-time advertisement — `SandboxProvider::tool_disposition()`

Runtime-only rejection isn't enough. The model plans against the
advertised tool list at conversation start; if `monitor` and `lsp`
appear there, a model under microvm will plan to use them and fail
at execution. Worse, RFD 0005 subagents like `code-reviewer` and
`halo-implementer` are configured with explicit tool allowlists; if
their allowlist includes `lsp` and the operator runs them under
microvm, they fail mid-task in a confusing way.

Commit G adds a **plan-time capability-query API**:

```rust
// New on the SandboxProvider trait (RFD 0022 amendment):
pub enum SandboxToolDisposition {
    Guest,        // routes through provider's guest path
    HostDirect,   // routes through provider's host-direct path
    Unavailable,  // model should not see this tool advertised
}

trait SandboxProvider {
    /// Plan-time capability query: which tools does this provider
    /// support? **The default is `Unavailable`** — a safe default
    /// that prevents the "model planned against a lie" class of
    /// bug for newly added tools that no provider has been updated
    /// to handle. Every concrete provider MUST override and
    /// classify its supported tools explicitly:
    ///
    ///   - `LocalProcessProvider::tool_disposition()` returns
    ///     `Guest` if the tool exists in its `ToolRegistry` (the
    ///     `with_defaults`/`with_readonly_defaults`/etc.
    ///     constructors gate which tools count), else
    ///     `Unavailable`.
    ///   - `MicroVmProvider::tool_disposition()` uses an explicit
    ///     match: `Guest` for `read`/`write`/`edit`/`bash`/
    ///     `grep`/`find`/`ls`; `HostDirect` for `web_search`;
    ///     `Unavailable` for everything else (including future
    ///     tools that haven't been classified yet).
    ///
    /// Adding a new tool to pi-rs that should be reachable under
    /// `microvm` is therefore a two-touch change: register it in
    /// the relevant tool registry AND extend
    /// `MicroVmProvider::tool_disposition()`. The compiler doesn't
    /// enforce this (the trait method is a free function), but the
    /// safe-default + matrix-test below catches drift.
    fn tool_disposition(&self, tool_name: &str) -> SandboxToolDisposition {
        SandboxToolDisposition::Unavailable
    }

    // ... existing execute_tool, etc. ...
}
```

Runtime startup (in `pi-agent-core` `RuntimeConfig::build()`) filters
the advertised tool list:

```rust
let advertised_tools: Vec<&Tool> = self.tools.iter()
    .filter(|t| match t.dispatch_class() {
        ToolDispatchClass::RuntimeNative => true,    // always advertised
        ToolDispatchClass::SandboxManaged => match &self.sandbox_provider {
            None    => true,                          // no provider → everything advertised
            Some(p) => !matches!(p.tool_disposition(t.name()),
                                 SandboxToolDisposition::Unavailable),
        },
    })
    .collect();
```

`task` executor behavior when an agent definition's allowlist
includes an unavailable tool: **strip-with-warning** (the v1
default). The agent runs with the filtered list, and the runtime
emits a one-line `tool_filtered_out` event to the session JSONL
(seen by `pi-stats`) plus a stderr banner on the first filter event
per session. Operators with strict-mode tenants can opt into
**fail-fast** via `[task] on_unavailable_tool = "fail-fast"` in the
campaign or settings.json — at session start, if any allowlisted
tool is `Unavailable` under the active provider, the agent aborts
with `AgentError::ToolUnavailable` before the first turn.

This is also a NEW API (Commit G ships it). The default
implementation keeps `LocalProcessProvider` behavior unchanged.

### Tool availability under `microvm` — full matrix

Every currently shipped tool that `SandboxProvider` could see, with
its v1 disposition:

| Tool                | Disposition  | Rationale                                                                 |
| ------------------- | ------------ | ------------------------------------------------------------------------- |
| `read`              | guest        | Pure file I/O on `/work`; pi-tools-core.                                  |
| `write`             | guest        | Same.                                                                     |
| `edit`              | guest        | Same.                                                                     |
| `bash`              | guest        | Process spawn — primary code-execution surface; this is what the sandbox is FOR. Real Bash in the rootfs (§6 userland note). |
| `grep`              | guest        | File traversal; busybox provides it. pi-tools-core.                       |
| `find`              | guest        | File traversal; busybox provides it. pi-tools-core.                       |
| `ls`                | guest        | Directory listing; busybox provides it. pi-tools-core.                    |
| `web_search`        | host-direct  | Network query, no host fs side effects; runs on host (§"web_search host-dispatch"). |
| `monitor`           | unavailable  | One-shot RPC vs. streaming mismatch; returns `ToolUnavailable` (§"monitor exclusion"). |
| `lsp`               | unavailable  | The current `lsp` tool lives in `crates/pi-coding-agent/src/native/lsp/tool.rs`, ABOVE `pi-sandbox` in the dependency graph — `pi-sandbox` cannot dispatch to it without a circular dep or a lower-layer rewrite. **v1 microvm marks `lsp` unavailable** (returns `ToolUnavailable` with a startup-time stderr banner if the user has LSP integration enabled). The `LspWriteTool` write-decoration path is similarly unavailable: under microvm, `write` runs guest-side without LSP post-processing. Operators who need LSP under sandbox use `--sandbox-provider=local-process`. A future RFD can re-home `lsp` into a lower crate and reclassify it as `host-direct`; that's not in Commit G's scope. |
| Future tools        | TBD per RFD  | Each new tool RFD MUST classify into one of `guest` / `host-direct` / `unavailable`. The default (if a tool RFD forgets) is `unavailable`. |

The host-direct registry in `MicroVmProvider` hardcodes **`web_search`
only** in v1 — no operator-extensible registration, no `lsp`, to keep
the trust boundary tight and the cross-crate dep graph clean.

#### `monitor` exclusion (decided)

`pi-tools::monitor` spawns a long-running observer that streams partial output to the agent. The v1 wire protocol in §3 is one `ToolRequest` → one `ToolResponse`, JSON-line-framed, single round trip. **Streaming is incompatible with the v1 protocol shape.** Two paths considered:

- **(a) Add streaming responses to v1.** Adds significant complexity to the host and worker (state machine for partial messages, EOF detection, cancellation), and `monitor` is the only consumer.
- **(b) Exclude `monitor` from `pi-tools-core`.** It stays in pi-tools but is not reachable through `microvm`. A microvm-mode session that calls `monitor` returns a clean error (`tool not available under --sandbox-provider=microvm; use --sandbox-provider=local-process or RFD 0026 remote backends`).

**Decision: (b).** Telemetry from existing pi sessions can quantify monitor usage; if it's > 5% of tool calls in real coding sessions, v1.1 of this RFD adds streaming responses. Until then, the protocol stays one-shot.

This decision is upstream of Commit A3 (the protocol crate). It must land here, not deferred.

#### `web_search` host-dispatch (decided)

`web_search` is a network-egress query — it contacts external search
engines / LLM-as-search backends and returns text. It does NOT execute
untrusted code on the host; it queries data. The microVM boundary
exists to contain *code execution* (untrusted bash, file mutation,
process spawn), not data queries.

Excluding `web_search` would lose real capability — agents lose the
ability to look up library docs, search for known issues, or fetch
external references mid-task. Three options:

(a) Hide `web_search` from the advertised tool list under `microvm`.
(b) Reject calls with `ToolUnavailable`; user must switch providers.
(c) Keep `web_search` available; **route it to a host-side dispatch
path** instead of the guest worker. The `MicroVmProvider` partitions
tools into "guest-bound" (read/write/edit/bash/grep/find/ls — anything
touching `/work` or spawning processes) and "host-bound"
(web_search — network-only, no host fs side effects). Host-bound
tools run on the host through the existing `pi-tools-net`
implementation; guest-bound tools route through the vsock RPC to the
guest worker.

**Decision: (c).** Rationale:

- The microVM's job is to contain *code execution*. `web_search`
  doesn't execute code; it returns text. Running it on the host
  doesn't widen the host's blast radius beyond what host-side tooling
  already does.
- The host already has the network credentials and provider config;
  proxying egress through the guest would require giving the guest
  net access (which IS what we are explicitly preventing).
- The agent's mental model stays consistent across providers
  (`local-process`, `microvm:local`, `microvm:managed`,
  remote-backend) — the same tool name behaves the same way from the
  model's perspective.
- Auto-approve policy (RFD 0027 H4) governs `web_search` calls
  identically regardless of provider.

Threat-model note: search-result content can include prompt-injection
material. That's a content-injection concern that the microVM does
not address either way (a model on `local-process` reads the same
results); it's an orthogonal hardening track (per-result content
filter / quarantine, separate RFD).

§2's `MicroVmProvider::execute_tool()` sample reflects this routing:
`monitor` returns `ToolUnavailable`; `web_search` dispatches to the
host registry; everything else routes through the guest. The
telemetry row gets `dispatch_path = "host-direct"` for host-bound tools
so operators can see which path each call took.

### Inspiration: aegis

`/home/nemesis/code/aegis/aegis-detonation/` ships 3486 LoC of working Firecracker integration (vsock IPC, snapshot/restore, hostd daemon). Useful as architectural reference, **not as a dependency**:

- `firecracker.rs` lines 1789–2008 — vsock client; portable in concept across launchers.
- `firecracker.rs` lines 1–1788 — Firecracker spawn / TAP / iptables / `/dev/kvm` probes — Linux-only and would need a vfkit/cloud-hypervisor analogue.
- `firecracker_hostd.rs` — supervised pool + snapshot/restore — the pattern v1.x adopts.

## Proposal

### 1. Architecture

```
pi_sandbox::SandboxProvider  (RFD 0022)
    │
    ├── LocalProcessProvider  (no isolation — kept as explicit "I trust the model")
    │
    └── MicroVmProvider  (this RFD)
            │
            └─ holds Box<dyn MicroVmLauncher>
                  │
                  ├─ FirecrackerLauncher       (#[cfg(target_os = "linux")])
                  ├─ VfkitLauncher             (#[cfg(target_os = "macos")])
                  └─ CloudHypervisorLauncher   (#[cfg(target_os = "windows")])
```

### 2. Trait signatures

These are **load-bearing for the design**; everything downstream depends on them. Commit A reviews land or fail on these.

```rust
// crates/pi-sandbox/src/microvm/launcher.rs

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::time::Duration;
use async_trait::async_trait;
use pi_tool_types::{ToolError, ToolResult};
use serde::{Deserialize, Serialize};

#[async_trait]
pub trait MicroVmLauncher: Send + Sync {
    /// Slug used in telemetry; one of "firecracker", "vfkit",
    /// "cloud-hypervisor". Stable across patch releases.
    fn transport_name(&self) -> &'static str;

    /// Probe at construction. Lets `pi sandbox doctor` produce
    /// actionable diagnostics without booting anything.
    async fn probe(&self) -> Result<ProbeReport, ProbeError>;

    /// Acquire a VM ready to execute a tool call. v1.0 launchers
    /// MAY return a pooled+warm-restored VM (FirecrackerLauncher
    /// MUST); others may cold-boot.
    async fn acquire(&self, spec: &VmSpec) -> Result<Box<dyn VmHandle>, AcquireError>;
}

// Error taxonomy. The two layers below SandboxProvider (launcher +
// VM handle) have their own error types so a typed sandbox error
// can carry the originating cause without flattening it into a
// stringy `SandboxError(String)`. SandboxProvider's `execute_tool`
// wraps both AcquireError and ExecuteError into the SandboxError
// enum that callers see; the wrapping preserves the typed
// discriminant so the runtime can route on it (retry policy for
// `BrokerMasterEpochTooOld`, alert-the-operator for
// `BrokerOidcRejected` config errors, etc.).

#[derive(Debug, thiserror::Error)]
pub enum ProbeError {
    #[error("launcher binary not found: {what}")]
    BinaryMissing { what: String },
    #[error("kernel feature missing: {feat}")]
    KernelFeatureMissing { feat: String },
    #[error("permission denied: {detail}")]
    PermissionDenied { detail: String },
    #[error("other: {0}")]
    Other(#[source] anyhow::Error),
}

#[derive(Debug, thiserror::Error)]
pub enum AcquireError {
    #[error("vm cold-boot failed: {0}")]
    BootFailed(#[source] anyhow::Error),
    #[error("guest readiness timeout after {boot_timeout_ms}ms; last dmesg:\n{last_dmesg}")]
    ReadyTimeout { boot_timeout_ms: u32, last_dmesg: String },
    #[error("host transport setup failed (vsock CID/port collision?): {detail}")]
    HostTransportSetup { detail: String },
    #[error("guest daemon ({daemon}) exited with code {exit}")]
    GuestDaemonStartFailed { daemon: String, exit: i32 },
    #[error("broker rejected with master_epoch_too_old (configured epoch={configured_epoch})")]
    BrokerMasterEpochTooOld { configured_epoch: u32 },
    #[error("broker oidc denial: {code} ({detail})")]
    BrokerOidcRejected { code: String, detail: String },
    #[error("broker tenant-mode mismatch: {detail}")]
    BrokerTenantModeMismatch { detail: String },
    #[error("pool capacity exhausted (max={max})")]
    PoolExhausted { max: u32 },
}

#[derive(Debug, thiserror::Error)]
pub enum ExecuteError {
    #[error("guest tool failed: {0}")]
    GuestToolFailed(#[source] anyhow::Error),
    #[error("vsock RPC failure: {0}")]
    Rpc(#[source] anyhow::Error),
    #[error("call exceeded ceiling: {detail}")]
    CallLimit { detail: String },
}

#[derive(Debug, thiserror::Error)]
pub enum SandboxError {
    #[error("microvm unavailable: {0}")]
    Unavailable(#[source] ProbeError),
    #[error("acquire failed")]
    Acquire(#[source] AcquireError),
    #[error("execute failed")]
    Execute(#[source] ExecuteError),
    #[error("tool {tool} unavailable: {reason}")]
    ToolUnavailable { tool: String, reason: String },
}

#[derive(Debug, Clone)]
pub struct VmSpec {
    /// Host path mounted at /work in the guest.
    pub host_cwd: PathBuf,
    /// Whether /work is writable. v1.0 is always true; we keep the
    /// flag so future per-tool policy can mount RO selectively.
    pub host_cwd_writable: bool,
    /// Environment variables forwarded into the guest. The full
    /// host env is NEVER forwarded; only an explicit allowlist.
    pub env: BTreeMap<String, String>,
    /// Network policy. v1.0 only supports `Deny`.
    pub network_policy: NetworkPolicy,
    /// Per-VM resource ceiling — the absolute cap on what the VM
    /// may consume. Per-call limits in `CallLimits` are evaluated
    /// against this; never exceed it.
    pub vm_ceiling: VmCeiling,
    /// Which rootfs version to boot. Pinned to one image per
    /// `proto_version`; mismatch refuses to boot.
    pub rootfs_version: RootfsVersion,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum NetworkPolicy {
    Deny,
    // Future: AllowList(Vec<DomainPattern>), AllowAll
}

/// `BootSpec.transport_mode` — `local` vs `managed` per the §3.5
/// deployment-mode matrix. Different modes mean different rootfs
/// init paths (managed brings up contextfsd; local doesn't), so
/// they MUST partition the warm pool.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TransportMode {
    /// virtio-fs RW direct mount (macOS/vfkit, Windows/cloud-hypervisor).
    Local,
    /// contextfs FUSE over Noise-IK (Linux/Firecracker only).
    Managed,
}

/// VM-level ceiling. Set at acquire(); cannot change without
/// rebooting the VM. One component of the pool key (see
/// `BootSpec` below).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct VmCeiling {
    pub mem_mib: u32,        // default 512 (host budget per VM)
    pub vcpus: u8,           // default 2
    pub disk_mib: u32,       // ephemeral overlay; default 256
}

/// Pool partition key. Two acquire() calls share a warm VM iff
/// their `BootSpec`s are equal — every field listed here is
/// boot-time state that cannot change without a reboot, and a
/// mismatch silently reusing a VM would breach worktree isolation
/// (RFD 0006), env-allowlist policy, or network-policy guarantees.
///
/// Per-call concerns (timeout, output cap) live on `CallLimits` and
/// vary against the same warm VM. `host_cwd` MUST be canonicalised
/// (symlink-resolved + absolute) before keying so two callers
/// pointing at the same directory by different paths share a VM.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct BootSpec {
    pub canonical_host_cwd: PathBuf,    // RFD 0006 worktree boundary
    pub env_hash: [u8; 32],             // SHA-256 of allowlisted env names+values
    pub network_policy: NetworkPolicy,
    pub vm_ceiling: VmCeiling,
    pub rootfs_version: String,         // forces cold boot on rootfs upgrade
    pub transport_mode: TransportMode,  // local | managed (§3.5)
}

/// Per-CALL limits. Evaluated against the VM's `VmCeiling` and
/// applied to the single tool execution. A long bash build can
/// raise its `wall_timeout` without forcing a fresh VM boot.
#[derive(Debug, Clone, Copy)]
pub struct CallLimits {
    pub wall_timeout: Duration,    // default 60s
    pub max_output_bytes: u32,     // default 256 KiB
}

#[async_trait]
pub trait VmHandle: Send + Sync {
    /// Send one ToolRequest, await one ToolResponse over vsock.
    /// Bridges `ToolContext` from the runtime by serialising the
    /// fields the guest needs (cwd, max_output_bytes), plus the
    /// per-call limits (wall_timeout, max_output_bytes). Per-call
    /// is the right scope for limits — a long bash build needs a
    /// generous timeout but is in the same VM as a quick `ls`
    /// the next call over.
    async fn execute(
        &self,
        ctx: &pi_tools::ToolContext,
        limits: &CallLimits,
        tool_name: &str,
        tool_input: &serde_json::Value,
    ) -> Result<VmExecution, ExecuteError>;

    /// Release the VM. v1.0 in pooled mode = return to pool;
    /// non-pooled = shutdown. Best-effort: errors are logged but
    /// do not propagate. Pool hygiene rule: VMs released after a
    /// non-trivial `ExecuteError` (e.g. tool panic, RPC failure)
    /// are **destroyed** rather than returned to the pool, to
    /// bound the leak from corrupt guest state.
    async fn release(self: Box<Self>, exec_outcome: ExecuteOutcomeHint);

    /// Synchronous best-effort teardown. Used **only** by
    /// `ReleaseGuard::Drop` when no tokio runtime is available
    /// (e.g. drop happens in a sync test, in panic-unwind, or after
    /// runtime shutdown). Default impl spawns a detached OS thread
    /// that drives a one-shot current-thread runtime just long
    /// enough to drive `release(Cancelled)` to completion or hit a
    /// 5-second timeout, whichever first. Launchers may override for
    /// faster paths (e.g. firecracker can SIGKILL its child PID
    /// directly without spinning a runtime). Must not panic.
    fn kill_blocking(self: Box<Self>) {
        std::thread::spawn(move || {
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build();
            if let Ok(rt) = rt {
                let _ = rt.block_on(tokio::time::timeout(
                    std::time::Duration::from_secs(5),
                    self.release(ExecuteOutcomeHint::Cancelled),
                ));
            }
        });
    }
}

/// Hint to `release()` so the launcher can decide pool-vs-destroy.
pub enum ExecuteOutcomeHint {
    Clean,                              // success or benign error → return to pool
    SuspectGuestState,                  // tool panic / RPC midstream → destroy
    Cancelled,                          // ctx cancellation → destroy (state unknown)
}

pub struct VmExecution {
    /// Model-facing tool result (matches the inline path's shape;
    /// `ToolResult.model_output` is what gets fed back to the model).
    pub tool_result: ToolResult,
    /// Raw stdout/stderr/exit_status from the guest worker, for
    /// telemetry and operator debugging. `tool_result.model_output`
    /// is what the model sees; `execution.stdout/stderr` are the
    /// raw streams the worker captured. They diverge whenever the
    /// worker post-processes (e.g. truncation, formatting).
    pub execution: SandboxExecution,
    pub guest_duration_ms: u32,    // measured INSIDE the guest
    /// Time from `acquire()` to the moment the host's vsock
    /// connection to the guest succeeded. NOT pure boot time —
    /// includes guest init, vsock listen, accept handshake. The
    /// host can't see "boot finished" without guest cooperation,
    /// so this is the most honest end-to-end measurement.
    pub acquire_to_ready_ms: u32,
    /// True when this acquire required a cold boot (pool miss).
    pub cold_boot: bool,
    /// Worker's post-call hygiene verdict; mirrors `ToolResponse.post_call_state`.
    pub post_call_state: PostCallState,
}

#[derive(Debug, Clone, Serialize)]
pub struct ProbeReport {
    pub launcher: &'static str,         // "firecracker" | "vfkit" | "cloud-hypervisor"
    pub available: bool,
    pub version: Option<String>,        // e.g. "Firecracker v1.15.0"
    pub probe_duration_ms: u32,
    pub blockers: Vec<String>,          // human-readable, actionable
    pub remediation: Vec<String>,       // shell commands to run
    /// Per-precondition results so doctor can show what's
    /// actually broken, not just "available=false".
    pub checks: Vec<ProbeCheck>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ProbeCheck {
    pub name: &'static str,             // "kvm_open_rw" | "vsock_module" | ...
    pub passed: bool,
    pub detail: Option<String>,
}

// (the canonical `pub enum SandboxError` is defined above, in the
// MicroVmLauncher trait block. This was previously a second
// definition with overlapping variants — collapsed in v0.17.)
```

`MicroVmProvider` is the `SandboxProvider` impl that owns one `Box<dyn MicroVmLauncher>`. The launcher owns the pool (so all calls through one `MicroVmProvider` share its pool); the provider owns the spec defaults and per-call limits derivation:

```rust
// crates/pi-sandbox/src/microvm/provider.rs

pub struct MicroVmProvider {
    /// Owns the warm pool (if the launcher chose to maintain one).
    /// Subagents inheriting this Arc<dyn SandboxProvider> SHARE the pool.
    launcher: Box<dyn MicroVmLauncher>,
    /// Default `VmSpec` used at acquire time. Overridden per-call only
    /// for fields that don't force a fresh boot (i.e. NOT VmCeiling).
    default_spec: VmSpec,
    /// Default per-call limits. Each `execute_tool()` derives a
    /// concrete `CallLimits` by overlaying ToolContext-supplied caps
    /// on these defaults.
    default_call_limits: CallLimits,
    /// Per-tool overrides. `bash` typically wants a longer timeout
    /// than `read`/`ls`. Looked up by tool name.
    per_tool_call_limits: BTreeMap<String, CallLimits>,
    /// In-process dispatcher for host-bound tools (web_search in v1).
    /// `MicroVmProvider::execute_tool` first asks
    /// `host_tools.is_host_bound(tool_name)` and short-circuits the
    /// VM acquire when true. Inherited unchanged across subagents
    /// (same Arc); the dispatcher carries provider config / API
    /// credentials for any host-side tool the operator opted into.
    /// v1 ships exactly one impl, `BuiltinHostTools`, that hardcodes
    /// `web_search` against `pi-tools-net`. Deliberately not extensible
    /// from operator config in v1 — widening the host-trust boundary
    /// before the default path is dogfooded is premature.
    host_tools: Arc<dyn HostBoundToolDispatcher>,
}

/// In-process executor for tools that bypass the guest microVM. Used
/// for tools that must execute on the host (network egress, the
/// caller's filesystem outside `<host_cwd>`, …) — currently only
/// `web_search`. Returns the same shape the guest path returns so
/// telemetry stays uniform.
///
/// **Single source of truth for host-bound classification.**
/// `MicroVmProvider::tool_disposition(name)` returns
/// `ToolDispatchClass::HostDirect` **iff** `host_tools.is_host_bound(name)`
/// returns true. The provider implementation calls
/// `host_tools.is_host_bound(name)` to derive `tool_disposition()`,
/// not a separate const list. A unit test in
/// `crates/pi-sandbox/tests/host_bound_parity.rs` round-trips every
/// pi-tools tool name through both surfaces and asserts equality;
/// drift is impossible by construction (the second surface delegates
/// to the first), and the test is a defense-in-depth check against
/// future regressions when more host-bound tools land.
#[async_trait]
pub trait HostBoundToolDispatcher: Send + Sync {
    /// Plan-time check: would `execute()` accept this tool?
    /// **Source of truth.** `tool_disposition()` defers to this.
    fn is_host_bound(&self, tool_name: &str) -> bool;

    /// Execute the tool in-process on the host. Returns
    /// `(tool_result, execution)` so the caller can build a
    /// `SandboxOutcome` with `dispatch_path: "host-direct"`. Errors
    /// surface as `SandboxError` with the same taxonomy as guest
    /// dispatch (Acquire / Execute) where applicable.
    async fn execute(
        &self,
        ctx: &pi_tools::ToolContext,
        tool_name: &str,
        tool_input: &serde_json::Value,
    ) -> Result<HostExecOutcome, SandboxError>;
}

pub struct HostExecOutcome {
    pub tool_result: ToolResult,
    pub execution: SandboxExecution,
}

impl MicroVmProvider {
    /// Build a CallLimits for one tool invocation. Overlays per-tool
    /// override on default; `ctx.max_output_bytes` clamps the cap.
    fn build_call_limits(&self, ctx: &ToolContext, tool_name: &str) -> CallLimits {
        let base = self
            .per_tool_call_limits
            .get(tool_name)
            .copied()
            .unwrap_or(self.default_call_limits);
        CallLimits {
            wall_timeout: base.wall_timeout,
            max_output_bytes: base.max_output_bytes.min(ctx.max_output_bytes as u32),
        }
    }

    fn spec_for(&self, ctx: &ToolContext) -> VmSpec {
        let mut s = self.default_spec.clone();
        s.host_cwd = ctx.cwd.clone();
        s
    }
}

#[async_trait]
impl SandboxProvider for MicroVmProvider {
    fn name(&self) -> &'static str { "microvm" }

    async fn execute_tool(
        &self,
        ctx: &ToolContext,
        tool_use_id: &str,                   // NEW: threaded from outer ToolCall.call_id (RFD 0022 trait amendment)
        tool_name: &str,
        tool_input: &serde_json::Value,
    ) -> Result<SandboxOutcome, SandboxError> {
        // Runtime-native orchestration tools (task / subagent — anything
        // that drives the agent's outer loop, per RFD 0005) bypass
        // SandboxProvider entirely BEFORE we get here. The runtime
        // dispatcher checks `tool.dispatch_class()` first and never
        // calls SandboxProvider::execute_tool for `RuntimeNative`
        // tools (task/subagent/etc.). We assert it as a defense-in-
        // depth invariant; reaching this branch is a runtime bug,
        // not a sandbox concern.
        debug_assert!(tool_dispatch_class(tool_name) != ToolDispatchClass::RuntimeNative,
            "RuntimeNative tool {tool_name} should never reach SandboxProvider");
        // monitor: incompatible with one-shot RPC, see §"Tool availability".
        if tool_name == "monitor" {
            return Err(SandboxError::ToolUnavailable {
                tool: tool_name.into(),
                reason: "monitor is unavailable under --sandbox-provider=microvm \
                         (one-shot RPC; use --sandbox-provider=local-process)",
            });
        }
        // web_search: network-egress query, runs on the host (the guest
        // has no network by design). See §"web_search host-dispatch".
        if self.host_tools.is_host_bound(tool_name) {
            let started = Instant::now();
            let host_outcome = self.host_tools.execute(ctx, tool_name, tool_input).await?;
            // host_outcome is itself a (ToolResult, SandboxExecution) pair —
            // pi-tools-net's executor returns both.
            return Ok(SandboxOutcome {
                tool_result: host_outcome.tool_result,
                execution: host_outcome.execution,
                telemetry: SandboxTelemetry {
                    provider: "microvm",
                    launcher: None,                   // no VM involved
                    dispatch_path: Some("host-direct"),
                    acquire_to_ready_ms: None,
                    guest_duration_ms: Some(started.elapsed().as_millis() as u32),
                    cold_boot: None,
                    cost_usd: None,
                },
            });
        }
        // Everything else routes through the guest microVM.
        let spec = self.spec_for(ctx);
        let limits = self.build_call_limits(ctx, tool_name);
        let vm = self.launcher.acquire(&spec).await
            .map_err(SandboxError::Acquire)?;
        // Cancel-safe release. Wrap the VM in a guard whose Drop
        // releases with `Cancelled` if the future is dropped between
        // acquire and explicit release. A normal completion calls
        // `guard.release(hint)` which disarms the Drop. A panic in
        // `vm.execute` unwinds through the guard and releases as
        // `SuspectGuestState`. Cancellation (parent task drop)
        // releases as `Cancelled` so the launcher can choose its own
        // policy (today: destroy, since we cannot prove guest state).
        let mut guard = ReleaseGuard::new(vm);
        let exec_result = guard.vm_mut().execute(ctx, &limits, tool_name, tool_input).await;
        // The host's tentative hint. The wire-borne
        // `post_call_state` from the worker (set after the per-call
        // hygiene probe — see §"Post-call hygiene") composes with
        // this hint to produce the final value passed to release().
        // Successful tool execution by itself is NOT proof the VM is
        // reusable: a `bash 'sleep 999 &'` leaves a daemon alive
        // after `tool.invoke()` returns; only the worker's probe
        // can detect that.
        let host_hint = match &exec_result {
            Ok(_) => ExecuteOutcomeHint::Clean,
            Err(ExecuteError::CallLimit { .. }) => ExecuteOutcomeHint::Clean,
            Err(_) => ExecuteOutcomeHint::SuspectGuestState,
        };
        let post_call = exec_result
            .as_ref()
            .map(|exec| exec.post_call_state)
            .unwrap_or(PostCallState::SuspectGuestState);
        let outcome_hint = compose_outcome(host_hint, post_call);
        // compose_outcome: Clean iff BOTH inputs are Clean; otherwise
        // SuspectGuestState. Cancelled propagates separately via
        // ReleaseGuard::Drop.
        guard.release(outcome_hint).await;  // disarms Drop; awaits the actual release
        let exec = exec_result.map_err(SandboxError::Execute)?;
        Ok(SandboxOutcome {
            tool_result: exec.tool_result,
            execution: exec.execution,        // raw stdout/stderr/exit_status, separate from model_output
            telemetry: SandboxTelemetry {
                provider: "microvm",
                launcher: Some(self.launcher_name()),  // "firecracker" | "vfkit" | "cloud-hypervisor"
                dispatch_path: Some("guest"),
                acquire_to_ready_ms: Some(exec.acquire_to_ready_ms),
                guest_duration_ms: Some(exec.guest_duration_ms),
                cold_boot: Some(exec.cold_boot),
                cost_usd: None,
            },
        })
    }
}

/// Cancel-safe wrapper that ensures `release` always fires.
///
/// `ReleaseGuard` owns the `VmHandle` between acquire and execute. The
/// happy path calls `guard.release(hint).await` which disarms the
/// Drop; the cancel path (future dropped) hits Drop, which spawns a
/// detached release task with `ExecuteOutcomeHint::Cancelled`. The
/// detached task runs on the launcher's runtime so Drop itself stays
/// synchronous (Rust requirement).
pub struct ReleaseGuard {
    vm: Option<Box<dyn VmHandle>>,
}

impl ReleaseGuard {
    pub fn new(vm: Box<dyn VmHandle>) -> Self { Self { vm: Some(vm) } }
    pub fn vm_mut(&mut self) -> &mut dyn VmHandle { self.vm.as_mut().unwrap().as_mut() }
    pub async fn release(mut self, hint: ExecuteOutcomeHint) {
        if let Some(vm) = self.vm.take() {
            vm.release(hint).await;
        }
    }
}
impl Drop for ReleaseGuard {
    fn drop(&mut self) {
        if let Some(vm) = self.vm.take() {
            // Cannot await in Drop. Two failure modes to consider:
            //
            // 1. No current tokio runtime (Drop running outside an async
            //    context). `tokio::runtime::Handle::try_current()` lets us
            //    detect this; on Err we fall through to a synchronous
            //    best-effort cleanup. The VmHandle's launcher exposes
            //    `kill_blocking()` for this case (default impl: spawn a
            //    detached thread that drives a one-shot Tokio runtime
            //    just long enough to fire `release(Cancelled)` — bounded
            //    because release is non-await on the happy path and
            //    timeout-bounded on the slow path).
            //
            // 2. Runtime is shutting down. `tokio::spawn` may panic or
            //    drop the future immediately. We tolerate this: at
            //    process shutdown the launcher's pool is being torn
            //    down anyway, and any leaked VM is reaped by the
            //    process-exit hook (Linux: `prctl(PR_SET_PDEATHSIG)`
            //    on the firecracker child; macOS/Windows: process
            //    group teardown). A leak window of milliseconds at
            //    shutdown is acceptable.
            match tokio::runtime::Handle::try_current() {
                Ok(_) => {
                    tokio::spawn(async move { vm.release(ExecuteOutcomeHint::Cancelled).await; });
                }
                Err(_) => {
                    // Synchronous fallback. Panics here would be
                    // worse than a brief leak, so swallow.
                    let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                        vm.kill_blocking();
                    }));
                }
            }
        }
    }
}

// SandboxOutcome now carries BOTH the model-facing tool result AND the
// raw execution streams. RFD 0022's existing SandboxExecution is the
// raw shape; ToolResult is what feeds back to the model. They differ
// when the worker post-processes (truncation, header injection, etc.).
pub struct SandboxOutcome {
    pub tool_result: ToolResult,        // model-facing; what the agent sees
    pub execution: SandboxExecution,    // raw stdout/stderr/exit_status, for telemetry/debugging
    pub telemetry: SandboxTelemetry,
}

// `host_tools` is the in-process dispatcher for host-bound tools.
// In v1 it hardcodes `web_search` as the only host-bound tool —
// no operator-extensible registry, since making host-direct
// extensible in the first release would widen the trust boundary
// before the default path is proven. v1.1 may revisit if telemetry
// shows demand. Guest-bound tools (read/write/edit/bash/grep/find/
// ls) always route through the launcher.

// SandboxProvider's trait return is unified: every provider — local-
// process, microvm, remote backends — returns the SAME SandboxOutcome
// shape. LocalProcessProvider's existing inline-execution body wraps
// its `SandboxExecution` into the new envelope (one wrapping helper,
// no associated-type machinery). This is a one-time non-breaking
// expansion of RFD 0022's trait return.

pub struct SandboxTelemetry {
    /// Which `SandboxProvider` impl handled this call.
    pub provider: &'static str,          // "microvm" | "local-process" | "e2b" | "sprites" | "daytona"
    /// For microvm only: which launcher backend booted the VM.
    pub launcher: Option<&'static str>,  // "firecracker" | "vfkit" | "cloud-hypervisor"
    /// For microvm only: which leg of the partition handled the
    /// tool — guest microVM vs the host-bound dispatch path
    /// (web_search, future host-bound tools per §"Tool availability").
    pub dispatch_path: Option<&'static str>, // "guest" | "host-direct"
    pub acquire_to_ready_ms: Option<u32>,// None for local-process or host-direct, Some for microvm-guest/remote
    pub guest_duration_ms: Option<u32>,
    pub cold_boot: Option<bool>,
    pub cost_usd: Option<f64>,           // remote-backend per-call billing
}
```

The pool ownership rule is **the launcher owns the pool**. This means:

- One `MicroVmProvider` instance ↔ one launcher instance ↔ one pool.
- Subagents that inherit the parent's `Arc<dyn SandboxProvider>` (via `RuntimeConfig.sandbox_provider`) **share that pool**. The pool is normatively keyed by `BootSpec` — implementation is equivalent to `tokio::sync::Mutex<HashMap<BootSpec, VecDeque<WarmVm>>>`. Acquire looks up the warm-VM ring for the requesting `BootSpec`; release puts it back into the same ring. Two subagents with different `BootSpec`s (different `host_cwd` per RFD 0006 worktree, different `env_hash`, etc.) NEVER share a warm VM. If the matching ring is empty, the launcher cold-boots an ad-hoc VM for that `BootSpec`.
- A user who wants **per-subagent pool isolation** must construct a fresh `MicroVmProvider` for each subagent runtime — explicit, not implicit. Halo's RFD 0025 supervisor will configure this; documented in the halo integration notes.

This was previously misstated in v0.2's Open Question #2 ("each subagent's runtime gets its own MicroVmProvider"); v0.3 corrects it.

Telemetry rows extend the existing `SessionEntryKind::SandboxAction` from RFD 0022. The schema decision is **one union struct with all-optional new fields** (rather than splitting into `Local`/`Remote` variants), because it lets `pi-stats::aggregate::by_sandbox_provider()` keep its current rollup shape without per-variant code paths, and because all "new" fields are independently meaningful (a local pool-miss telemetry row has a `cold_boot` but no `cost_usd`; a remote E2B row has the inverse).

```rust
SandboxAction {
    provider: String,           // "microvm" | "local-process" | "e2b" | "sprites" | "daytona"
    tool_name: String,
    duration_ms: u64,           // total host-observed; sum of acquire + guest (or just elapsed for host-direct)
    exit_status: i32,
    is_error: bool,
    // NEW (this RFD — local microVM, three-field split per v0.9):
    #[serde(default, skip_serializing_if = "Option::is_none")]
    launcher: Option<String>,   // "firecracker" | "vfkit" | "cloud-hypervisor" (microvm-guest only)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    dispatch_path: Option<String>, // "guest" | "host-direct" (microvm only)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    acquire_to_ready_ms: Option<u32>,  // host-observed time-to-first-byte (None for host-direct / local-process)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    cold_boot: Option<bool>,
    // NEW (RFD 0026 — remote):
    #[serde(default, skip_serializing_if = "Option::is_none")]
    cost_usd: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    round_trip_ms: Option<u32>,
}
```

The new fields are added as an **amendment to RFD 0022** (which is currently marked Implemented v1.0 — adding optional fields is non-breaking; existing telemetry rows deserialize fine because of `#[serde(default)]`). RFD 0022's revision history will be appended with an `(amended by RFDs 0023 + 0026)` note when those RFDs land. `pi-stats::ingest` adds the following nullable columns to the `sandbox_actions` SQLite table — one per new struct field: `launcher TEXT`, `dispatch_path TEXT`, `acquire_to_ready_ms INTEGER`, `guest_duration_ms INTEGER`, `cold_boot INTEGER`, `cost_usd REAL`, `round_trip_ms INTEGER`.

**JSONL backward compatibility** — pre-amendment rows lack the new fields; `#[serde(default)]` makes them deserialize as `None`, and `pi-stats::ingest` writes `NULL` to the new columns. **Migration ordering**: schema migration (`ALTER TABLE sandbox_actions ADD COLUMN ...` × 7 new) MUST run before any pi binary speaking the new fields ingests rows; otherwise the binary will refuse to start with `schema_migration_required`. v1 ships an integration test (`tests/sandbox_action_compat.rs`) that loads a fixture of pre-amendment rows and asserts both the old binary's rows are still readable AND the new binary's rows have the new fields populated. The `provider` field already exists on the struct in RFD 0022 and is not new here.

### 3. The local microVM contract

#### Guest rootfs (one artifact, every host)

- alpine 3.19+ minirootfs as the base (~6 MB).
- `pi-sandbox-worker` binary (statically linked against musl, ~6–8 MB) at `/usr/local/bin/pi-sandbox-worker`.
- An init script at `/init` that:
  1. mounts `/proc`, `/sys`, `/dev/vsock`. `/work` is mounted by `pi-cfs-init` as a contextfs FUSE filesystem on Linux Stage 1 (§3.5.9), or by the rootfs init script as a virtio-fs share on macOS/Windows v1.0.
  2. parses `/proc/cmdline` for `pi.proto_version=N`; if mismatch, prints a fatal diagnostic to the serial console and halts.
  3. execs `pi-sandbox-worker --vsock-port=5001`.
- Versioned: `pi-sandbox-rootfs-vMAJOR.MINOR.PATCH.img.zst`. SHA256 published alongside the artifact.
- Distributed as a CI release asset; downloaded on first sandbox use to `~/.cache/pi/sandbox/rootfs/<version>/rootfs.img`. Auto-upgraded in lockstep with the pi binary (the binary has the expected rootfs version baked in at build time; on mismatch with the cache, it auto-downloads with a progress bar and SHA256 verification).
- Hackers can rebuild from `crates/pi-sandbox-rootfs/build.sh` (alpine miniroot tarball + busybox + cargo-built worker). `PI_SANDBOX_ROOTFS=/path` env override for offline use.

#### Wire protocol

```rust
// crates/pi-sandbox-protocol/src/lib.rs
pub const CURRENT_PROTOCOL_VERSION: u32 = 1;
pub const VSOCK_DEFAULT_PORT: u32 = 5001;

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ToolRequest {
    pub proto_version: u32,
    pub call_id: String,
    pub tool_name: String,
    pub tool_input: serde_json::Value,
    pub max_output_bytes: u32,
    pub timeout_ms: u32,
    /// Host-side cwd this call originated from (already canonicalised
    /// by `MicroVmProvider`). The worker keeps the value opaque, with
    /// two narrow uses:
    ///   1. Inject `PWD=<host_cwd>` into bash's environment so shell
    ///      builtins (`pwd`, `echo $PWD`) print the host path —
    ///      best-effort per the path-virtualization contract;
    ///      `pwd -P`, `realpath .`, `readlink /proc/self/cwd` still
    ///      see `/work`.
    ///   2. Provide the inverse-rewrite anchor when the worker emits
    ///      `model_output` / `display`: literal `/work` prefixes are
    ///      rewritten back to `host_cwd` BEFORE the response is sent,
    ///      so the host receives already-natural paths and only has
    ///      to rewrite the JSON keys it doesn't trust the worker to
    ///      handle.
    /// The host re-validates inverse-rewrites on receipt — guest
    /// output is not trusted to be rewriting-correct. **Failure
    /// policy:** if the host finds a `/work/...` substring in
    /// `model_output` after the guest claimed to have rewritten it,
    /// the host applies its own rewrite (the registry is the
    /// authority) and logs a `path_rewrite_drift` warning to
    /// telemetry. Same for `display`. The host does NOT reject the
    /// response — that would lose tool work — but the drift
    /// counter is a CI alert: any nonzero value indicates a
    /// worker/host mismatch that needs reconciling. Conversely, a
    /// `display` field that the host's registry doesn't recognize
    /// (unknown JSON shape) is passed through verbatim with a
    /// `path_rewrite_unknown_shape` counter bump; the host never
    /// fabricates a rewrite for a shape it doesn't understand.
    pub host_cwd: String,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ToolResponse {
    pub call_id: String,
    /// Post-processed text destined for `ToolResult.model_output`.
    /// The guest worker has already invoked the native pi-tools-core
    /// tool, so this is what `tool.invoke()` returned for the current
    /// process model — e.g. for `bash`, the worker emits the existing
    /// `stdout + stderr + [exit N]` model-formatted string here. The
    /// host then only path-rewrites this value; it does NOT re-derive
    /// it from raw stdout/stderr.
    pub model_output: String,
    /// Raw stdout/stderr/exit_status, kept for telemetry and debugging
    /// only. Surfaces on `SandboxOutcome.execution`, not on the
    /// model-facing `ToolResult`. May equal `model_output` for
    /// pass-through tools (read/write/edit/etc.) and differ for
    /// tools that post-process (bash today).
    pub stdout: String,
    pub stderr: String,
    pub exit_status: i32,
    pub guest_duration_ms: u32,
    pub is_error: bool,
    /// Mirrors `ToolResult.display`. The worker copies the native
    /// tool's `display` value here verbatim; the host inverse-rewrites
    /// embedded path strings before forwarding (per the path-key
    /// registry below).
    #[serde(default)]
    pub display: Option<serde_json::Value>,
    /// Worker's post-call hygiene verdict. Set by the worker AFTER
    /// the tool returns and AFTER the worker has run the per-call
    /// process-subtree probe (cgroup-empty on Linux; pgrp-orphan
    /// fallback elsewhere) + temp-dir scrub described in §"Post-call
    /// hygiene". `Clean` means the VM is provably idle and may be
    /// returned to the pool. `SuspectGuestState` means at least one
    /// post-call check failed (lingering descendant, residual temp
    /// state, …) — the host MUST destroy this VM regardless of the
    /// tool's success bit. The host's `outcome_hint` derivation
    /// composes WITH this field: `final = min(host_hint,
    /// post_call_state)` where `Clean < SuspectGuestState`. Defaults
    /// to `SuspectGuestState` on a missing field — workers are
    /// trusted to *prove* cleanliness, not to skip the field.
    #[serde(default = "default_post_call_state")]
    pub post_call_state: PostCallState,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum PostCallState { Clean, SuspectGuestState }

fn default_post_call_state() -> PostCallState { PostCallState::SuspectGuestState }
```

One JSON line per direction, `\n`-framed. Carried over a vsock connection on `VSOCK_DEFAULT_PORT`. **Guest-initiated**: the guest worker listens on the vsock port; the host's `acquire()` blocks until the guest signals "ready" by accepting a connection. This dodges known macOS host-listen quirks under vfkit and matches aegis's working pattern under Firecracker.

#### `ToolResponse` ↔ `ToolResult` field mapping

The host needs to reconstruct a `pi_tool_types::ToolResult` from the wire `ToolResponse` so the rest of the agent loop sees a uniform shape regardless of sandbox. `ToolResult` on `main` (`crates/pi-tool-types/src/lib.rs:21-31`, post-A1):

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolResult {
    pub tool_use_id: String,
    pub model_output: String,                 // text fed back to the LLM
    #[serde(default)]
    pub display: Option<serde_json::Value>,   // structured UI hint, None when no hint
    #[serde(default)]
    pub is_error: bool,
}
```

`display` is `Option<serde_json::Value>` — host-side tools like `task` populate it for child-session pointers; **most pi-tools-core tools also populate `display`** (verified at `crates/pi-tools-core/src/{read,write,edit,grep,find,ls,bash,monitor}.rs`). The wire shape above mirrors this with an explicit `display` field.

**Why the wire carries both `model_output` and raw `stdout/stderr` instead of recomputing one from the other:** today's `bash` tool builds `model_output = "<stdout>\n<stderr>\n[exit N]"` inside `pi-tools-core/src/bash.rs`. If the wire carried only raw streams the host couldn't reproduce that without re-implementing bash's formatting (or every tool's). Conversely, dropping raw stdout/stderr destroys the telemetry value of `SandboxOutcome.execution`. Carrying both fields is one extra string in the JSONL (gzipped on persistence) for full parity.

Mapping rules (host-side, in `MicroVmProvider::execute_tool`):

| `ToolResult` field | Sourced from                                       | Note |
|--------------------|----------------------------------------------------|------|
| `tool_use_id`      | Threaded **explicitly** as a parameter to `SandboxProvider::execute_tool(ctx, tool_use_id, tool_name, tool_input)` from the runtime. The runtime passes the outer `ToolCall.call_id` (the LLM-facing id) here; the provider stores it on the resulting `ToolResult` so the LLM can correlate. NOT the wire `ToolRequest.call_id` (which is host-allocated for guest-side dedup and lives only on the wire). | RFD 0022's trait gains a `tool_use_id: &str` parameter on `execute_tool` as a non-default change; LocalProcessProvider, MicroVmProvider, and remote-backend providers all accept it. |
| `model_output`     | `ToolResponse.model_output` after inverse path rewrite (`/work` → `<host_cwd>`) | The guest worker has already done the native tool's post-processing (e.g. bash's stdout+stderr+[exit N] formatting); the host only rewrites paths. |
| `display`          | `ToolResponse.display` after inverse path rewrite of any string field whose JSON path matches the per-tool path-key list (e.g. `path`, `paths[*]`, `matches[*].path`). Image `read` carries `{kind: "image", path: <abs>, base64: <b64>}` — `path` is rewritten, `base64` is passed through verbatim. | Preserves UI parity with `LocalProcessProvider`. |
| `is_error`         | `ToolResponse.is_error`                            | Direct copy. |

`SandboxOutcome.execution: SandboxExecution` (defined in §2) is populated from the raw wire fields **without rewriting**: `execution.stdout = ToolResponse.stdout`, `execution.stderr = ToolResponse.stderr`, `execution.exit_status = ToolResponse.exit_status`. Path rewriting applies **only** to `model_output` and `display`; `execution` stays guest-truthful so post-mortem debugging shows what the guest process actually saw (`/work/...` paths included). Operators who want a host-flavored execution view can derive it from `model_output` (which IS rewritten); the raw stream is kept for fidelity. `guest_duration_ms` goes into the `SandboxAction` telemetry row alongside the existing `duration_ms`. There is **no** `SandboxAction.stderr` field — earlier drafts referenced one; it does not exist in the schema in §2 and is not added.

**Post-call hygiene (background daemon problem).** v1 microvm does **not** support cross-call background daemons or services. After every guest tool call — successful, errored, or timed out — the worker MUST run a hygiene probe before signaling the host that the VM is idle. The probe verifies:

1. **Process subtree empty.** Every descendant the per-call tool spawned (the `bash` shell, anything it forked) is gone. Implementation: bash runs as the leader of a fresh process group; the worker reaps it, then `kill(-pgid, 0)` returns `ESRCH`. On Linux the worker additionally consults a per-call cgroup v2 (`cgroup.procs` empty) — the cgroup is the authoritative test because a daemonized child reparented to PID 1 still appears in the cgroup. macOS/Windows use process-group reparenting detection as a coarser fallback (a known v1 gap; documented).
2. **Worker transient state cleared.** Per-call temp dirs (`$TMPDIR`/<call_id>) removed; per-call open file descriptors closed; `$PWD`/env scrubbed.

The worker reports the verdict on the wire as `ToolResponse.post_call_state` (`Clean` | `SuspectGuestState`). The tool itself may have succeeded; the post-call probe is what decides pool reuse. The host composes `final_hint = min(host_outcome_hint, post_call_state)` (`Clean < SuspectGuestState`) before calling `release(final_hint)`, so a daemonization leak forces destroy regardless of the tool's success bit. A missing `post_call_state` field defaults to `SuspectGuestState` — workers must *prove* cleanliness rather than implicitly assert it. The verdict is not surfaced to the model (the tool's `model_output` is unchanged); it's strictly a host/launcher signal.

**macOS / Windows v1: destroy-on-release, no pooling.** The cgroup-based probe is Linux-only. macOS and Windows launchers in v1 have a coarser `process-group orphan` check that does NOT detect `setsid()` / fully-detached daemons. Rather than ship a known false-`Clean`, the v1 normative rule for `VfkitLauncher` and `CloudHypervisorLauncher` is **always destroy on release**: the launcher's `release()` ignores `ExecuteOutcomeHint::Clean` and tears the VM down unconditionally. This costs the warm-pool latency benefit on those OSes; an `--sandbox-microvm-pool=force` override is available for operators who accept the daemonization risk for their own workloads. The Linux/Firecracker path keeps the cgroup-based pool. A future RFD lifts the macOS/Windows restriction once each launcher has a proven-clean per-call container/cgroup analog.

**Tested cases.** The Commit D integration suite includes negative tests for: `bash 'sleep 999 &'`, `bash 'nohup foo &'`, `bash '(sleep 5; touch /work/marker) &'`, `bash 'mkdir -p /tmp/x && touch /tmp/x/leftover'`, plus the timeout path (`bash 'sleep 60'` with `timeout_ms=1000`). Each test asserts that (a) the affected VM does **not** return to the pool, (b) the next acquire on the same `BootSpec` does NOT see the leftover process or `/tmp` residue, and (c) telemetry records `outcome=SuspectGuestState`.

**Timeout hygiene before pool return.** When `ExecuteError::CallLimit { wall_timeout }` fires, the worker MUST: (1) send `SIGTERM` to the spawned tool process group; (2) drain stdin/stdout/stderr pipes to EOF or a 250 ms hard cutoff, whichever is first; (3) on cutoff, send `SIGKILL` and continue draining; (4) emit the resulting `ToolResponse` with `is_error = true`, `model_output` set to a short timeout marker, and `exit_status = 124` (GNU `timeout` convention). Only after step (4) is the worker considered idle — and only then is the host allowed to mark the VM as `ExecuteOutcomeHint::Clean` and return it to the pool. The host's `release()` waits for `vm.execute()`'s future to resolve with the `CallLimit` error before reading the hint, which gives the worker the natural signal it has finished cleanup. If the worker itself hangs past `wall_timeout + 1s`, the launcher escalates to a hard VM kill and reports `ExecuteOutcomeHint::SuspectGuestState`.

This mapping is what `RemoteSession::execute()` in RFD 0026 must also implement so that local + remote sandboxes produce indistinguishable `ToolResult` shapes downstream of the agent loop.

#### Path virtualization — host ↔ guest path mapping

The agent's tool calls today (`read`, `write`, `edit`, `grep`, `find`,
`ls`, `bash`) accept absolute host paths like
`/home/<user>/<project>/src/lib.rs`. Inside the guest those paths
don't exist; only `/work` does. The microVM provider rewrites paths
at the dispatch boundary so the model can keep using host-shaped
absolute paths AND the guest worker still sees `/work`-shaped paths.

**Model-visible cwd**: the prompt and tool docs continue to advertise
the host-side absolute path of the user's session cwd as the working
directory (so the model's chain-of-thought, file references, and
diff hunks stay coherent across providers). The provider performs
the translation; the model never sees `/work` in tool inputs or
outputs.

**Per-tool rewriting**: the table below is the **normative
specification** today. Commit G adds
`crates/pi-sandbox/src/path_rewrite/registry.rs` — a const map of
per-tool input/output path keys (`InputKey::Single("path")`,
`OutputKey::DisplayJsonPath("path")`,
`OutputKey::ModelOutputSubstring`, etc.) plus a parity test at
`crates/pi-sandbox/tests/path_rewrite_registry.rs` that iterates the
registry against fixtures captured from `LocalProcessProvider`
results and asserts host↔guest parity for each tool. From Commit G
onward the registry, not this table, is the source of truth; the
table stays as documentation and is updated lockstep with the
registry. Adding a new guest-bound tool requires extending the
registry + adding a fixture; the parity test fails closed otherwise.

| Tool          | Inputs rewritten host→guest                          | Outputs rewritten guest→host (in `model_output` AND `display`)            |
| ------------- | ---------------------------------------------------- | ------------------------------------------------------- |
| `read`        | `path` field (`<host_cwd>/<rel>` → `/work/<rel>`); leaf must exist (`allow_missing_leaf=false`)    | text reads: `display.path`. Image reads: `display.path` (binary `display.base64` passed through verbatim).  |
| `write`       | `path` field; leaf may not exist (`allow_missing_leaf=true`) | success message in `model_output` (`"wrote N bytes to <path>"`); `display.path`.                                                     |
| `edit`        | `path` field; leaf must exist (`allow_missing_leaf=false`) | success message in `model_output`; `display.path` for both success and `edit-error` displays.        |
| `grep`        | `path` field (search root); leaf must exist          | each matching line's path prefix in `model_output`; no per-match path field in current `display` (count summary only). |
| `find`        | `path` field; leaf must exist                        | each result path in `model_output`; `display` carries glob/match-count summary only (no per-path list to rewrite). |
| `ls`          | `path` field; leaf must exist                        | `model_output` is filenames only (no directory prefix); `display.path` (the listed dir) is rewritten. |
| `bash`        | `cwd` field; leaf must exist; command string best-effort substring-rewritten (see below) | stdout/stderr in `model_output`: literal `/work` prefixes (with word boundaries) rewritten back to `<host_cwd>`. `display.command` and `display.cwd` rewritten. |

**Canonicalization (`resolve_beneath`)**: the provider does not run a
plain `canonicalize` on the requested path — that breaks `write` to
new files because the leaf doesn't exist yet. Instead each input path
goes through:

```rust
fn resolve_beneath(
    host_cwd: &Path,        // canonicalised once at session start
    requested: &Path,       // tool input
    allow_missing_leaf: bool, // true for write only in v1
) -> Result<PathBuf, ToolError>
```

Algorithm:
1. If `requested` is relative, treat it as relative to `host_cwd`.
2. Walk from `host_cwd` toward `requested` lexically, popping `..`
   without ever escaping above `host_cwd`. (Strict lexical jail; no
   filesystem touch.)
3. Find the deepest *existing* ancestor of the resulting path and
   `canonicalize` it (symlink-resolve). Then re-attach the remaining
   non-existing tail lexically.
4. Verify the canonicalised root is still beneath the canonicalised
   `host_cwd`; if not, reject.
5. If `allow_missing_leaf=false` and the final path doesn't exist,
   reject (the per-tool semantic — read/edit need an existing file).
6. Reject any escape with `ToolError::InvalidInput("path escapes session cwd: <requested>")`. v1 does not add a new `ToolError` variant; the existing `InvalidInput` carries enough context for the agent loop and avoids a wire/semver bump on `pi-tool-types`.

**`bash` contract**: shell commands are free-form, so the provider
can't fully translate paths embedded inside them. The contract is:

- The `cwd` parameter (if explicit) is rewritten host→guest;
  otherwise the worker uses `/work`.
- The command string is best-effort substring-rewritten: if it
  contains the literal host cwd as a prefix-aligned substring (with
  word boundaries to avoid clobbering `/home/work/...`), that
  substring is replaced with `/work`. No deep parsing.
- The model is documented (in the bash tool description) that under
  microvm, the cwd is the user's session directory and absolute
  paths to files inside the project resolve, but absolute paths
  outside the project don't. (Same constraint as virtio-fs `local`
  mode + the cfs-fs-server `--backend-root` jail in `managed` mode.)
- `pwd` inside bash returns the host-side path: the worker injects
  `PWD=<host_cwd>` into the bash environment, which makes shell
  builtins `pwd` (without `-P`) and `echo $PWD` print the host path.
  This is **best-effort, not a complete illusion** — anything that
  resolves the kernel cwd directly (`pwd -P`, `readlink /proc/self/cwd`,
  `getcwd(3)`, `realpath .`) bypasses `$PWD` and shows `/work`. Output
  rewriting (`/work` → `<host_cwd>` substring substitution) catches
  most of those leaks in stdout/stderr but is not airtight; this is
  documented for users running scripts that compare paths byte-for-byte.

**Why not just expose `/work` to the model**: tested empirically
on local-process: when the model sees `/work/src/lib.rs` instead of
`/home/<user>/.../src/lib.rs` in the prompt, its diff suggestions
and "open this file in your editor" pointers are wrong host-side.
Path virtualization keeps the model's externally-visible state
consistent across providers.

#### Filesystem semantics — `/work` read-write (transport split per OS)

`/work` in the guest is the agent's read-write workspace, mounted from
the host's session cwd. Tools that mutate files (write/edit) modify
host files directly. The guest enforces no path traversal beyond
`/work` (the worker's tool dispatcher rejects absolute paths or `..`
segments outside the mount).

The transport differs per OS:

- **Linux/Firecracker Stage 1:** `/work` is a contextfs FUSE mount,
  backed by host-side `cfs-fs-server` over a Noise-IK channel; every
  write is mediated by contextfsd's PDP and audit-ping (§3.5).
  `virtiofsd` is **not** required on Linux Stage 1.
- **macOS/Windows v1.0:** `/work` is a virtio-fs RW share (no
  contextfs); writes are direct. A follow-up RFD brings contextfs to
  these platforms once contextfs supports a non-vsock embedder
  transport.

`bash` runs inside the guest with `/work` as its cwd. Bash writes to
`/work` are durable on the host. Bash writes outside `/work` (in the
guest's tmpfs, e.g. `/tmp`) are ephemeral — gone at VM shutdown. This
is documented loudly so users understand the boundary.

This is the v1.0 minimum. A read-only-then-flush copy-up overlay was considered and rejected (transactional-looking but breaks on partial writes, symlinks, concurrent reads).

#### Worker reuse for remote backends (cross-RFD ownership)

`pi-sandbox-worker` is **explicitly designed to be transport-agnostic** at the JSON-line layer. The worker reads `ToolRequest` from any `AsyncRead` and writes `ToolResponse` to any `AsyncWrite`; v1.0 wires it to vsock for the local microVM case but the same binary runs unchanged when:

- Uploaded into an E2B / Sprites / Daytona sandbox as a long-running process talking over a Unix socket (or stdin/stdout of a `nc` connection).
- Embedded in any future transport that delivers byte streams.

This decision lives in **RFD 0023** (not 0026) because it constrains the worker's design, not the remote vendor's API. RFD 0026 v0.2 may pick **Option A (ship-the-worker)** or **Option B (per-vendor reimplementation)** as its remote-side strategy, but if it picks A, the worker binary it ships is exactly the artifact this RFD produces. The protocol crate (`pi-sandbox-protocol`) and the worker (`pi-sandbox-worker`) are each one artifact, multiple transports.

What RFD 0026 owns:
- The remote vendor adapter that reshapes vendor APIs into a transport for the worker (or, under Option B, into per-tool RPCs).
- The cwd-sync / git-clone strategy that materialises the user's project inside the remote sandbox.
- The cost-telemetry and auth-storage glue.

What RFD 0023 owns (this RFD):
- The worker binary, the wire protocol, the `ToolResponse↔ToolResult` mapping, and the local microVM transport.

### 3.5. Deployment-mode matrix and ContextFS integration

`microvm` ships two deployment modes, but **availability per OS is
not symmetric**. The matrix:

| OS / Launcher                  | `local` | `managed` | `/work` transport in v1                              |
| ------------------------------ | :-----: | :-------: | ---------------------------------------------------- |
| Linux / Firecracker            |   ✗ v1  |    ✓     | contextfs FUSE over Noise-IK (§3.5.1–§3.5.9)         |
| macOS / vfkit                  |    ✓    |   ✗ v1   | virtio-fs RW (Hypervisor.framework, vfkit ≥ 0.5)     |
| Windows / cloud-hypervisor     |    ✓    |   ✗ v1   | virtio-fs RW (WHPX, cloud-hypervisor)                |

The CLI flag is `--sandbox-microvm-mode={local,managed,auto}`, default
`auto`. `auto` resolves per-OS to the only column with a checkmark
(the matrix above). Explicit pins fail loud on unsupported
combinations:

- `--sandbox-provider=microvm:firecracker --sandbox-microvm-mode=local`
  → hard error at provider construction:
  `unsupported: Linux/Firecracker v1 has no `local`-mode workspace
  transport. Firecracker does not support virtio-fs (RFD
  0023-known-issues). Use --sandbox-microvm-mode=managed (requires
  contextfs broker config) or --sandbox-provider=local-process.`
- `--sandbox-provider=microvm:vfkit --sandbox-microvm-mode=managed`
  → hard error: contextfs's vsock embedder transport is Linux-only
  per contextfs RFD-0023 §"Goals/non-goals: Windows/macOS embedders".

**Why Linux/Firecracker has no `local` mode in v1.** Firecracker
ships no virtio-fs support (its public stance, captured in
`rfd/0023-known-issues.md`). A "no contextfs, but still RW workspace"
path on Firecracker would require a custom FUSE-over-vsock bridge
that pi-rs does not currently have specced. Designing that bridge
is a separate RFD; until it lands, Linux microVM users either bring
contextfs (use `managed`) or use `--sandbox-provider=local-process`.

**Why macOS/Windows have no `managed` in v1.** ContextFS's vsock
transport is Linux-only. A non-vsock embedder transport for those
platforms is a contextfs follow-up.

The two **modes** describe what's mounted at `/work` and what
authentication the agent's writes go through:

- **`local` mode (macOS/Windows v1)**: no broker, no OIDC, no
  per-tenant master material, no contextfs daemon. `/work` is a
  direct virtio-fs RW share from the launcher to the host's session
  cwd. The agent's writes hit the host filesystem unmediated by any
  PDP or audit chain. This is the single-user-CLI shape and matches
  the v1.0 promise of `pi --sandbox-provider=microvm` for ordinary
  local users on Mac/Windows.
- **`managed` mode (Linux/Firecracker v1)**: brings up an in-guest
  `contextfsd` daemon that mediates `/work` writes through a host-side
  broker. Adds policy-as-code (Cedar PDP), per-VM tenant secret
  derivation, OIDC validation against host workload-identity tokens,
  and audit-chain plumbing. This is the deployment shape pi-rs ships
  for hostile-tenancy / compliance-graded / multi-user environments
  (the platform shape RFD 0021 anticipated). For the maintainer's
  own dogfood, `managed` mode is what runs.

`pi sandbox doctor` per OS:

- macOS/Windows (`local`): probes the platform launcher
  (vfkit / cloud-hypervisor), virtio-fs prerequisites, and the rootfs
  cache.
- Linux (`managed`): probes Firecracker + KVM + vsock module + the
  contextfs binaries (contextfs-broker, cfs-fs-server, cfs-mesh,
  contextfs-cli) + the rootfs cache. Does NOT probe virtio-fs (not
  needed on this path).

The remainder of §3.5 (3.5.1 through 3.5.9) describes the `managed`
mode contract — Linux/Firecracker only. macOS/Windows in `local`
mode have no contextfs surface at all. Operator-managed deployments
invoke pi with `--sandbox-microvm-mode=managed` plus the fields
documented below; misconfiguration (e.g. `mode=managed` but no
`broker.socket_path` configured) is a hard error at provider
construction, not at first request.

#### 3.5.1 — `vm_id` source

Pi-rs already mints a per-VM UUIDv4 in
`crates/pi-sandbox/src/microvm/firecracker.rs:643` (`Uuid::new_v4()`)
at provisioning. The same id is hashed into the vsock CID in
`vm_id_to_cid()`. Commit G threads it into the daemon TOML as the
operator-supplied `vm_id` field (contextfs RFD-0023 §5: required when
embedder mode is in use; format `[A-Za-z0-9._-]{1,128}`).

`vm_id` lifetime: tied to the warm-pool VM, not to a single tool call.
A pooled VM that survives N tool calls retains the same `vm_id` (and
the same per-VM secret derived from it) across those calls; rotation
fires when the VM is torn down per the §4 pool-rotation policy.
Audit attribution is therefore per-VM, not per-call. Subagents that
share a pooled VM (the v1 default per RFD 0005) share its `vm_id`;
isolated subagents constructing their own `MicroVmProvider` mint a
distinct `vm_id`.

#### 3.5.2 — Per-VM tenant secret (host-side derivation)

At provisioning, the orchestrator calls the contextfs CLI helper:

```bash
contextfs-cli key derive-per-vm-secret \
    --master-path /etc/contextfs/<tenant>.master \
    --tenant-id <tenant_id> \
    --vm-id "<pi_firecracker_uuid>" \
    --master-epoch <N> > "<run_dir>/<vm_id>/cfs-tenant-secret"
chmod 0600 "<run_dir>/<vm_id>/cfs-tenant-secret"
```

The output is **raw 32 bytes** as written to stdout (matching the
in-tree `tenant_secret` files contextfsd already loads). Pi-rs writes
to a tmpfs file (mode 0600) inside the VM's run-dir, e.g.
`<run_dir>/<vm_id>/cfs-tenant-secret`, and bind-mounts the file into
the guest at `/var/run/cfs/tenant_secret`. The daemon TOML's
`tenant_secret_path` points at the bind-mounted location.

`<N>` (master epoch) is read from broker configuration; Commit G's
`MicroVmProvider` accepts a `master_epoch: u32` field on its
construction config and threads it into both the CLI invocation and
the TOML.

**Pool refresh on `BrokerMasterEpochTooOld`.** Per the contextfs
typed `StartError::BrokerMasterEpochTooOld` variant, when the broker
rotates its epoch beyond the 4-deep window, in-flight VMs in the warm
pool become unusable. The MicroVmProvider's pool eviction policy
treats this as fatal-for-this-VM: the affected VM is torn down, a
fresh derivation runs with the current epoch, and a new VM is booted
into the pool. Other VMs in the pool with the same epoch may also
need refresh; v1 evicts them lazily on next-use rather than eagerly.

#### 3.5.3 — Workload-identity OIDC token

The host orchestrator (the layer above pi-rs that mints jobs) supplies
a Workload Identity token per job. Commit G accepts the token path as
a `MicroVmProviderConfig` field; the orchestrator's exact source is
out of scope for this RFD. Pi-rs bind-mounts the token file into the
guest at `/var/run/secrets/token` (mode 0444, single-tenant per VM)
and the daemon TOML `oidc_token_path` points at it. The daemon reads
the token on every `WriteVerifyRequest`; rotation is host-driven
(orchestrator rewrites the file in place when its WI token rotates).

Failure routing for contextfs's three typed OIDC denials:
- `oidc_validation_failed` → `StartError::BrokerOidcRejected`.
  Transient if JWKS rotation; persistent if config. MicroVmProvider's
  `acquire_to_ready` retries once after JWKS-rotation grace, otherwise
  surfaces to the operator.
- `oidc_token_required` — broker has a validator configured but the
  daemon sent an empty token. Configuration bug; surfaced immediately,
  no retry.
- `oidc_token_unexpected` — daemon sent a token but the broker has no
  validator for this tenant (broker's `--tenant-mode` flag missing or
  the wrong validator pinned). Configuration bug; surfaced
  immediately, no retry.

#### 3.5.4 — Per-mount audit-ping

ContextFS RFD-0023 §7 (HEAD `dbe2df5`) ships a v1 audit-ping shape:
every successful write-class FUSE op (write / create / unlink /
rename / setattr / xattr.set / xattr.remove) forwards the
AuditRecord to the broker as a `Request::WriteAuditPing`. The daemon
TOML opts in per mount.

For the routine code-editing-agent use case Commit G targets,
`fail-open` with `high_water_mark = 1024` is the default — a transient
broker hiccup must not fail the agent's `cargo build`. Operators with
hostile-tenancy / compliance-graded tenants set their tenant configs
to `fail-closed`; the contextfs broker (post-`dbe2df5`) refuses writes
with `EIO` BEFORE backend mutation when the audit-ping channel is
saturated, so fail-closed is integrity-correct. The audit-ping queue
is in-memory only (contextfs RFD-0023 §7 explicit note); daemon
restarts wipe queued events.

Pi-rs will expose equivalent tenant-level defaults in its sandbox
provider config (`MicroVmProviderConfig::audit_ping`, with per-tenant
overrides resolvable by the orchestrator). The per-tenant override
plumbing is a Commit G deliverable.

#### 3.5.5 — Transport topology (Linux/Firecracker Stage 1) — two channels

The Linux Stage 1 design has **two separate cfs-mesh channels**: a
control plane (broker traffic) and a data plane (cfs-fs-server FUSE
backend traffic). One bridge/listener pair per channel; one host-side
service per channel; one guest-local UDS per channel that contextfsd
consumes:

```
            ┌──────────────────── HOST (per VM) ─────────────────┐
            │ contextfs-broker        cfs-fs-server               │
            │      ↑ /run/<vm>/        ↑ /run/<vm>/                │
            │      │  broker.sock      │  fs.sock                  │
            │  cfs-mesh vsock-bridge  cfs-mesh vsock-bridge        │
            │  (CID, port=P_b)        (CID, port=P_f)              │
            └──────┼─────────────────────────┼───────────────────┘
                   │                         │
                   │      vsock CID =        │
                   │      vm_id_to_cid(vm_id)│
            ┌──────┼─────────────────────────┼─────── GUEST ─────┐
            │ cfs-mesh vsock-listen   cfs-mesh vsock-listen      │
            │  --port P_b --uds        --port P_f --uds          │
            │  /run/cfs/broker.sock    /run/cfs/fs.sock          │
            │      ↑                       ↑                     │
            │      │     [broker]          │  [mount.remote_fs]  │
            │      │     socket_path       │  target_uds         │
            │  contextfsd ─── reads daemon.toml ─── mounts /work │
            │                                                    │
            │  pi-sandbox-worker (cwd = /work)                   │
            └────────────────────────────────────────────────────┘
```

Host-side, per VM:

- `contextfs-broker --listen-uds <run_dir>/<vm_id>/broker.sock
  --tenant-peer-uid <tenant>=<bridge_uid> ...` — the broker on a
  per-VM UDS. The broker's TCP-HMAC auth (`--auth-secret-path`) is
  **not used**; UDS auth is by SO_PEERCRED via `--tenant-peer-uid`,
  matching the bridge process's effective uid.
- `cfs-fs-server --socket <run_dir>/<vm_id>/fs.sock
  --backend-root <host_cwd> --allowed-uid <bridge_uid> ...` —
  the file-server that fronts the user's cwd as the FUSE backend.
- `cfs-mesh vsock-bridge --cid <guest-cid> --port <P_b>
  --key <broker_bridge_key> --peer-pubkey-path <guest_broker_pubkey>
  --target-uds <run_dir>/<vm_id>/broker.sock` — control plane.
- `cfs-mesh vsock-bridge --cid <guest-cid> --port <P_f>
  --key <fs_bridge_key> --peer-pubkey-path <guest_fs_pubkey>
  --target-uds <run_dir>/<vm_id>/fs.sock` — data plane.
- `<guest-cid>` is `vm_id_to_cid(vm_id)`; `P_b` and `P_f` are
  per-VM-allocated TCP-style port numbers (e.g. base 5000 + 2*pool_idx
  and base 5000 + 2*pool_idx + 1).

Guest-side, in the rootfs init script:

- `cfs-mesh vsock-listen --cid <numeric-guest-cid> --port <P_b>
  --key <guest_broker_key> --peer-pubkey-path <broker_bridge_pubkey>
  --uds /run/contextfs/broker.sock` — control plane re-exposed.
- `cfs-mesh vsock-listen --cid <numeric-guest-cid> --port <P_f>
  --key <guest_fs_key> --peer-pubkey-path <fs_bridge_pubkey>
  --uds /run/contextfs/fs.sock` — data plane re-exposed.
- `contextfsd --config /etc/contextfsd/daemon.toml` — daemon. Its
  `[broker].socket_path` points at `/run/contextfs/broker.sock` and
  its `[mount.remote_fs].target_uds` points at
  `/run/contextfs/fs.sock`.
- `pi-sandbox-worker` with `cwd = /work` — runs only after readiness
  gates fire (§3.5.9).

**Key material**, per contextfs RFD-0023 §4 Noise-IK:

- One bridge static key + one corresponding guest static key per
  channel (4 keypairs total per VM). Generated host-side at VM
  provisioning, dropped into the rootfs alongside the daemon TOML.
  Lifetime is the VM lifetime; no rotation within a single VM. Keys
  are per-VM (not per-tenant or per-host) so a guest compromise can
  forge nothing beyond THIS VM's data plane.
- `pi sandbox doctor` checks the bridge keypairs exist and are in
  the rootfs manifest before launch.

Cross-discriminant safety per contextfs RFD-0023 §3 (`PeerId::Uid`
vs `PeerId::VsockCid`): the guest's UDS allow-list (`PeerId::Uid` of
contextfsd) and the host bridge's vsock CID allow-list never compare
across discriminants — a misconfigured allow-list fails closed.

#### 3.5.6 — Broker invocation (host-side)

The host-side broker is invoked per VM with:

```
contextfs-broker \
    --listen-uds <run_dir>/<vm_id>/broker.sock \
    --tenant-peer-uid <tenant_id>=<bridge_uid> \
    --tenant-mode <tenant_id>=embedder \
    --verify-write-oidc-issuer <issuer-url> \
    --verify-write-oidc-audience <wi-audience> \
    --verify-write-oidc-alg RS256 \
    [other flags]
```

No `--vsock*` flags (vsock termination is owned by the host-side
`cfs-mesh vsock-bridge` per §3.5.5); no `--auth-secret-path` (that
flag is the TCP-HMAC path and conflicts with the UDS topology).

`--tenant-mode <t>=embedder` is the operator's opt-in to embedder
mode. Without it the broker defaults to legacy mode and refuses any
request carrying `vm_id`/`master_epoch` with a typed
`tenant_mode_legacy_no_vm_id` denial. With it set, every request
must carry non-empty `vm_id` + `master_epoch` (typed `vm_id_required`
denial otherwise). Pi-rs's MicroVmProvider asserts both are set on
its config at construction; absence is a hard error at startup, not
at first request.

`--tenant-peer-uid <t>=<bridge_uid>` pins SO_PEERCRED auth on the
broker UDS to the cfs-mesh-bridge's effective uid. v1.0 runs the
bridge as a dedicated `pi-sandbox-bridge` system uid; pi-rs's
provisioning sets up the uid at first run and the `MicroVmProvider`
holds the uid in its config.

#### 3.5.7 — Daemon TOML rendered at provisioning

The full operator-rendered `/etc/contextfsd/daemon.toml` inside the
guest, matching contextfsd's actual config shape:

```toml
tenant_id          = "tenant-a"
vm_id              = "<pi_firecracker_uuid>"
master_epoch       = 7
tenant_secret_path = "/var/run/cfs/tenant_secret"   # bind-mounted from host tmpfs
oidc_token_path    = "/var/run/secrets/token"        # bind-mounted WI token
audit_log_path     = "/var/log/contextfs/audit.ndjson"

[pdp]
policy_path       = "/etc/contextfs/policy.cedar"
default_principal = 'Agent::"pi"'

[broker]
socket_path = "/run/contextfs/broker.sock"          # guest-local UDS, exposed by control-plane cfs-mesh vsock-listen (§3.5.5)

[[mount]]
name       = "workspace"
mountpoint = "/work"
backend    = "remote-fs"                             # FUSE mediates via cfs-fs-server over Noise-IK data plane
cache_dir  = "/var/cache/contextfs/workspace"
read_only  = false
audit_ping = { mode = "fail-open", high_water_mark = 1024 }

  [mount.remote_fs]
  target_uds = "/run/contextfs/fs.sock"              # guest-local UDS, exposed by data-plane cfs-mesh vsock-listen (§3.5.5)
```

Commit G's `MicroVmProvider` renders this from a
`MicroVmProviderConfig` struct at construction. The TOML schema is
that of contextfsd; the canonical source is
`<contextfs>/crates/contextfsd/src/config.rs` and Commit G adds a
serde-aligned struct on the pi-rs side that fails loud on schema
drift (broken match → compile-time error).

#### 3.5.8 — Version compatibility

ContextFS broker MUST be `>= v0.3.0` — the version that shipped
`vm_id`/`master_epoch`/`oidc_token` over the wire and the
`--tenant-mode` flag. v0.2.x brokers reject any embedder request via
serde unknown-field rejection at the first `verify_write`; that is
the documented fail-loud signal.

`pi sandbox doctor` performs **functional connectivity checks**, not
argv inspection of foreign processes:
- `cfs-mesh vsock-bridge` reachable from the host (TCP/UDS round-trip).
- `contextfs-broker` reachable via the bridge (a no-op `verify_write`
  with a synthetic `vm_id` is rejected with a typed denial code; the
  doctor accepts any 4xx-class response as "reachable").
- `contextfs-cli` invocable on `$PATH`, prints a version string.
- ContextFS daemon binary is present in the cached rootfs and matches
  a known SHA from the rootfs manifest (RFD 0023 §3.2).

#### 3.5.9 — Guest boot contract (rootfs + `pi-cfs-init` readiness)

The §3.2 rootfs builder ships the following additions for Linux
Stage 1:

| Binary / file                                  | Source                                  | Purpose                              |
| ---------------------------------------------- | --------------------------------------- | ------------------------------------ |
| `/usr/local/bin/contextfsd`                    | `contextfs-v0.3.0` static-musl release  | FUSE mount + verify_write loop       |
| `/usr/local/bin/cfs-mesh`                      | same                                    | vsock-listen subcommand              |
| `/usr/local/bin/pi-cfs-init`                   | pi-rs (NEW, owned by Commit G)          | rootfs init wrapper + readiness gate |
| `/etc/contextfsd/daemon.toml`                  | rendered at provisioning (§3.5.7)       | daemon config                        |
| `/etc/contextfs/policy.cedar`                  | bundled in rootfs (default agent policy)| Cedar PDP                            |
| `/var/cache/contextfs/workspace/` (tmpfs)      | created at boot                         | FUSE backend cache                   |
| `/var/log/contextfs/` (tmpfs)                  | created at boot                         | audit chain                          |
| Bind-mounted: `/var/run/cfs/tenant_secret`     | host tmpfs (§3.5.2)                     | per-VM keying material               |
| Bind-mounted: `/var/run/secrets/token`         | host WI mount (§3.5.3)                  | OIDC token                           |

Required kernel/guest features (the rootfs builder asserts each at
build time, and `pi sandbox doctor` re-checks at host provisioning):

- Kernel: `CONFIG_FUSE_FS=y` (or built-in).
- Kernel: `CONFIG_VSOCKETS=y`, `CONFIG_VIRTIO_VSOCKETS=y`.
- Device: `/dev/fuse` accessible to the contextfsd uid.
- Static-musl binaries (no glibc in the alpine rootfs).

**`pi-cfs-init`** is a new static-musl Rust binary owned by pi-rs
(roughly 200 LoC, included in Commit G's scope). It:

1. Mounts tmpfs at `/var/cache/contextfs/workspace/` and
   `/var/log/contextfs/`.
2. Spawns the two `cfs-mesh vsock-listen` processes per §3.5.5
   (control-plane `/run/contextfs/broker.sock` and data-plane
   `/run/contextfs/fs.sock`). Readiness gate: each UDS accepts a
   connection.
3. Spawns `contextfsd --config /etc/contextfsd/daemon.toml` (the
   actual flag form, per `<contextfs>/crates/contextfsd/src/main.rs`).
4. Polls `stat("/work")` until it returns a `FUSE` filesystem type
   (typically <100 ms after contextfsd reports the mount). On success,
   writes `/work/.cfs-ready` (touch).
5. Spawns `pi-sandbox-worker` with `cwd = /work` only after step 4's
   sentinel exists. This is the existing worker; no changes beyond
   the cwd.
6. Acts as PID 1 / re-aps zombies from the daemon side; if `contextfsd`
   exits, `pi-cfs-init` writes a single-line failure record to the
   guest's serial console (`/dev/ttyS0`) of the form
   `PI_FAIL: contextfsd exited with code <N>` and panics the guest.
   The host-side `FirecrackerLauncher` is already wired to capture
   the guest's serial-console stdout (per §4.1's `firecracker
   --boot-args "console=ttyS0"`) and parses the `PI_FAIL:` prefix.
   Firecracker does NOT support virtio-fs (per `rfd/0023-known-issues.md`);
   we deliberately do not use a virtio-fs sentinel path.
7. If steps 2-4 fail to complete within `boot_timeout` (default 10s),
   `pi-cfs-init` writes `PI_FAIL: ready-timeout` to the serial console
   and exits non-zero. The `MicroVmLauncher` surfaces this as a typed
   `AcquireError::ReadyTimeout { boot_timeout, last_dmesg }`.

The readiness sentinel lives in `/work` (the FUSE mount itself); its
appearance proves both that the FUSE mount is up AND that contextfsd
accepted the write through the broker → `cfs-fs-server` → kernel
path. We do NOT claim contextfsd writes the sentinel — `pi-cfs-init`
does, after externally observing the FUSE mount.

Failure surface visible to `MicroVmProvider`:

| Trigger                                                | `AcquireError` variant            |
| ------------------------------------------------------ | --------------------------------- |
| `cfs-mesh vsock-listen` fails (vsock CID/port collision) | `HostTransportSetup`            |
| `contextfsd` exits non-zero on startup                  | `GuestDaemonStartFailed { exit }` |
| `contextfsd` rejects with `BrokerMasterEpochTooOld`     | `BrokerMasterEpochTooOld { epoch }` (eviction per §3.5.2) |
| `contextfsd` rejects with `tenant_mode_legacy_no_vm_id` | `BrokerTenantModeMismatch`        |
| `contextfsd` rejects with `oidc_*` codes                | `BrokerOidcRejected { code }`     |
| `/work/.cfs-ready` not written within `boot_timeout`    | `ReadyTimeout { boot_timeout, last_dmesg }` |

Schema-drift detection: pi-rs's `MicroVmProviderConfig` has a
`#[serde(deny_unknown_fields)]` mirror of contextfsd's `DaemonConfig`
shape. **Drift surfaces at TOML deserialize time** when the user's
contextfs-version pin changes — not as a compile-time error (the
config lives in a sibling repo with independent semver). v1 ships an
integration test (`tests/contextfs_schema_pin.rs`) that
`serde::from_str`s the rendered TOML against contextfsd's actual
config struct, gated on a `CONTEXTFS_REPO_PATH` env var so CI can
opt in.

### 4. Per-OS launcher impls

#### Linux: `FirecrackerLauncher` (`#[cfg(target_os = "linux")]`)

- Probes `/dev/kvm` and the `firecracker` binary at construction.
- Maintains a **warm pool of N (default 2) pre-booted VMs per `BootSpec` ring**, as `tokio::sync::Mutex<HashMap<BootSpec, VecDeque<WarmVm>>>`. `acquire(&boot_spec)` pops a warm VM from the matching ring in O(1); release returns it to the same ring. Two callers with different `BootSpec`s (e.g. different `host_cwd` per RFD 0006 worktree) get separate rings and cannot share a warm VM. Default 2/ring because real coding-agent tool calls are dominantly sequential (write → read → bash → read); pool=2 covers one parallel subagent burst at ~512MB resident. Telemetry on pool hit-rate per ring decides whether to bump to 4. Empty rings garbage-collect after their last VM is released and an idle TTL elapses (default 5 min) so a long-running session that drifts across many cwds doesn't leak unbounded ring entries.
- Pool refills opportunistically in the background.
- Each Linux/Firecracker VM gets its own firecracker process, API socket, vsock socket, plus per-VM `cfs-fs-server` + `contextfs-broker` instances on per-VM run-dir UDSes (§3.5.5). macOS/vfkit and Windows/cloud-hypervisor VMs use a per-VM virtio-fs share (those launchers expose virtio-fs; Firecracker does not). Rotation: VMs are torn down and replaced after N tool calls or T seconds (default 50 calls / 5 minutes) to bound cumulative state leakage.
- Cribbed from aegis: vsock client wire-up; the rest is fresh.

LoC estimate: 1500 (incl. pool) + 200 for tests.

#### macOS: `VfkitLauncher` (`#[cfg(target_os = "macos")]`)

- Probes Hypervisor.framework availability + `vfkit` on `$PATH` (or the vendored copy if pi ships one).
- Uses vfkit's CLI-config model (no API socket — config baked at spawn).
- virtio-fs and virtio-vsock both supported in vfkit ≥ v0.5.
- v1.0 may cold-boot per call (no pool); upgrade to a pool later if telemetry justifies.

LoC estimate: 800.

#### Windows: `CloudHypervisorLauncher` (`#[cfg(target_os = "windows")]`)

- Probes WHPX (`Windows Hypervisor Platform`) availability + `cloud-hypervisor.exe`.
- v1.0 cold-boots per call.
- A WSL2-based launcher was considered and **cut from v1**; users who already run WSL2 can continue using `--sandbox-provider=local-process` inside the WSL2 distro. A separate `Wsl2Launcher` would need its own design section (rootfs, vsock, CI plan); not in scope for this RFD.

LoC estimate: 1000.

### 5. CLI UX and `pi sandbox doctor`

```
pi --sandbox-provider=microvm                  # auto-pick per OS
pi --sandbox-provider=microvm:firecracker      # explicit pin
pi --sandbox-provider=microvm:vfkit
pi --sandbox-provider=microvm:cloud-hypervisor
pi --sandbox-provider=local-process            # existing, no isolation
```

`pi sandbox doctor` runs all available launchers' `probe()` and prints a per-transport report:

```
$ pi sandbox doctor
microvm:firecracker  ✓ available  (Firecracker v1.15.0, probe 87ms, mode=managed)
                       checks: kvm_open_rw ✓, kvm_group_member ✓,
                               vsock_module ✓, fc_binary ✓,
                               contextfs_broker ✓, cfs_fs_server ✓,
                               cfs_mesh ✓, contextfs_cli ✓ (v0.3.2)
microvm:vfkit        ✗ unavailable
                       blocker: vfkit binary not found on $PATH
                       remediation: brew install vfkit  # or download from https://...
                       checks: hypervisor_framework ✓, vfkit_binary ✗
microvm:cloud-hypervisor ✗ unavailable (linux build does not include WHPX path)

Auto-selected for current host (linux): microvm:firecracker
```

Exits 0 if at least one transport is available; 2 otherwise.

#### Probe depth

The probe MUST go beyond binary-existence checks. The known false-positive case is "user passes `pi sandbox doctor` then sees `acquire()` fail with EPERM" — they were misled. Per-launcher checks:

- **`FirecrackerLauncher`** (Linux v1 = `managed` only): `firecracker` binary on PATH; `/dev/kvm` openable RW (catches kvm group / cgroup denial); kernel `vsock` module loaded (`lsmod` or `/sys/module/vsock`); `contextfs-broker` + `cfs-fs-server` + `cfs-mesh` + `contextfs-cli` binaries on PATH (per §3.5.8 functional probes); AppArmor/SELinux denial test (no-op `firecracker --version` under restrictive profile). Does NOT probe `virtiofsd` — Firecracker has no virtio-fs and Linux v1 doesn't need it.
- **`VfkitLauncher`:** Hypervisor.framework available (probe via `sysctl kern.hv_support`); `vfkit` binary on PATH or vendored; vfkit version ≥ 0.5 (virtio-vsock support).
- **`CloudHypervisorLauncher`:** WHPX feature enabled (probe via `Get-WindowsOptionalFeature -FeatureName HypervisorPlatform`); `cloud-hypervisor.exe` on PATH; admin/elevated check (WHPX requires either elevated tokens or a service install).

A real probe takes 50–200ms, not 10ms — the time accounts for the actual filesystem/process operations.

### 6. Threat model

`microvm` defends against:

- **Malicious model output executing arbitrary host commands.** A model that is jailbroken or prompt-injected emits `bash` calls; the guest contains the blast radius (no host fs outside `/work`, no network, killed on timeout).
- **Tool input crafted to escape the sandbox.** Path traversal via absolute paths or `..` segments in `read`/`write`/`edit` is rejected by the guest worker. Host-side cwd boundary enforcement is **strong on Linux/Firecracker** (`cfs-fs-server --backend-root` does symlink-resolve-beneath: every path operation is canonicalised under the backend root, and a symlink whose target escapes the root is rejected). On **macOS/vfkit** and **Windows/cloud-hypervisor** v1, virtio-fs's mount-rooted scope only blocks paths that escape via the FUSE namespace; a symlink already inside `/work` that points outside (e.g. `/work/escape -> /etc`) is followed by virtio-fs without a beneath-root check. **v1 limitation, documented**: on those platforms the agent can theoretically read/write outside `/work` via a pre-existing symlink in the host cwd. v1.1 adds explicit resolve-beneath in the worker on those platforms; until then, operators on macOS/Windows should not microvm-sandbox sessions where the host cwd contains adversarially-placed symlinks.
- **Resource exhaustion.** `VmCeiling` (per-VM, set at boot) and `CallLimits` (per-call, derived from `ToolContext`) cap memory, vCPUs, disk, wall time, and per-call output bytes — `VmCeiling` is the partition key on the warm pool.

`microvm` does **not** defend against:

- **A user who explicitly chose `local-process`.** That flag means "I trust the model." No silent downgrade from `microvm` to `local-process` ever happens.
- **Side channels through `/work`.** A malicious tool writing a poisoned file the user later opens is out of scope; users are responsible for what they let the model edit.
- **VMM exploits.** Firecracker / vfkit / cloud-hypervisor have small but non-zero attack surfaces. Mitigation: pin to recent versions, monitor security advisories.

**Rootfs contents are public knowledge.** The alpine image, busybox version, the `pi-sandbox-worker` binary itself — all of it is observable to a jailbroken model running `cat /etc/os-release` or `cat /etc/passwd` inside the guest. Do **not** bake credentials, tokens, or per-user state into the rootfs. The rootfs is a single artifact shared by every pi user worldwide; treat it like a pre-built docker image.

**Guest userland: real Bash, busybox for the rest.** The `bash` tool's
contract MUST be preserved across providers (model behavior depends
on it). The guest rootfs ships **real Bash** as `/bin/bash` (the
`bash-static-musl` 5.x release, ~1.5 MiB statically linked); the
worker's `bash` tool dispatcher invokes `/bin/bash`, never busybox
`ash`. The rest of the userland (coreutils-like) IS busybox to keep
the rootfs small — so `find`, `grep`, `ls`, `cat`, etc. have busybox
quirks: `find -printf`, `grep -P`, `ls --color=auto`, GNU-specific
tar flags, etc. behave differently. v1.0 ships with a
known-incompatibility list in the docs covering only the busybox
side; the `bash` tool itself is unchanged. Users hitting busybox
friction can pin `--sandbox-provider=local-process`; a future RFD
can offer a glibc/coreutils rootfs as an opt-in pin
(`microvm:firecracker:rootfs=glibc-debian`).

**ContextFS-specific threats (Linux Stage 1, added v0.6).** The §3.5
integration introduces new threat categories beyond plain virtio-fs:

- **Guest-resident per-VM secret + OIDC token.** A jailbroken model
  that escalates inside the guest can read both. Compartmentalization:
  per-VM secret derivation (contextfs RFD-0023 §5) bounds the leak to
  THIS VM's audit chain — cross-tenant `decision_id`s remain forgeable
  only for THIS `vm_id`. The OIDC token is the host orchestrator's WI
  token, scoped per-job. Mitigation: short token TTL (host-rotation
  cadence ≤ token_lifetime), per-VM secret regenerated on every fresh
  pool boot, and warm-pool VMs torn down per §4 rotation policy.
- **Audit-ping loss window.** Between successful FUSE write and broker
  ping, an in-guest attacker who crashes the VM mid-write wipes the
  pending event. Bounded by the contextfs RFD-0023 §7 in-memory queue
  drain (default 1s); fail-closed shrinks the window to zero at the
  cost of broker-availability coupling.
- **Vsock handshake DoS.** A rogue guest can open half-open Noise-IK
  handshakes from a forged CID at line rate; the host-side
  `cfs-mesh vsock-bridge` rate-limits per the contextfs RFD-0023 §4
  cap (`--vsock-handshake-rate`, default 32/CID, 256 global). Pi-rs's
  MicroVmProvider observes the typed `vsock_handshake_rate_limited`
  event via `pi sandbox doctor` and surfaces it.
- **Warm-pool secret persistence.** A pooled VM that survives N tool
  calls retains its `vm_id`, per-VM secret, and OIDC token mount
  across calls. State leakage between calls is by design (it's how
  pooling earns its latency). Mitigation: §4 rotation policy bounds N
  (default 50 calls / 5 min); operators with stricter posture pin
  N=1 (cold-boot per call) at the cost of the §7 SLO.
- **Compromised broker / master rotation under HA.** Broker
  compromise leaks all per-VM secrets across the rotation window.
  This is the same blast radius as today's single-tenant master-on-disk;
  contextfs RFD-0017 (broker HA) is the host-side hardening, not in
  pi-rs's lap. Pi-rs's MicroVmProvider treats `BrokerOidcRejected`
  with a persistent (non-JWKS-rotation) cause as an operator alert.

### 7. Performance SLO

Per-tool-call overhead targets, measured as `host_observed_total_ms - guest_duration_ms`:

| Transport         | Cold boot | Warm pool acquire |
|-------------------|-----------|-------------------|
| Firecracker (Linux) | ≤ 400ms | ≤ 30ms          |
| vfkit (macOS)       | ≤ 800ms | (pool deferred) |
| cloud-hypervisor (Windows) | ≤ 1200ms | (pool deferred) |

A 20-tool-call coding turn under Linux+pool should add ≤ 600ms total vs `local-process`. If telemetry shows worse than 1.5× the SLO sustained, follow-up optimization is required before declaring v1.0 GA.

The `acquire_to_ready_ms` + `cold_boot` telemetry fields make the pool-hit-rate visible.

## Implementation schedule

Nine commits across four phases. Realistic LoC estimates (calibrated against aegis's 3486 LoC for Firecracker alone):

### Phase 1 — Foundations (no user-visible changes)

| # | Commit | Est. LoC |
|---|---|---|
| **A1** | Extract `pi-tool-types` crate + reroute pi-tools imports | 300 |
| **A2** | Split `pi-tools` into `pi-tools-core` (file/process) + `pi-tools-net` (web_search) | 300 |
| **A3** | New `pi-sandbox-protocol` crate: wire types, version constant, framing helpers | 250 |
| **A4** | New `pi-sandbox-worker` binary: vsock listener, JSON-line dispatch, calls into `pi-tools-core` | 350 |

### Phase 2 — Rootfs + downloadable artifact

| # | Commit | Est. LoC |
|---|---|---|
| **B** | `crates/pi-sandbox-rootfs/build.sh` + `RootfsCache` (download, sha256 verify, resume) + CI artifact publish | 400 + scripts |

### Phase 3 — Local launchers (gated; flag holds until all three land)

| # | Commit | Est. LoC |
|---|---|---|
| **C** | `MicroVmLauncher` trait, `VmHandle`, `VmSpec`, `VmCeiling`, `CallLimits`, `NetworkPolicy`, errors | 200 |
| **D** | `FirecrackerLauncher` + warm pool + integration test (gated on `PI_SANDBOX_FC_TEST`) | 1700 |
| **E** | `VfkitLauncher` (macOS) + integration test (gated on `PI_SANDBOX_VFKIT_TEST`) | 800 |
| **F** | `CloudHypervisorLauncher` (Windows) + integration test (gated on `PI_SANDBOX_CHV_TEST`) | 1000 |

### Phase 4 — Public surface

| # | Commit | Est. LoC |
|---|---|---|
| **G** | `MicroVmProvider`, CLI flag, `pi sandbox doctor`, end-to-end dogfood per OS | 600 |

**Total: ~5900 LoC across 9 commits.**

### Phased flag rollout (formalised)

The user-facing surface lands in stages so the maintainer's primary use case (Linux + Firecracker) ships without waiting on macOS + Windows runners:

- **Stage 1 (after Commit D merges):** `--sandbox-provider=microvm:firecracker` (explicit pin) goes live. Documented as "Linux only, beta." `pi sandbox doctor` works for the firecracker path. Maintainer dogfoods on Manjaro.
- **Stage 2 (after Commit E merges):** `--sandbox-provider=microvm:vfkit` (explicit pin) goes live. macOS users can dogfood.
- **Stage 3 (after Commit F merges):** `--sandbox-provider=microvm:cloud-hypervisor` goes live on Windows.
- **Stage 4 (Commit G + post-impl follow-ups):** `--sandbox-provider=microvm` (auto-pick) goes live with the cross-OS coverage promise. **Gating bar**: not just "all three launchers exist" — also requires symlink-resolve-beneath parity on macOS/vfkit and Windows/cloud-hypervisor (per §6 threat model: those launchers' v1 host-side path resolution does not check beneath-root for symlinks pre-placed in `/work`). Until that parity ships:
  - Linux/Firecracker `--sandbox-provider=microvm` and `microvm:firecracker` are GA.
  - macOS/vfkit and Windows/cloud-hypervisor remain **explicit-pin beta** (`--sandbox-provider=microvm:vfkit` / `:cloud-hypervisor`) with a stderr banner on first use noting the symlink-escape limitation. Plain `--sandbox-provider=microvm` on those OSes errors with a remediation message pointing to the explicit pin or RFD 0026 remote backends.
  - The auto-pick promise unlocks platform-by-platform once each platform's resolve-beneath lands. v1.1 (per `rfd/0023-sandbox-microvm.md` §6) closes the gap on vfkit; v1.2 on cloud-hypervisor.

This avoids the "Linux first, macOS later" anti-pattern (which the previous critic correctly flagged as user-hostile) while not blocking the Linux launch on Windows runner availability. Users get explicit pins as each lands; the auto-pick flag waits for the full matrix.

## Cross-OS CI

Honest about what GitHub-hosted runners can and cannot do:

- **Linux** — runners have no nested virt. Integration tests gated on `PI_SANDBOX_FC_TEST=1` and run on a **self-hosted runner** with KVM. Maintainer's Manjaro box is the bootstrap; longer-term we provision a dedicated runner.
- **macOS** — `macos-14` (Apple Silicon) runners support Hypervisor.framework. Integration tests run there.
- **Windows** — GitHub-hosted Windows runners do **not** reliably support WHPX nested virt. Two options:
  1. **Larger runners** (paid GitHub-hosted Azure SKUs that support nested virt). Budget required.
  2. **Self-hosted Windows runner** on bare metal.
- v1.0 ships option (2) for Windows. Option (1) is on the table if and when budget is approved.

The `Phase 3` commits ship integration tests gated on env vars; CI invokes the appropriate gate per platform. Tests not invocable in CI are validated on the maintainer's hardware before each release; `pi sandbox doctor` output from each release is captured into `release-notes/`.

## Out of scope / deferred

- **Remote backends (E2B, Sprites, Daytona)** — RFD 0026.
- **Snapshot / restore beyond simple pool.** v1.x.
- **Per-tool network policy.** A future RFD.
- **Custom rootfs per tool.**
- **Per-subagent pool isolation.** v1.0: by default subagents inherit the parent's `Arc<dyn SandboxProvider>` and **share its pool** (this is the existing pi-rs pattern from RFD 0005). The mutex around `VecDeque<WarmVm>` serializes acquire/release; the pool grows on demand if both parent and subagent need a VM concurrently. Operators who want strict isolation construct a fresh `MicroVmProvider` per subagent (memory cost: ~256MB × pool_size per extra provider) — this is the halo supervisor's choice, not the default.

## Open questions

1. **Cloud-hypervisor as unified launcher across all three OSes?** It supports KVM/Hypervisor.framework/WHPX in one binary. v1.0 picks best-of-breed (Firecracker on Linux, vfkit on macOS, cloud-hypervisor on Windows); revisit after one quarter of telemetry.
2. **Pool size default.** N=4 host-side, N=1 per subagent. Memory: 4 × 256MB = 1GB resident. Negotiable if users complain.
3. ~~**Tool selection for `pi-tools-core` (the guest-buildable subset).**~~ **DECIDED, v0.8/v0.9.** Guest-bound tools (route through the launcher → vsock → guest worker → pi-tools-core): `read`, `write`, `edit`, `bash`, `grep`, `find`, `ls`. Host-bound (run on the host directly via pi-tools-net): `web_search`. Unavailable under microvm: `monitor` (one-shot RPC vs. streaming mismatch). The split is the §"Tool availability under `microvm`" matrix, not a per-crate question.
4. **Default `--sandbox-provider` value after `microvm` ships GA.** Stays at `local-process` (no breaking change for existing users). Migration: docs + release notes + a one-shot first-run prompt suggesting `microvm` if all probes pass.
5. **Rootfs upgrade UX on pi binary upgrade.** Auto-download with progress bar (default) or refuse + prompt? **Auto-download** is the answer for v1.0; users on offline systems set `PI_SANDBOX_OFFLINE=1` to disable it (and the binary then refuses to use `microvm` until a manual `pi sandbox update`).
6. **Auth storage for remote backends** — moves to RFD 0026.
7. **What happens if the warm pool is exhausted under burst load?** Block on a free VM (with timeout), or boot ad-hoc? v1.0: ad-hoc cold boot, telemetry tracks the rate.

## Testing strategy

### Unit tests

- `pi-sandbox-protocol`: round-trip ser/de, version mismatch, payload size limits.
- `pi-sandbox/microvm/launcher`: trait mock, spec construction, error mapping.
- `pi-sandbox-worker`: tool dispatch (against pi-tools-core), error normalization.
- `pi-tools-core`: existing tests must pass after the type extraction.

### Integration tests (gated)

- `microvm_firecracker` (Linux + KVM, gated on `PI_SANDBOX_FC_TEST=1`):
  - Boot + ls a tmp dir.
  - Read + edit + verify host fs mutation through virtio-fs RW.
  - bash with cwd boundary check (path traversal rejected).
  - Pool warm-vs-cold timing.
  - Resource limit honored (OOM killed at mem cap).
- `microvm_vfkit` (macOS, gated): same coverage where supported.
- `microvm_chv` (Windows, gated): same coverage where supported.
- Negative: rootfs sha mismatch refuses to boot; guest-side tool error surfaces a clean `is_error: true` ToolResult.

### Dogfood (per phase)

- After Phase 3 + G: `pi --sandbox-provider=microvm:firecracker "ls + read + edit a marker file"` on Manjaro. Confirm session JSONL has the expected `provider="microvm"`, `launcher="firecracker"`, `dispatch_path="guest"`, `acquire_to_ready_ms`, and `cold_boot` telemetry fields, and the host file actually changed.
- Same dogfood on macos-14 + windows runner once D and F have landed.
- `pi --stats sandbox-actions` must show non-zero rows with the new `transport` column populated.

## Revision history

- **v0.24 (2026-05-04):** rfd-critic v0.23 pass: 3 criticals, 3
  underspec. Real ones closed. (1) Post-call hygiene mechanism
  promoted from "marker text in model_output" to first-class
  `ToolResponse.post_call_state: PostCallState` (`Clean` |
  `SuspectGuestState`); host composes
  `final_hint = min(host_hint, post_call_state)` before calling
  release. Missing field defaults to `SuspectGuestState` —
  workers must *prove* clean. `VmExecution` carries it through to
  the host. The verdict is a host/launcher signal, not surfaced
  to the model. (2) macOS / Windows v1: `VfkitLauncher` and
  `CloudHypervisorLauncher` are now normatively
  **destroy-on-release, no pooling**; the cgroup-based probe is
  Linux-only and pgrp-orphan can't catch `setsid()` daemonization
  reliably. Operators who accept the risk on those OSes can use
  `--sandbox-microvm-pool=force`; default is destroy. Future RFD
  lifts the restriction once each launcher has a proven-clean
  per-call container/cgroup analog. Linux/Firecracker keeps the
  pool. (3) Stale `transport` sweep round 2: `ProbeReport.transport`
  → `ProbeReport.launcher`; §"Tool availability" final paragraph
  `transport = "host-direct"` → `dispatch_path = "host-direct"`.
  Underspec items: (a) `VmExecution` now carries
  `post_call_state` so the runtime can propagate it; (b)
  cross-platform GA gate softened from "doesn't ship until all 3
  OS work" → "explicit launcher pins ship per-platform as each
  lands; unqualified auto-pick ships only after all three meet
  the GA bar"; (c) the critic's claim that
  `crates/pi-tools-core/src/monitor.rs` doesn't exist was a
  **review-environment false negative** — the file is on `main`
  (verified via `ls`); citation kept.
- **v0.23 (2026-05-04):** rfd-critic v0.22 pass found 2 critical
  + 3 underspec + a real citation. Both criticals real and closed.
  (1) **Pool hygiene proof.** v0.22 marked every `Ok(_)` and
  `CallLimit` as `Clean`, but a successful `bash 'sleep 999 &'`
  leaks a daemon into the warm pool. New §"Post-call hygiene"
  pins the worker contract: per-call cgroup-empty check (Linux)
  / process-group-orphan check (macOS/Windows fallback) +
  per-call temp-dir scrub. Either failing downgrades to
  `SuspectGuestState` regardless of the tool's success bit, and
  the launcher destroys the VM. Negative test matrix added:
  `sleep 999 &`, `nohup foo &`, deferred `(sleep 5; touch …) &`,
  `/tmp` residue, plus the timeout path. Each test asserts
  (a) no pool return, (b) next-acquire is clean, (c) telemetry
  records `outcome=SuspectGuestState`. Also added an explicit
  v1 statement: microvm v1 does **not** support cross-call
  background daemons. (2) **Citation rot.** A2 paragraph said
  `pi-tools-core = read/write/edit/bash/grep/find/ls`; verified
  on `main` that `crates/pi-tools-core/src/monitor.rs` exists.
  Updated to: `monitor` source-tree-present but **not registered
  in the guest worker** under v1 (one-shot RPC can't carry
  streaming output). The other claim — `rfd/0023-known-issues.md`
  doesn't exist — **was wrong**: file does exist; left in place.
  Underspec items (a–c) closed: (a) Telemetry alignment —
  schema migration text now lists 7 nullable columns
  (`launcher`, `dispatch_path`, `acquire_to_ready_ms`,
  `guest_duration_ms`, `cold_boot`, `cost_usd`, `round_trip_ms`),
  matching the `SandboxAction` struct; `× 7 new`, not `× 3 new`.
  (b) Single source of truth for host-bound classification:
  `tool_disposition()` defers to `host_tools.is_host_bound()`;
  documented + parity test at
  `crates/pi-sandbox/tests/host_bound_parity.rs`. (c) Host
  re-validation policy: drift logs `path_rewrite_drift` to
  telemetry and the host re-rewrites (registry is authoritative);
  unknown `display` shapes pass through with a
  `path_rewrite_unknown_shape` counter bump; the host never
  rejects the response (would lose tool work) and never fabricates
  a rewrite for an unknown shape.
- **v0.22 (2026-05-04):** rfd-critic v0.21 pass found 2 critical +
  3 underspec + stale-name sweep. Both criticals were real, not
  bikeshedding. (1) `ToolRequest` lacked any host-cwd field, so the
  worker couldn't actually inject `PWD=<host_cwd>` for bash even
  though §"Path virtualization" promised it. Added explicit
  `host_cwd: String` to `ToolRequest`, with documented uses (PWD
  injection + inverse-rewrite anchor; host re-validates on receipt).
  (2) `MicroVmProvider` pseudocode called `self.host_tools.is_host_bound(...)`
  / `.execute(...)` but the struct definition omitted the field and
  no trait existed. Added `host_tools: Arc<dyn HostBoundToolDispatcher>`
  to the struct, defined the `HostBoundToolDispatcher` trait
  (`is_host_bound` / `execute`) and `HostExecOutcome` shape; v1
  ships exactly one impl (`BuiltinHostTools`, `web_search` only).
  Underspec items (a–c) closed: (a) `SandboxExecution` is now
  guest-truthful (raw `/work` paths, NOT rewritten) — only
  `model_output` and `display` get inverse-rewritten; documented
  why this asymmetry is the right one (post-mortem fidelity vs
  model coherence). (b) Timeout hygiene before pool return: worker
  contract pinned — `SIGTERM` → 250ms drain → `SIGKILL` → emit
  `is_error=true, exit_status=124` ToolResponse → only then
  `Clean`-eligible; launcher escalates to hard-VM-kill +
  `SuspectGuestState` if the worker itself misses
  `wall_timeout + 1s`. (c) Path-rewrite registry tense corrected:
  RFD table is the normative spec **today**; Commit G adds
  `crates/pi-sandbox/src/path_rewrite/registry.rs` and from then on
  the registry is the source of truth. Stale-name sweep: §"Threat
  model" `ResourceLimits` → `VmCeiling`/`CallLimits`; §SLO/Dogfood
  `boot_ms` → `acquire_to_ready_ms`; §Dogfood `transport` →
  `provider`/`launcher`/`dispatch_path`; schedule §"Phase 3 commit C"
  `ResourceLimits` → `VmCeiling`/`CallLimits`; pseudocode
  `tool.is_runtime_native()` / `is_runtime_native_tool(...)` →
  `tool.dispatch_class()` / `tool_dispatch_class(...) != RuntimeNative`.
- **v0.21 (2026-05-04):** rfd-critic v0.20 pass found 1 critical +
  3 small. Critical: §2 (`VmExecution`) and §3 (wire `ToolResponse`)
  disagreed on what crosses the wire — §2 said the worker
  post-processes (so `tool_result.model_output` and raw
  `execution.stdout` may diverge), but the wire only carried raw
  `stdout`/`stderr`, with `ToolResult.model_output := stdout` direct.
  That breaks `bash` immediately, since today's `pi-tools-core/src/bash.rs`
  builds `model_output = "<stdout>\n<stderr>\n[exit N]"`. Fix:
  `ToolResponse` now carries an explicit `model_output: String`
  field alongside raw `stdout`/`stderr`/`exit_status` — the worker
  post-processes once (calls native tool, reads its `ToolResult`)
  and emits both: post-processed `model_output` for
  `ToolResult.model_output`, and raw streams for
  `SandboxOutcome.execution`. One JSONL string of overhead per call,
  full parity with local-process. Removed duplicate `ToolResponse`
  definition that v0.20 had left in place. Three small: (a)
  `ReleaseGuard::Drop` now uses `Handle::try_current()` and falls
  back to a synchronous `VmHandle::kill_blocking()` (default
  thread-spawn + one-shot runtime + 5s timeout) when no async
  context exists or runtime is shutting down. (b) Path-rewrite
  registry promoted from prose to a code-owned const map at
  `crates/pi-sandbox/src/path_rewrite/registry.rs` with a normative
  parity test against `LocalProcessProvider` fixtures. (c) Schedule
  header corrected: "Eight commits across three phases" → "Nine
  commits across four phases" (matches the table). Also dropped
  the stale `SandboxAction.stderr` reference (no such field exists
  in §2's schema).
- **v0.20 (2026-05-04):** rfd-critic v0.19 pass found 2 critical
  issues, both real, both closed. (1) **Path-validation primitive
  was wrong**: spec said `ToolError::InvalidPath` (which doesn't
  exist on `main` — `pi-tool-types` only has `NotFound`,
  `InvalidInput`, `Io`, `Other`) and used full `canonicalize`,
  which breaks `write` to new files. Replaced with
  `resolve_beneath(host_cwd, requested, allow_missing_leaf)`:
  lexical jail walk, deepest-existing-ancestor canonicalisation,
  optional missing-leaf for `write`. Errors raised via existing
  `ToolError::InvalidInput` (no semver/wire bump). (2)
  **`ToolResponse ↔ ToolResult` mapping was lossy**: spec said
  `display = None` in v1 and that "no pi-tools-core produces
  display"; verified false (read/write/edit/grep/find/ls/bash all
  emit `display: Some(...)` at `crates/pi-tools-core/src/*.rs`).
  Wire `ToolResponse` now carries `display: Option<serde_json::Value>`;
  host-side mapping inverse-rewrites paths in **both** `model_output`
  and `display` (per-tool key list); image `read` `display.path`
  is rewritten while `display.base64` is passed verbatim. Per-tool
  rewrite table updated: `write`/`edit` success messages get
  inverse rewrite; bash output rewrite direction corrected
  (`/work` → `<host_cwd>`, not the reverse). Two underspec items
  also closed: (a) `Cancelled` outcome is now reachable —
  `ReleaseGuard::Drop` spawns a detached release with
  `ExecuteOutcomeHint::Cancelled` if the future is dropped between
  acquire and explicit release; (b) `PWD=<host_cwd>` claim
  downgraded from "makes pwd/realpath behave host-style" to
  "best-effort: shell builtins read $PWD, but `pwd -P`,
  `readlink /proc/self/cwd`, `realpath .` bypass it; output
  rewriting catches most leaks but isn't airtight". `6 calls`
  finding from critic was a misread of the prompt's tool-call
  cap; body still says 50 calls / 5 min and is internally
  consistent.
- **v0.19 (2026-05-04):** rfd-critic v0.18 pass found 2 critical
  issues. (1) Genuinely substantive missing piece: **path
  virtualization**. The agent's tools accept absolute host paths
  like `/home/<user>/proj/src/lib.rs`, but inside the guest only
  `/work` exists. New §"Path virtualization" subsection added
  before §"Filesystem semantics": per-tool host↔guest path rewrite
  table, canonicalization rules (resolve-symlinks + beneath-`<host_cwd>`
  enforced BEFORE rewrite), explicit model-visible cwd contract
  (still the host path, never `/work`), and the `bash` contract
  (cwd parameter rewritten; command string best-effort
  substring-rewritten with word-boundary to avoid clobbering;
  PWD env injected so `pwd` returns host path inside the guest).
  (2) `MicroVmProvider::execute_tool()` sample now always releases
  the VM (no more silent `vm.release().await.ok()` only on success
  path) with an explicit `ExecuteOutcomeHint` derived from the
  execute result: `Clean` → return to pool (incl. CallLimit
  timeouts since the guest's still consistent); other errors →
  `SuspectGuestState` → destroy.
- **v0.18 (2026-05-04):** rfd-critic v0.17 pass found 3 critical
  issues. All small, all closed. (1) `ToolResult.display` corrected
  to `Option<serde_json::Value>` (the actual shape on `main`,
  `crates/pi-tool-types/src/lib.rs:21-31`); microvm guest tools
  emit `display = None` in v1. (2) `VmHandle::execute()` now
  returns `Result<_, ExecuteError>` (consistent with the typed
  taxonomy introduced v0.17, no longer collapses into `SandboxError`
  one layer too early). `release()` is best-effort + takes an
  `ExecuteOutcomeHint` so the launcher can route Clean →
  return-to-pool, SuspectGuestState → destroy (pool hygiene rule).
  (3) `NetworkPolicy` gains `PartialEq, Eq, Hash` derives so
  `BootSpec` actually compiles; `TransportMode` (referenced in the
  `BootSpec` key) is now defined inline (`Local` / `Managed`).
- **v0.17 (2026-05-04):** rfd-critic v0.16 pass found 4 critical
  issues. All real, all closed. (1) Background §"pi-tools dependency
  problem" updated: A1 + A2 are MERGED on main (per task list #33,
  #40); cited the actual current Cargo.toml shape and reframed the
  remaining A-series as "extracting Tool / ToolContext from
  pi-ai-touching machinery", not "split pi-tools". (2)
  `ToolDispatchClass` placement: keep in `pi-tool-types` (it's a POD
  enum, fine there), but `dispatch_class()` method lives on the
  existing `Tool` trait in `pi-tools` (the trait crate that already
  exposes `Tool::name()` etc.). pi-tool-types stays POD-only.
  (3) Error type taxonomy: introduced `ProbeError`, `AcquireError`,
  `ExecuteError`, and `SandboxError` that wraps them with typed
  `#[source]` chaining. The launcher's `acquire()` returns
  `Result<_, AcquireError>`; provider wraps into `SandboxError`. The
  duplicate `pub enum SandboxError` in §2 collapsed to one
  definition. (4) `ToolResult.display` corrected to
  `serde_json::Value` (matches actual `pi-tool-types/src/lib.rs`),
  not `Option<String>`. Microvm guest tools emit
  `display: serde_json::Value::Null` in v1.
- **v0.16 (2026-05-04):** rfd-critic v0.15 pass: closed the one
  substantive remaining issue (safety default contradiction). The
  trait `tool_disposition()` default is now `Unavailable`, not
  `Guest` — the safe choice. `LocalProcessProvider` overrides to
  return `Guest` iff the tool is in its `ToolRegistry`;
  `MicroVmProvider` overrides with an explicit whitelist. Future
  tools that no provider has been updated to handle are silently
  hidden from the model rather than silently advertised-and-broken.
  Citation-pinning (replace sibling-repo absolute paths with
  commit-pinned permalinks) is acknowledged as a publish-readiness
  task — those references currently work for the maintainer's local
  cross-team review but need pinning before the RFD bumps state to
  `published`.
- **v0.15 (2026-05-04):** rfd-critic v0.14 pass found 1 critical
  issue (transport error mid-output truncated the rest, but the
  surfaced finding was substantive). Closed it. (1) Plan-time tool
  advertisement: today the model gets the FULL tool list at session
  start — `monitor`/`lsp` would appear advertised under microvm
  even though they're runtime-rejected, and RFD 0005 subagents
  configured with `lsp` in their allowlist would crash mid-task.
  Added §"Plan-time advertisement": new `SandboxProvider::tool_disposition()`
  capability-query API + runtime startup filter that strips
  `Unavailable` tools from the advertised list before the model
  plans. `task` executor's behavior on an agent-defined allowlist
  containing an unavailable tool: strip-with-warning by default
  (one stderr banner + a `tool_filtered_out` session-JSONL event);
  fail-fast opt-in via `[task] on_unavailable_tool = "fail-fast"`.
  Both `tool_disposition()` and the runtime filter are NEW APIs
  Commit G adds; `LocalProcessProvider`'s default returns `Guest`
  for everything (behavior unchanged).
- **v0.14 (2026-05-04):** rfd-critic v0.13 pass found 2 critical
  issues. Both real (and a class I'd been hand-waving). Closed both.
  (1) `lsp` was claimed host-direct via "pi-tools-net's LSP backend
  (RFD 0007)" — that backend doesn't exist and the actual `lsp` tool
  lives ABOVE `pi-sandbox` in the dep graph (`crates/pi-coding-agent/
  src/native/lsp/tool.rs`). Reclassified as **`unavailable` in
  microvm v1**: returns `ToolUnavailable` with a startup-time stderr
  banner, `LspWriteTool` write-decoration is similarly unavailable
  under microvm. A future RFD can re-home `lsp` into a lower crate
  and reclassify; not in Commit G's scope. The host-direct registry
  hardcodes `web_search` only. (2) `tool.is_runtime_native()` was
  cited as if it existed — it doesn't. Replaced with a normative
  `ToolDispatchClass` API that Commit G adds: `RuntimeNative` vs
  `SandboxManaged`, with the runtime branch at `runtime.rs:1677`
  spelled out. `task` overrides `dispatch_class()` to return
  `RuntimeNative`. The §"Tool dispatch boundary" subsection now
  explicitly marks itself a NEW API (Commit G) rather than describing
  an existing primitive.
- **v0.13 (2026-05-04):** rfd-critic v0.12 pass found 3 critical
  issues. Closed all of them. (1) `tool_use_id` is now an explicit
  parameter on `SandboxProvider::execute_tool(ctx, tool_use_id,
  tool_name, tool_input)`; the runtime threads the outer
  `ToolCall.call_id` through. RFD 0022's trait gains a non-default
  `tool_use_id: &str` parameter. The §3 mapping table updated to
  describe the explicit threading. (2) §"Tool availability" is now
  a full matrix covering all currently shipped tools (read/write/
  edit/bash/grep/find/ls/web_search/monitor/lsp), each with a
  disposition (`guest` / `host-direct` / `unavailable`) and rationale.
  `lsp` joins `web_search` as host-direct (LSP servers are
  glibc-heavy; running them in the busybox guest is impractical, and
  they query workspace state through the same FUSE/contextfs mount
  the agent uses). The host-direct registry is hardcoded to
  {web_search, lsp} in v1, no operator-extensible registration.
  (3) `firecracker.rs:643` cite double-checked: `let vm_id =
  Uuid::new_v4().to_string();` IS at line 643 today. The v0.12
  critic's "wrong" reading appears to have been a sandbox limitation;
  cite kept and a `git blame` reference added in §3.5.1 prose.
- **v0.12 (2026-05-04):** rfd-critic v0.11 pass found 3 critical
  issues. Closed all of them. (1) Pool partition normatively keyed:
  `tokio::sync::Mutex<HashMap<BootSpec, VecDeque<WarmVm>>>` instead
  of the un-keyed `Mutex<VecDeque<WarmVm>>`. Acquire/release/refill
  text in §2 and §4.1 updated; added empty-ring GC after idle TTL
  so long-running cwd-drifting sessions don't leak ring entries.
  (2) Stripped the `microvm:wsl2` reference from §4.3 (Windows
  launcher text) and §5 (CLI examples). The revision-history claim
  matched the v0.10 prose, but the body still showed it; now actually
  removed. WSL2 acknowledgement reframed as "considered, cut from
  v1, document why." (3) §"Phased flag rollout" Stage 4 GA bar
  tightened: not just "all three launchers exist" — also requires
  symlink-resolve-beneath parity on macOS/vfkit and Windows/cloud-
  hypervisor (the §6 limitation we documented in v0.11). Until
  parity ships: Linux/Firecracker is GA; macOS/vfkit and
  Windows/cloud-hypervisor are explicit-pin beta with a stderr
  banner; plain `--sandbox-provider=microvm` errors on those OSes
  pointing the operator at the explicit pin. v1.1 closes vfkit;
  v1.2 closes cloud-hypervisor.
- **v0.11 (2026-05-04):** rfd-critic v0.10 pass found 3 critical
  issues. Closed all of them. (1) New §"Tool dispatch boundary"
  subsection: runtime-native orchestration tools (`task` from
  RFD 0005, future `apply_plan`/`evolve_tick` etc.) bypass
  `SandboxProvider::execute_tool` BEFORE provider dispatch. The
  runtime checks `tool.is_runtime_native()` first; SandboxProvider
  only sees guest-buildable tools + host-direct exceptions
  (`web_search`). The provider sample asserts this with a
  `debug_assert!`. (2) §6 threat-model claim about virtio-fs
  cwd-confinement softened to the truth: on Linux/Firecracker (with
  `cfs-fs-server --backend-root`) we DO get symlink-resolve-beneath;
  on macOS/vfkit and Windows/cloud-hypervisor we do NOT — a symlink
  pre-placed in `/work` pointing outside is followed by virtio-fs.
  Documented as a v1 limitation; v1.1 adds resolve-beneath in the
  worker on those platforms. (3) §6 "busybox userland" claim flipped:
  the guest rootfs SHIPS real Bash (statically-linked
  `bash-static-musl`, ~1.5 MiB) so the `bash` tool's contract is
  preserved across providers. Only the coreutils-like commands
  (`find`/`grep`/`ls`/etc.) remain busybox; the known-incompat list
  shrinks to those.
- **v0.10 (2026-05-04):** rfd-critic v0.9 pass found 3 critical issues
  + sweep gaps. Closed all of them. (1) `SandboxOutcome` is now defined
  ONCE: `{ tool_result, execution, telemetry }`. The earlier
  associated-type / dual-definition story is gone — every provider
  (LocalProcessProvider, MicroVmProvider, remote backends) returns the
  same envelope. LocalProcessProvider wraps its existing
  `SandboxExecution` into the unified shape via a one-line helper.
  (2) `SessionEntryKind::SandboxAction` schema block now matches the
  v0.9 three-field split: `launcher` / `dispatch_path` /
  `acquire_to_ready_ms` / `cold_boot` / `cost_usd` / `round_trip_ms`,
  with explicit JSONL backward-compat semantics (`#[serde(default)]`
  on the new fields) and SQLite migration ordering rules
  (`ALTER TABLE` before any new-binary ingest, schema-version refusal
  otherwise). New integration test `sandbox_action_compat.rs` pins it.
  (3) Sweep: `pi sandbox doctor` example output, FirecrackerLauncher
  probe checklist, and per-VM share descriptions all now reference
  contextfs binaries on Linux (no `virtiofsd` mention). Open Question
  3 marked DECIDED with the §"Tool availability" matrix as the
  resolution. The §"web_search" host-bound dispatcher is hardcoded
  to `web_search` in v1 (not operator-extensible) per critic's
  overengineering feedback. The earlier `microvm:wsl2` reference was
  cut from v1 (was vestigial; not specced).
- **v0.9 (2026-05-04):** rfd-critic v0.8 pass found 2 critical issues
  remaining + naming-overload + stale text. Closed all of them.
  (1) §3.5 now opens with an **availability matrix** (OS × launcher ×
  mode × `/work` transport) that makes the asymmetry explicit:
  Linux/Firecracker is `managed`-only in v1; macOS/vfkit and
  Windows/cloud-hypervisor are `local`-only in v1. Linux/Firecracker
  has no `local`-mode `/work` transport in v1 because Firecracker
  ships no virtio-fs (per `rfd/0023-known-issues.md`). Operators
  who explicit-pin `--sandbox-microvm-mode=local` on Linux get a
  hard-error at provider construction with a concrete remediation
  message. macOS/Windows are `local`-only because contextfs's vsock
  is Linux-only. (2) §2 `VmExecution` now carries BOTH `tool_result`
  (model-facing) AND `execution` (raw stdout/stderr/exit_status);
  earlier the sample provider code read `result.stderr` /
  `exit_status` from a `ToolResult` that had no such fields. The
  host-bound dispatch path returns the same pair. (3) Telemetry
  field `transport` was overloaded across three meanings; split into
  `provider` (`microvm` | `local-process` | remote backend names),
  `launcher` (`firecracker` | `vfkit` | `cloud-hypervisor`), and
  `dispatch_path` (`guest` | `host-direct`). The `SessionEntryKind::SandboxAction`
  schema gets the same three fields. Sample provider code updated.
  (4) `SandboxOutcome` extended to carry both the model-facing
  `tool_result` and the raw `execution`. The trait return is now
  `Result<SandboxOutcome, SandboxError>`.
- **v0.8 (2026-05-04):** rfd-critic v0.7 pass found 5 critical issues +
  1 citation rot. Closed all of them. (1) §3.5 split into two
  deployment modes: **`local`** (default, single-user CLI, no
  contextfs/broker/OIDC) and **`managed`** (opt-in, operator-deployed).
  Ordinary local-CLI users typing `pi --sandbox-provider=microvm`
  get `local` mode; the contextfs stack is now opt-in via
  `--sandbox-microvm-mode=managed`. (2) §2 introduced explicit
  `BootSpec` pool partition key (`canonical_host_cwd` + `env_hash` +
  `network_policy` + `vm_ceiling` + `rootfs_version` +
  `transport_mode`), preserving RFD 0006 worktree isolation —
  previously a warm VM could be reused across different `host_cwd`s,
  silently sharing one subagent's workspace with another. (3) §2
  provider contract now returns `SandboxOutcome { execution,
  telemetry }` so `acquire_to_ready_ms` / `cold_boot` / `transport` /
  real `stderr` + `exit_status` actually reach the
  `SessionEntryKind::SandboxAction` row. (4) §3.5.9 failure-sentinel
  path moved off the (non-existent) Firecracker virtio-fs share onto
  the serial console (`/dev/ttyS0`) — `pi-cfs-init` writes
  `PI_FAIL: <reason>` lines that the host parses from the captured
  serial-console stdout. (5) New §"Tool availability under microvm"
  adds an explicit `web_search` exclusion decision parallel to the
  existing `monitor` decision (both return typed `ToolUnavailable`,
  not silently hidden). Citation fix: dropped the wrong
  `local.rs:62-67` quote (replaced with a non-citing description of
  the same behavior). The contextfs integration sub-sections
  (3.5.1-3.5.9) are now scoped under `managed` mode and their
  applicability is conditional, addressing the v0.7 finding that
  "Linux v1 is no longer self-contained."
- **v0.7 (2026-05-04):** rfd-critic v0.6 pass found 3 critical issues +
  1 citation rot. Closed all of them. (1) §3.5.5 now documents both
  control-plane (broker) AND data-plane (cfs-fs-server) channels with
  explicit per-VM run-dir UDSes, two cfs-mesh bridge/listen pairs,
  4 keypairs per VM, and per-VM port allocation (P_b / P_f). §3.5.6
  drops the wrong `--auth-secret-path` (TCP-HMAC flag) for UDS
  topology and adds `--listen-uds` + `--tenant-peer-uid` + dedicated
  `pi-sandbox-bridge` system uid for SO_PEERCRED. (2) §3.5.9 now
  introduces `pi-cfs-init` — a NEW pi-rs-owned static-musl binary
  (~200 LoC, in Commit G's scope) that orchestrates rootfs init,
  spawns both `cfs-mesh vsock-listen`s + `contextfsd --config`,
  polls `stat("/work")` for FUSE readiness, owns the `/work/.cfs-ready`
  sentinel (we no longer falsely attribute readiness behavior to
  stock contextfsd), and re-aps zombies. Fixed the `contextfsd /etc/...`
  spelling to `contextfsd --config /etc/...`. Failure surface table
  now uses typed `AcquireError` variants. Schema-drift detection
  scoped to deserialize-time integration test (gated on
  `CONTEXTFS_REPO_PATH`), not the unrealistic compile-time-error claim.
  (3) Swept §1 (load-bearing bullet 4 now scopes virtio-fs to
  macOS/Windows v1.0; Linux Stage 1 is contextfs), §3 init contract
  (`/work` mounting now branches per-OS), §"Filesystem semantics"
  (transport split per-OS clearly stated, `virtiofsd` not required on
  Linux Stage 1), §4.1 (per-VM share now lists contextfs services on
  Linux Stage 1; virtio-fs explicitly the Linux-fallback / non-Linux
  path), §5 doctor probes (functional connectivity checks for the
  contextfs binaries; virtiofsd only on the fallback path), §6 threat
  model (cwd-boundary enforcement uses `cfs-fs-server --backend-root`
  on Linux Stage 1). Citation fix: dropped the wrong RFD 0022
  §"Per-tenant config overlays" cite (replaced with "Commit G
  deliverable: per-tenant audit_ping defaults in MicroVmProviderConfig
  with orchestrator overrides").
- **v0.6 (2026-05-04):** rfd-critic v0.5 pass found 5 critical issues +
  2 citation rots. Closed all of them. (1) §3.5 now scopes contextfs
  to **Linux/Firecracker Stage 1 only**; macOS/Windows v1.0 keeps the
  §3 virtio-fs RW share at `/work` and a follow-up RFD brings contextfs
  to those platforms once contextfs supports a non-vsock embedder
  transport. (2) §3.5 now states explicitly that on Linux contextfs
  **supersedes** the §3 virtio-fs RW share for the workspace path —
  the worker's cwd `/work` is a contextfs FUSE mount, not a virtio-fs
  share. (3) §3.5.7's TOML rewritten against contextfsd's actual
  config shape (`[[mount]]` not `[mounts.<name>]`, `backend = "remote-fs"`
  + `[mount.remote_fs] target_uds`, mandatory `audit_log_path` /
  `[pdp]` / `cache_dir`). (4) §3.5.5 picks one transport topology end-
  to-end: host runs broker on UDS + `cfs-mesh vsock-bridge` to the
  guest's vsock listener; broker has no `--vsock*` flags. The
  guest-side `cfs-mesh vsock-listen` re-exposes the host broker as a
  guest-local UDS that contextfsd consumes. (5) New §3.5.9 Guest boot
  contract: rootfs additions table (contextfsd + cfs-mesh binaries,
  policy.cedar, tmpfs cache/log), required kernel features
  (`CONFIG_FUSE_FS`, `CONFIG_VSOCKETS`, `CONFIG_VIRTIO_VSOCKETS`,
  `/dev/fuse`), `init` script's startup ordering with explicit
  readiness gates and the `/work/.cfs-ready` rendezvous file, typed
  failure surface visible to MicroVmProvider. Citation fixes: dropped
  the wrong "RFD 0021 §Open Q3" cite (replaced with "operator-supplied
  path; orchestrator owns it") and the unverifiable
  "/version endpoint (RFD 0022 §A)" cite (replaced with synthetic
  verify_write reachability check). Added `pi sandbox doctor`
  functional probes (no foreign-argv inspection). New §3.5.2 explicit
  encoding: `contextfs-cli key derive-per-vm-secret` writes raw 32
  bytes to stdout (matching the in-tree `tenant_secret` format).
  Added §3.5.1 lifetime note (`vm_id` survives pool reuse, so audit
  attribution is per-pooled-VM not per-call) and §3.5.2 pool refresh
  policy on `BrokerMasterEpochTooOld`. Added §6 threat-model bullets
  for guest-resident secret/token, audit-ping loss window, vsock
  handshake DoS, warm-pool secret persistence, broker compromise/HA.
- **v0.5 (2026-05-04):** Added §3.5 ContextFS integration. Originally
  the RFD said nothing about how the in-guest contextfsd is configured
  at provisioning, treating it as "just sits in the rootfs." That
  silently begged questions that contextfs's RFD-0023 (their numbering)
  shipped in published form between 2026-05-03 and 2026-05-04: per-VM
  tenant secret derivation, OIDC token plumbing, audit-ping per-mount
  config, broker invocation flags, version compat. §3.5 documents each
  field pi-rs renders into the daemon TOML at provisioning, the
  host-side `contextfs-cli key derive-per-vm-secret` invocation, the
  Workload-Identity token mount path, the typed OIDC denial-routing
  contract, and the broker-side `--tenant-mode <t>=embedder` requirement.
  Pinned contextfs broker version to `>= v0.3.0` (the version that
  shipped `vm_id`/`master_epoch`/`oidc_token` over the wire). No
  changes to §1-§2 architecture or §4 launcher impls; the integration
  delta is purely in the runtime provisioning path Commit G owns.
- **v0.4 (2026-05-02):** In-repo `rfd-critic` (gpt-5.4 xhigh) pass. Closed three P0 deltas the external Plan-agent missed: (1) added explicit `ToolResponse↔ToolResult` field-mapping table in §3 (previously lossy — `tool_use_id`/`display` were unspecified); (2) clarified pool-ownership rule (launcher owns the pool; subagents inheriting `Arc<dyn SandboxProvider>` SHARE it — corrected v0.2's wrong OQ#2 claim); (3) showed `CallLimits` construction site in `MicroVmProvider::execute_tool` pseudocode. Also: declared the worker's transport-agnostic design as RFD 0023's responsibility (so RFD 0026 owns the remote-side strategy, not the worker design). Picked the SandboxAction telemetry schema decision: one union struct with all-optional new fields (amends RFD 0022 non-breakingly). Added `ToolUnavailable` error variant for the `monitor` exclusion path.
- **v0.3 (2026-05-02):** Second Plan-agent critique pass. **Decided `monitor` exclusion** from microvm — protocol stays one-shot RPC, `monitor` returns clean error in microvm mode (telemetry decides whether v1.1 adds streaming). Split `ResourceLimits` into per-VM `VmCeiling` and per-call `CallLimits` (the previous design was wrong with pooling). Renamed `boot_ms` → `acquire_to_ready_ms` to be honest about what's measurable. Deepened probe to actually exercise preconditions (`/dev/kvm` open RW, vsock module, virtiofsd, AppArmor) with per-check results in `ProbeReport.checks`. Reduced default pool from N=4 to N=2 (real coding sessions are sequential; pool=2 covers one parallel subagent burst at ~512MB resident). Added explicit "rootfs contents are public knowledge" note to threat model + busybox-not-GNU compatibility note. Formalised the phased flag rollout (`microvm:firecracker` ships Stage 1; auto-pick waits for full matrix). Acknowledged `pi-tool-types` becomes a stable public ABI by virtue of being on the wire AND in the host tool API.
- **v0.2 (2026-05-02):** Restructured after Plan-agent critical review. Promoted writable virtio-fs and Firecracker pooling to v1.0. Added concrete `MicroVmLauncher` / `VmHandle` / `VmSpec` Rust signatures. Added pi-tools dependency-extraction pre-work (Commits A1/A2). Split remote backends out to RFD 0026. Added explicit threat model and performance SLO. Pinned CI matrix to self-hosted Linux/Windows runners, GitHub-hosted macos-14.
- **v0.1 (2026-05-02):** Initial draft covering local microVM + remote backends.

## References

- **RFD 0022** — Sandbox Execution for Tool Decisions.
- **RFD 0026** (sister) — Remote Sandbox Transports (E2B, Sprites, Daytona).
- **aegis-detonation** (`/home/nemesis/code/aegis/aegis-detonation/`) — Firecracker reference impl with vsock/snapshot/hostd patterns.
- **Firecracker** — https://firecracker-microvm.github.io/.
- **vfkit** — https://github.com/crc-org/vfkit.
- **cloud-hypervisor** — https://www.cloudhypervisor.org/.
- **virtio-fs** — https://virtio-fs.gitlab.io/.
- **virtio-vsock** — kernel docs.
- **contextfs RFD-0023** (`/home/nemesis/code/contextfs/rfds/0023-embedder-profile.md`,
  state: published, target broker version `>= v0.3.0`) — the
  contextfs-side spec that pi-rs Commit G integrates against. Their §1
  (library entry), §2 (transport-generic mesh), §3 (listener-generic
  cfs-fs-server), §4 (vsock subcommands), §5 (per-VM tenant_secret),
  §6 (verify_write OIDC validator), §7 (audit-ping stub) are the seven
  load-bearing surfaces our §3.5 consumes.
