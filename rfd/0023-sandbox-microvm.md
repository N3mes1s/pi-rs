# RFD 0023 — Local MicroVM Sandbox (Linux/macOS/Windows)

- **Status:** Discussion (v0.5)
- **Author:** pi-rs maintainers
- **Created:** 2026-05-02
- **Implemented:** (pending)

## Summary

RFD 0022 shipped the `SandboxProvider` trait but only a passthrough `LocalProcessProvider` whose own docstring admits it doesn't isolate anything: `tool.invoke()` runs inline, same fs / same UID / same network. The wiring is correct, the contents are vapor. End-to-end dogfood produced `duration_ms: 0` per call — proof nothing forks.

This RFD ships the first **real isolation backend**: a `MicroVmProvider` that runs each tool call inside a Linux microVM, with one rootfs and one wire protocol shared by per-OS launchers (Firecracker on Linux, vfkit on macOS, cloud-hypervisor+WHPX on Windows). The user-facing CLI flag (`--sandbox-provider=microvm`) does **not** ship until all three OS paths work in the same release.

Remote sandbox vendors (E2B, Sprites, Daytona) are split into a sister RFD — see **RFD 0026** — because they share only the `SandboxProvider` surface, none of the rootfs/protocol/launcher infrastructure.

### What's load-bearing

1. **One Linux guest rootfs** (alpine, ~50–80 MB compressed) hosting `pi-sandbox-worker`. Same artifact on every host OS.
2. **One vsock JSON-line wire protocol**, version-negotiated.
3. **Three `MicroVmLauncher` impls** under one trait, each `#[cfg]`-gated to its OS.
4. **Read–write virtio-fs** of the user's cwd. Read-only is **not** acceptable; the agent's two highest-frequency tools (`write`, `edit`) require host fs mutation. v1.0 ships writable.
5. **Per-launcher pooling** required for `FirecrackerLauncher` in v1.0 (a real coding turn calls 20+ tools; cold-boot-per-call burns 5–10s of perceived "sandbox tax" per turn, sending users back to `local-process`). Other launchers may no-op pooling initially.

### What's deliberately deferred (with reasons)

- **Remote backends** — RFD 0026.
- **Snapshot / restore** beyond simple pooling — v1.0 keeps a warm pool of N pre-booted VMs; snapshot/restore is a v1.x optimization.
- **Per-tool network policy** — guests have no network in v1.0. A future RFD adds selective egress.
- **Custom rootfs per tool** — one rootfs serves all tools in v1.0.

## Background

### What RFD 0022 left vapor

`crates/pi-sandbox/src/local.rs:62-67`:

> the LocalProcessProvider does not create a tmpdir in the MVP (all file tools operate on the same cwd as the inline path). This is intentional: tmpdir isolation is deferred to the subprocess variant (future commit).

That subprocess variant never landed. Today every "sandboxed" tool call is a function call in the agent's own address space. Time to ship the substance.

### The pi-tools dependency problem (the silent killer)

A guest worker that runs pi's tools cannot link `pi-ai` (the LLM-provider crate). It also cannot link `reqwest` (network) or `tokio::net::TcpListener` etc. — the guest has no network, no LLM creds, no DNS. But every file in `crates/pi-tools/src/*.rs` today does:

```rust
use pi_ai::{ToolResult, ToolSpec};
```

Audit of `pi-tools/Cargo.toml` shows `pi-ai.workspace = true` and `reqwest.workspace = true` are unconditional. The host build pulls in the whole world.

**Resolution (Commits A1/A2):** extract the POD types `ToolResult` and `ToolSpec` (and `ToolError`) into a tiny new crate `pi-tool-types` with no transitive deps beyond `serde`/`serde_json`/`thiserror`. `pi-ai` re-exports them from there for backward compatibility. `pi-tools` switches its imports to `pi_tool_types::*`. Then split `pi-tools` into `pi-tools-core` (read/write/edit/bash/grep/find/ls — file + process only) and `pi-tools-net` (web_search). The guest worker depends on `pi-tool-types` + `pi-tools-core` + `pi-sandbox-protocol`. Compiles statically against musl, links into a ~6–8 MB binary, fits in alpine.

Estimated impact: ~600 LoC moved, no behavior change. Fully reversible.

**`pi-tool-types` becomes a stable public ABI** by virtue of being on the wire protocol AND in the host-side tool API. Field additions to `ToolResult`/`ToolSpec` after Commit A1 are breaking changes that must bump the crate's MAJOR version and the wire-protocol version in lockstep. Acknowledged here so future-us doesn't blunder.

### The `monitor` tool exclusion (decided)

`pi-tools::monitor` spawns a long-running observer that streams partial output to the agent. The v1 wire protocol in §3 is one `ToolRequest` → one `ToolResponse`, JSON-line-framed, single round trip. **Streaming is incompatible with the v1 protocol shape.** Two paths considered:

- **(a) Add streaming responses to v1.** Adds significant complexity to the host and worker (state machine for partial messages, EOF detection, cancellation), and `monitor` is the only consumer.
- **(b) Exclude `monitor` from `pi-tools-core`.** It stays in pi-tools but is not reachable through `microvm`. A microvm-mode session that calls `monitor` returns a clean error (`tool not available under --sandbox-provider=microvm; use --sandbox-provider=local-process or RFD 0026 remote backends`).

**Decision: (b).** Telemetry from existing pi sessions can quantify monitor usage; if it's > 5% of tool calls in real coding sessions, v1.1 of this RFD adds streaming responses. Until then, the protocol stays one-shot.

This decision is upstream of Commit A3 (the protocol crate). It must land here, not deferred.

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
    async fn probe(&self) -> Result<ProbeReport, SandboxError>;

    /// Acquire a VM ready to execute a tool call. v1.0 launchers
    /// MAY return a pooled+warm-restored VM (FirecrackerLauncher
    /// MUST); others may cold-boot.
    async fn acquire(&self, spec: &VmSpec) -> Result<Box<dyn VmHandle>, SandboxError>;
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

#[derive(Debug, Clone, Copy)]
pub enum NetworkPolicy {
    Deny,
    // Future: AllowList(Vec<DomainPattern>), AllowAll
}

/// VM-level ceiling. Set at acquire(); cannot change without
/// rebooting the VM. Pool partitioning is keyed by this so a
/// pool acquire returns a VM whose ceiling matches the request.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct VmCeiling {
    pub mem_mib: u32,        // default 512 (host budget per VM)
    pub vcpus: u8,           // default 2
    pub disk_mib: u32,       // ephemeral overlay; default 256
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
    ) -> Result<VmExecution, SandboxError>;

    /// Release the VM. v1.0 in pooled mode = return to pool;
    /// non-pooled = shutdown.
    async fn release(self: Box<Self>) -> Result<(), SandboxError>;
}

pub struct VmExecution {
    pub result: ToolResult,        // shape compatible with inline path
    pub guest_duration_ms: u32,    // measured INSIDE the guest
    /// Time from `acquire()` to the moment the host's vsock
    /// connection to the guest succeeded. NOT pure boot time —
    /// includes guest init, vsock listen, accept handshake. The
    /// host can't see "boot finished" without guest cooperation,
    /// so this is the most honest end-to-end measurement.
    pub acquire_to_ready_ms: u32,
    /// True when this acquire required a cold boot (pool miss).
    pub cold_boot: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct ProbeReport {
    pub transport: &'static str,
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

#[derive(Debug, thiserror::Error)]
pub enum SandboxError {
    #[error("microvm unavailable: {0}")]
    Unavailable(String),
    #[error("guest tool error: {0}")]
    Tool(#[from] ToolError),
    #[error("vsock io: {0}")]
    Vsock(String),
    #[error("rootfs version mismatch: expected {expected}, got {found}")]
    RootfsMismatch { expected: u32, found: u32 },
    #[error("tool '{tool}' unavailable in sandbox: {reason}")]
    ToolUnavailable { tool: String, reason: &'static str },
    #[error("timeout after {0:?}")]
    Timeout(Duration),
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
}
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
        tool_name: &str,
        tool_input: &serde_json::Value,
    ) -> Result<SandboxExecution, SandboxError> {
        // monitor is excluded under microvm — see §"The monitor tool exclusion".
        if tool_name == "monitor" {
            return Err(SandboxError::ToolUnavailable {
                tool: tool_name.into(),
                reason: "tool not available under --sandbox-provider=microvm; \
                         use --sandbox-provider=local-process or RFD 0026 remote backends",
            });
        }
        let spec = self.spec_for(ctx);
        let limits = self.build_call_limits(ctx, tool_name);
        let vm = self.launcher.acquire(&spec).await?;
        let exec = vm.execute(ctx, &limits, tool_name, tool_input).await?;
        vm.release().await.ok();
        Ok(SandboxExecution {
            stdout: exec.result.model_output,
            stderr: String::new(),
            exit_status: if exec.result.is_error { 1 } else { 0 },
        })
    }
}
```

The pool ownership rule is **the launcher owns the pool**. This means:

- One `MicroVmProvider` instance ↔ one launcher instance ↔ one pool.
- Subagents that inherit the parent's `Arc<dyn SandboxProvider>` (via `RuntimeConfig.sandbox_provider`) **share that pool**. Concurrent tool calls from parent + subagents serialize on `tokio::sync::Mutex<VecDeque<WarmVm>>`; if the pool is empty, the launcher cold-boots an ad-hoc VM.
- A user who wants **per-subagent pool isolation** must construct a fresh `MicroVmProvider` for each subagent runtime — explicit, not implicit. Halo's RFD 0025 supervisor will configure this; documented in the halo integration notes.

This was previously misstated in v0.2's Open Question #2 ("each subagent's runtime gets its own MicroVmProvider"); v0.3 corrects it.

Telemetry rows extend the existing `SessionEntryKind::SandboxAction` from RFD 0022. The schema decision is **one union struct with all-optional new fields** (rather than splitting into `Local`/`Remote` variants), because it lets `pi-stats::aggregate::by_sandbox_provider()` keep its current rollup shape without per-variant code paths, and because all "new" fields are independently meaningful (a local pool-miss telemetry row has a `cold_boot` but no `cost_usd`; a remote E2B row has the inverse).

```rust
SandboxAction {
    provider: String,           // "microvm" | "local-process" | "e2b" | "sprites" | "daytona"
    tool_name: String,
    duration_ms: u64,           // total host-observed; sum of acquire + guest
    exit_status: i32,
    is_error: bool,
    // NEW (this RFD — local microVM):
    #[serde(default, skip_serializing_if = "Option::is_none")]
    transport: Option<String>,  // "firecracker" | "vfkit" | "cloud-hypervisor"
    #[serde(default, skip_serializing_if = "Option::is_none")]
    acquire_to_ready_ms: Option<u32>,  // host-observed time-to-first-byte
    #[serde(default, skip_serializing_if = "Option::is_none")]
    cold_boot: Option<bool>,
    // NEW (RFD 0026 — remote):
    #[serde(default, skip_serializing_if = "Option::is_none")]
    cost_usd: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    round_trip_ms: Option<u32>,
}
```

The new fields are added as an **amendment to RFD 0022** (which is currently marked Implemented v1.0 — adding optional fields is non-breaking; existing telemetry rows deserialize fine because of `#[serde(default)]`). RFD 0022's revision history will be appended with an `(amended by RFDs 0023 + 0026)` note when those RFDs land. `pi-stats::ingest` adds nullable columns (`transport TEXT`, `acquire_to_ready_ms INTEGER`, `cold_boot INTEGER`, `cost_usd REAL`, `round_trip_ms INTEGER`) to the `sandbox_actions` SQLite table.

### 3. The local microVM contract

#### Guest rootfs (one artifact, every host)

- alpine 3.19+ minirootfs as the base (~6 MB).
- `pi-sandbox-worker` binary (statically linked against musl, ~6–8 MB) at `/usr/local/bin/pi-sandbox-worker`.
- An init script at `/init` that:
  1. mounts `/proc`, `/sys`, `/work` (virtio-fs share), `/dev/vsock`.
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
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ToolResponse {
    pub call_id: String,
    pub stdout: String,
    pub stderr: String,
    pub exit_status: i32,
    pub guest_duration_ms: u32,
    pub is_error: bool,
}
```

One JSON line per direction, `\n`-framed. Carried over a vsock connection on `VSOCK_DEFAULT_PORT`. **Guest-initiated**: the guest worker listens on the vsock port; the host's `acquire()` blocks until the guest signals "ready" by accepting a connection. This dodges known macOS host-listen quirks under vfkit and matches aegis's working pattern under Firecracker.

#### `ToolResponse` ↔ `ToolResult` field mapping

The host needs to reconstruct a `pi_tool_types::ToolResult` from the wire `ToolResponse` so the rest of the agent loop sees a uniform shape regardless of sandbox. `ToolResult` (post-Commit-A1) has these fields:

```rust
pub struct ToolResult {
    pub tool_use_id: String,            // matches the LLM's tool_use id
    pub model_output: String,           // text fed back to the LLM
    pub display: Option<String>,        // optional UI-only rendering
    pub is_error: bool,
}
```

Mapping rules (host-side, in `MicroVmProvider::execute_tool`):

| `ToolResult` field | Sourced from                                       | Note |
|--------------------|----------------------------------------------------|------|
| `tool_use_id`      | The `call_id` field of the runtime's outer `ToolCall` (NOT the wire `ToolRequest.call_id`). | Wire `call_id` is host-allocated for guest-side dedup; the LLM-facing `tool_use_id` lives only on the host. |
| `model_output`     | `ToolResponse.stdout`                              | Direct copy. Stderr is dropped on the model-facing path; preserved only in `SandboxAction.stderr` telemetry (future addition). |
| `display`          | `None` in v1.0                                     | The wire protocol doesn't carry a `display` channel. Tools that today produce a `display` value (none of pi-tools-core do — only `monitor`) lose it under microvm. Documented; not a regression. |
| `is_error`         | `ToolResponse.is_error`                            | Direct copy. |

`ToolResponse.exit_status` and `guest_duration_ms` go into the `SandboxAction` telemetry row, not into `ToolResult`. `ToolResponse.stderr` is currently dropped on the model-facing path; a future v1.x can add `stderr_tail: Option<String>` to `ToolResult` if telemetry shows it's load-bearing for debugging.

This mapping is what `RemoteSession::execute()` in RFD 0026 must also implement so that local + remote sandboxes produce indistinguishable `ToolResult` shapes downstream of the agent loop.

#### Filesystem semantics — virtio-fs read-write (NOT read-only)

`/work` in the guest is a virtio-fs mount of the host's session cwd, **read-write**. Tools that mutate files (write/edit) modify host files directly through virtio-fs. The guest enforces no path traversal beyond `/work` (the worker's tool dispatcher rejects absolute paths or `..` segments outside the mount).

`bash` runs inside the guest with `/work` as its cwd. Bash writes to `/work` are durable on the host. Bash writes outside `/work` (in the guest's tmpfs, e.g. `/tmp`) are ephemeral — gone at VM shutdown. This is documented loudly so users understand the boundary.

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

### 3.5. ContextFS integration (added v0.5)

The microVM rootfs ships an in-guest `contextfsd` daemon that mediates
file-system access for the agent. ContextFS RFD-0023 (their numbering;
the embedder profile) is the contextfs-side spec that ships the
library entry, transport-generic mesh, vsock listeners, per-VM tenant
secret derivation, OIDC handoff, and audit-ping stub. It is published
at contextfs `v0.3.0+`. Pi-rs Commit G integrates against that surface;
all six sub-sections below are operator-side responsibilities at VM
provisioning time.

#### 3.5.1 — `vm_id` source

Pi-rs already mints a per-VM UUIDv4 in
`crates/pi-sandbox/src/microvm/firecracker.rs:643` (`Uuid::new_v4()`)
at provisioning. The same id is hashed into the vsock CID in
`vm_id_to_cid()`. Commit G threads it into the daemon TOML as the
operator-supplied `vm_id` field (contextfs RFD-0023 §5: required when
embedder mode is in use; format `[A-Za-z0-9._-]{1,128}`).

#### 3.5.2 — Per-VM tenant secret (host-side derivation)

At provisioning, the orchestrator calls the contextfs CLI helper:

```bash
secret=$(contextfs-cli key derive-per-vm-secret \
    --master-path /etc/contextfs/<tenant>.master \
    --tenant-id <tenant_id> \
    --vm-id "<pi_firecracker_uuid>" \
    --master-epoch <N>)
```

Output is 32 hex bytes. Pi-rs writes it to a tmpfs file (mode 0600)
inside the VM's run-dir, e.g.
`<run_dir>/<vm_id>/cfs-tenant-secret`, and bind-mounts the file into
the guest at `/var/run/cfs/tenant_secret`. The daemon TOML's
`tenant_secret_path` points at the bind-mounted location.

The orchestrator reads `<N>` (the master epoch) from broker
configuration; Commit G's `MicroVmProvider` accepts a
`master_epoch: u32` field on its construction config and threads it
into both the CLI invocation and the TOML.

#### 3.5.3 — Workload-identity OIDC token

Pi-rs's host orchestrator already mints a Workload Identity token per
job (this is the same flow that authenticates against any host service
the guest needs — RFD 0021 §"Open Q3"). For contextfs, that token is
mounted into the guest at `/var/run/secrets/token` (a single file,
mode 0644 since the guest is single-tenant per VM) and the daemon TOML
`oidc_token_path` points at it. The daemon reads the token on every
`WriteVerifyRequest`; rotation is host-driven (orchestrator rewrites
the file when its WI token rotates).

Failure routing for contextfs's three typed OIDC denials:
- `oidc_validation_failed` — broker rejected the token.
  → `BrokerOidcRejected` StartError variant (transient if JWKS
  rotation, persistent if config); MicroVmProvider's `acquire_to_ready`
  retries once after JWKS-rotation grace, otherwise surfaces to the
  operator.
- `oidc_token_required` — broker has a validator configured but the
  daemon sent an empty `oidc_token`. Configuration bug; surfaced to the
  operator immediately, no retry.
- `oidc_token_unexpected` — daemon sent a token but the broker has no
  validator for this tenant. Configuration bug (broker's `--tenant-mode`
  flag missing or the wrong validator pinned); surfaced immediately, no
  retry.

#### 3.5.4 — Per-mount audit-ping

ContextFS RFD-0023 §7 ships a v1 audit-ping shape: every successful
write-class FUSE op (write / create / unlink / rename / setattr /
xattr.set / xattr.remove) forwards the AuditRecord to the broker as a
`Request::WriteAuditPing`. The daemon TOML opts in per mount:

```toml
[mounts.<name>]
audit_ping = { mode = "fail-open", high_water_mark = 1024 }
```

For the routine code-editing-agent use case Commit G targets,
`fail-open` with `high_water_mark = 1024` is the default — a transient
broker hiccup must not fail the agent's `cargo build`. Operators with
hostile-tenancy / compliance-graded tenants opt their tenant configs
into `fail-closed`; the contextfs broker now (HEAD `dbe2df5`) refuses
writes with `EIO` BEFORE backend mutation when the audit-ping channel
is saturated, so fail-closed is integrity-correct. Pi-rs's per-tenant
config plumbing (RFD 0022 §C) maps to this 1:1.

#### 3.5.5 — Broker invocation (host-side)

The host-side broker MUST be invoked with:

```
contextfs-broker \
    --tenant-mode <tenant_id>=embedder \
    --verify-write-oidc-issuer <issuer-url> \
    --verify-write-oidc-audience <wi-audience> \
    --verify-write-oidc-alg RS256 \
    --vsock <CID>:<PORT>           # Linux only, gated on `vsock` Cargo feature
    --vsock-key <PATH>             # broker's Noise-IK static key
    --vsock-peer-pubkey <PATH>     # in-guest daemon's pinned static key
    [other flags]
```

`--tenant-mode <t>=embedder` is the operator's opt-in to embedder mode.
Without it the broker defaults to legacy mode and refuses any request
carrying `vm_id`/`master_epoch` with a typed
`tenant_mode_legacy_no_vm_id` denial. With it set, every request must
carry non-empty `vm_id` + `master_epoch` (typed `vm_id_required`
denial otherwise). Pi-rs's MicroVmProvider asserts both are set on its
config at construction; absence is a hard error at startup, not at
first request.

#### 3.5.6 — Daemon TOML rendered at provisioning

The full operator-rendered daemon TOML for one Commit G provisioning,
inside the guest at `/etc/contextfsd/daemon.toml`:

```toml
tenant_id        = "tenant-a"
vm_id            = "<pi_firecracker_uuid>"
master_epoch     = 7
tenant_secret_path = "/var/run/cfs/tenant_secret"   # bind-mounted from host tmpfs
oidc_token_path    = "/var/run/secrets/token"        # bind-mounted WI token

[broker]
socket_path = "/run/contextfs/broker.sock"           # vsock-bridge → host UDS

[mounts.workspace]
backend     = "local-dir"
mountpoint  = "/workspace"
audit_ping  = { mode = "fail-open", high_water_mark = 1024 }
```

The `MicroVmProvider` renders this from a `MicroVmProviderConfig`
struct at construction; `pi sandbox doctor` validates each field
(file paths exist, broker reachable, `--tenant-mode` flag visible in
the broker's argv) per the §5 probe contract.

#### 3.5.7 — Version compatibility

ContextFS broker MUST be `>= v0.3.0` (the version that shipped
`vm_id` / `master_epoch` / `oidc_token` over the wire). v0.2.x brokers
will reject any embedder request via serde unknown-field rejection at
the first `verify_write`; this is the documented fail-loud signal. Pi-rs's
`pi sandbox doctor` probes the broker's version via the existing
`/version` HTTP endpoint (RFD 0022 §A).

### 4. Per-OS launcher impls

#### Linux: `FirecrackerLauncher` (`#[cfg(target_os = "linux")]`)

- Probes `/dev/kvm` and the `firecracker` binary at construction.
- Maintains a **warm pool** of N (default 2) pre-booted VMs as a `tokio::sync::Mutex<VecDeque<WarmVm>>`. `acquire()` pops a warm VM in O(1); release returns it for the next call. Default is 2 because real coding-agent tool calls are dominantly sequential (write → read → bash → read); pool=2 covers one parallel subagent burst at ~512MB resident. Telemetry on pool hit-rate decides whether to bump to 4.
- Pool refills opportunistically in the background.
- Each VM gets its own firecracker process, API socket, vsock socket, and virtio-fs share. Rotation: VMs are torn down and replaced after N tool calls or T seconds (default 50 calls / 5 minutes) to bound cumulative state leakage.
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
- WSL2 alternative offered as `--sandbox-provider=microvm:wsl2` for users who already have WSL2 — different impl, separate launcher, NOT the default.

LoC estimate: 1000.

### 5. CLI UX and `pi sandbox doctor`

```
pi --sandbox-provider=microvm                  # auto-pick per OS
pi --sandbox-provider=microvm:firecracker      # explicit pin
pi --sandbox-provider=microvm:vfkit
pi --sandbox-provider=microvm:cloud-hypervisor
pi --sandbox-provider=microvm:wsl2             # opt-in Windows alt
pi --sandbox-provider=local-process            # existing, no isolation
```

`pi sandbox doctor` runs all available launchers' `probe()` and prints a per-transport report:

```
$ pi sandbox doctor
microvm:firecracker  ✓ available  (Firecracker v1.15.0, probe 87ms)
                       checks: kvm_open_rw ✓, kvm_group_member ✓,
                               vsock_module ✓, virtiofsd ✓, fc_binary ✓
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

- **`FirecrackerLauncher`:** `firecracker` binary on PATH; `/dev/kvm` openable RW (catches kvm group / cgroup denial); kernel `vsock` module loaded (`lsmod` or `/sys/module/vsock`); `virtiofsd` binary on PATH; AppArmor/SELinux denial test (no-op `firecracker --version` under restrictive profile).
- **`VfkitLauncher`:** Hypervisor.framework available (probe via `sysctl kern.hv_support`); `vfkit` binary on PATH or vendored; vfkit version ≥ 0.5 (virtio-vsock support).
- **`CloudHypervisorLauncher`:** WHPX feature enabled (probe via `Get-WindowsOptionalFeature -FeatureName HypervisorPlatform`); `cloud-hypervisor.exe` on PATH; admin/elevated check (WHPX requires either elevated tokens or a service install).

A real probe takes 50–200ms, not 10ms — the time accounts for the actual filesystem/process operations.

### 6. Threat model

`microvm` defends against:

- **Malicious model output executing arbitrary host commands.** A model that is jailbroken or prompt-injected emits `bash` calls; the guest contains the blast radius (no host fs outside `/work`, no network, killed on timeout).
- **Tool input crafted to escape the sandbox.** Path traversal in `read`/`write`/`edit` is rejected by the guest worker; the virtio-fs share enforces the host-side cwd boundary.
- **Resource exhaustion.** `ResourceLimits` caps memory, vCPUs, disk, wall time.

`microvm` does **not** defend against:

- **A user who explicitly chose `local-process`.** That flag means "I trust the model." No silent downgrade from `microvm` to `local-process` ever happens.
- **Side channels through `/work`.** A malicious tool writing a poisoned file the user later opens is out of scope; users are responsible for what they let the model edit.
- **VMM exploits.** Firecracker / vfkit / cloud-hypervisor have small but non-zero attack surfaces. Mitigation: pin to recent versions, monitor security advisories.

**Rootfs contents are public knowledge.** The alpine image, busybox version, the `pi-sandbox-worker` binary itself — all of it is observable to a jailbroken model running `cat /etc/os-release` or `cat /etc/passwd` inside the guest. Do **not** bake credentials, tokens, or per-user state into the rootfs. The rootfs is a single artifact shared by every pi user worldwide; treat it like a pre-built docker image.

**Guest tooling is busybox, not GNU coreutils.** `bash` calls run under busybox `ash`, not real bash. Subtle option drift: `find -printf`, `grep -P`, `ls --color=auto`, GNU-specific tar flags, etc. behave differently. v1.0 ships with a known-incompatibility list in the docs. Users with GNU-specific scripts can pin `--sandbox-provider=local-process` for those sessions. A future RFD can offer a glibc-based rootfs as an opt-in pin (`microvm:firecracker:rootfs=glibc-debian`) if telemetry shows the friction is widespread.

### 7. Performance SLO

Per-tool-call overhead targets, measured as `host_observed_total_ms - guest_duration_ms`:

| Transport         | Cold boot | Warm pool acquire |
|-------------------|-----------|-------------------|
| Firecracker (Linux) | ≤ 400ms | ≤ 30ms          |
| vfkit (macOS)       | ≤ 800ms | (pool deferred) |
| cloud-hypervisor (Windows) | ≤ 1200ms | (pool deferred) |

A 20-tool-call coding turn under Linux+pool should add ≤ 600ms total vs `local-process`. If telemetry shows worse than 1.5× the SLO sustained, follow-up optimization is required before declaring v1.0 GA.

The `boot_ms` + `cold_boot` telemetry fields make the pool-hit-rate visible.

## Implementation schedule

Eight commits across three phases. Realistic LoC estimates (calibrated against aegis's 3486 LoC for Firecracker alone):

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
| **C** | `MicroVmLauncher` trait, `VmHandle`, `VmSpec`, `ResourceLimits`, `NetworkPolicy`, errors | 200 |
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
- **Stage 4 (Commit G):** `--sandbox-provider=microvm` (auto-pick) goes live, with the cross-OS coverage promise honest.

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
3. **Tool selection for `pi-tools-core` (the guest-buildable subset).** Today: read/write/edit/bash/grep/find/ls/monitor. Excluded: web_search (needs network). Confirm pre-Commit A2.
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

- After Phase 3 + G: `pi --sandbox-provider=microvm:firecracker "ls + read + edit a marker file"` on Manjaro. Confirm session JSONL has the expected `transport`, `boot_ms`, `cold_boot` fields, and the host file actually changed.
- Same dogfood on macos-14 + windows runner once D and F have landed.
- `pi --stats sandbox-actions` must show non-zero rows with the new `transport` column populated.

## Revision history

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
