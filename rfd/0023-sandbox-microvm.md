# RFD 0023 — Local MicroVM Sandbox (Linux/macOS/Windows)

- **Status:** Discussion (v0.42 — rfd-critic READY, polish landing)
- **Author:** pi-rs maintainers
- **Created:** 2026-05-02
- **Implemented:** (pending)

## Summary

RFD 0022 shipped the `SandboxProvider` trait but only a passthrough `LocalProcessProvider` whose own docstring admits it doesn't isolate anything: `tool.invoke()` runs inline, same fs / same UID / same network. The wiring is correct, the contents are vapor. End-to-end dogfood produced `duration_ms: 0` per call — proof nothing forks.

This RFD ships the first **real isolation backend**: a `MicroVmProvider` that runs each tool call inside a Linux microVM, with one rootfs and one wire protocol shared by per-OS launchers (Firecracker on Linux, vfkit on macOS, cloud-hypervisor+WHPX on Windows). **Phased rollout:** explicit launcher pins (`--sandbox-provider=microvm:firecracker`, `…:vfkit`, `…:cloud-hypervisor`) ship per platform as each launcher lands and dogfoods clean. The unqualified `--sandbox-provider=microvm` *auto-pick* form ships only after all three OS paths meet the cross-platform GA bar (probe, dogfood, parity tests green on each).

**Linux/Firecracker v1 is operator-managed** — not the self-contained "just install pi and go" backend that the macOS and Windows launchers deliver. Because Firecracker has no virtio-fs (per `rfd/0023-known-issues.md` and upstream issue #1180), the Linux `/work` mount goes through contextfs, which requires an operator-deployed contextfs broker + `cfs-fs-server` + `cfs-mesh` and per-VM tenant secret derivation (§3.5). This is honest in the CLI UX: `pi sandbox doctor` on a Linux host with no contextfs config tells the user explicitly that Linux/Firecracker is "operator-managed mode (requires contextfs broker)" and points at the alternatives (`local-process`, or a future macOS/Windows host). macOS/Windows v1 ship a self-contained virtio-fs RW path with no broker — that's the "just works" UX. v1 does NOT ship a self-contained Linux mode; that's a follow-on RFD when Firecracker either gains virtio-fs or pi-rs gains a guest-side FUSE proxy that doesn't depend on contextfs.

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

### Terminology (one source per concept)

The RFD distinguishes three orthogonal axes that v0.25 reviewers have repeatedly conflated. This table is normative.

| Concept           | Values                                                    | Where it lives                                | CLI surface                                                   |
| ----------------- | --------------------------------------------------------- | --------------------------------------------- | ------------------------------------------------------------- |
| **Provider name** | `local-process` \| `microvm` \| `e2b`/`sprites`/`daytona` (RFD 0026) | `SandboxAction.provider`, `SandboxTelemetry.provider` | `--sandbox-provider=<name>` (or `<name>:<launcher>`)          |
| **Launcher name** | `firecracker` \| `vfkit` \| `cloud-hypervisor`            | `SandboxAction.launcher`, `MicroVmLauncher::launcher_name()` | `--sandbox-provider=microvm:<launcher>` (explicit pin)        |
| **Transport mode**| `local` \| `managed`                                      | `BootSpec.transport_mode`                      | `--sandbox-microvm-mode=<mode>` (Linux/Firecracker only)      |
| **Dispatch path** | `guest` (v1 microvm — every tool runs in the guest worker) | implicit; no telemetry field — `provider="microvm"` already implies it | not user-facing |

`microvm:local` and `microvm:managed` are NOT CLI names; they would conflate provider/launcher with transport mode. The CLI uses `--sandbox-provider=microvm:firecracker --sandbox-microvm-mode=managed` for the Linux/managed case.

**No host-direct dispatch.** Earlier drafts (≤v0.32) routed `web_search` through a host-direct path so the guest stayed network-free. v0.33+ removes that asymmetry: `web_search` registers in the guest worker like every other tool; its handler talks to a guest-side bridge daemon over a UDS, which forwards to a host-initiated vsock channel handled by an async task inside the existing pi binary process — no separate host binary, no host-listening sockets. From the agent loop, telemetry, audit-approve, and `tool_disposition()`, `web_search` is a guest tool. See §"web_search via vsock proxy" for the protocol and topology.

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

Audit of `pi-tools/Cargo.toml` *at the time RFD 0023 v0.4 was written (2026-05-02)* showed `pi-ai.workspace = true` and `reqwest.workspace = true` as unconditional `[dependencies]` — the host build pulled in the whole world. As of 2026-05-04 those entries have moved to `[dev-dependencies]` (Commit A2 landed; `pi-tools` itself is a thin re-export façade over `pi-tools-core` + `pi-tools-net`). The dependency-problem framing below is preserved as historical context for *why* the A1/A2 split exists; the current state is documented in the "Resolution" paragraph.

**Resolution (Commits A1/A2 — both shipped):** A1 extracts the POD types `ToolResult` / `ToolSpec` / `ToolError` into a tiny `pi-tool-types` crate (deps: `serde`/`serde_json`/`thiserror` only); `pi-ai` re-exports them for back-compat. A2 splits `pi-tools` into `pi-tools-core` (the guest-safe file/process tools — `read`/`write`/`edit`/`bash`/`grep`/`find`/`ls`/`monitor` all live here today; `monitor` is in the source tree but is **not registered** in the guest worker's tool dispatcher under v1 because the one-shot RPC can't carry its streaming output) and `pi-tools-net` (web_search), with `pi-tools` itself becoming a re-export façade. **As of 2026-05-04 both A1 and A2 are merged on `main`** (`crates/pi-tools/Cargo.toml` already pulls `pi-tools-core` + `pi-tools-net`). The remaining A-series work for guest-side completeness: extracting `ToolContext` / a `Tool` impl that doesn't transitively pull `pi-ai` (some shared trait machinery still lives in `pi-tools` re-exporting `pi-ai` types). The guest worker depends on `pi-tool-types` + `pi-tools-core` + `pi-sandbox-protocol` + `pi-search-proto` (the last is new in v0.34, ~50 LoC of wire types — does NOT pull `pi-tools-net` or `reqwest` into the guest; the worker's `web_search` handler is a pure-vsock shim that reuses `pi-search-proto::WebSearchRequest`/`Response` and forwards to the channel). Compiles statically against musl, links into a ~6–8 MB binary, fits in alpine.

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
    /// `unavailable` (the §"Tool availability under microvm"
    /// matrix). `web_search` is a `guest` tool whose handler
    /// proxies via vsock to the host (see §"web_search via vsock
    /// proxy"); there is no `host-direct` disposition in v1.
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
guest tools, `web_search` (guest, vsock-proxied), `monitor`
(unavailable), `lsp` (unavailable in v1). `task` is `RuntimeNative`
and never appears in that matrix.

#### Plan-time advertisement — `SandboxProvider::tool_disposition()`

Runtime-only rejection isn't enough. The model plans against the
advertised tool list at conversation start; if `monitor` and `lsp`
appear there, a model under microvm will plan to use them and fail
at execution. Worse, RFD 0005 subagents (project-local or bundled
agent definitions at `.pi/agents/<name>.md` or
`crates/pi-coding-agent/agents/<name>.md`) **may** carry explicit
tool allowlists in their frontmatter; when an allowlist includes
`lsp`, running the subagent under microvm fails mid-task in a
confusing way. The bundled agents in tree today do not always
declare such an allowlist — but the pattern is supported and
must work correctly when operators do declare one.

Commit G adds a **plan-time capability-query API**:

```rust
// New on the SandboxProvider trait (RFD 0022 amendment):
pub enum SandboxToolDisposition {
    Guest,        // routes through provider's guest path
                  // (for `web_search` under microvm: guest tool whose
                  // handler proxies via vsock — see §"web_search via vsock proxy")
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
    ///     `grep`/`find`/`ls`/`web_search` (web_search is a guest
    ///     tool whose worker-side handler proxies the actual HTTP
    ///     call out via a vsock control port to the host-side
    ///     a host-initiated vsock channel, ultimately handled by a
    ///     per-VM async task inside the pi process — §"web_search
    ///     via vsock proxy");
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
plus a stderr banner on the first filter event per session.
Operators with strict-mode tenants can opt into **fail-fast** via
`[task] on_unavailable_tool = "fail-fast"` in the campaign or
settings.json — at session start, if any allowlisted tool is
`Unavailable` under the active provider, the agent aborts with
`AgentError::ToolUnavailable` before the first turn.

The session event is a new variant on `SessionEntryKind`:

```rust
// pi-agent-core::session_log
SessionEntryKind::ToolFilteredOut {
    agent: String,             // e.g. "code-reviewer" — agent definition name
    tool: String,              // the filtered tool name, e.g. "lsp"
    provider: String,          // active provider, e.g. "microvm"
    reason: String,            // e.g. "Unavailable under provider 'microvm'"
}
```

Example JSONL line:
```json
{"ts":"2026-05-04T18:42:11Z","kind":"tool_filtered_out","agent":"code-reviewer","tool":"lsp","provider":"microvm","reason":"Unavailable under provider 'microvm'"}
```

`pi-stats::ingest` does **not** ship a SQLite column for this event in v1; it's session-JSONL-only. Operators can already grep / `jq` for `kind == "tool_filtered_out"` rows, and the count is low enough (per-agent-startup, not per-call) that aggregation isn't load-bearing yet. v1.1 may add a `tool_filtered_out` table if telemetry shows demand.

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
| `web_search`        | guest (proxied) | Network query. Registered in the guest worker like every other tool; the worker-side handler talks to a small in-guest bridge daemon over a UDS, which forwards to a host-initiated vsock channel handled by a per-VM async task inside the host's pi process. The guest itself has no network. From the agent loop / `tool_disposition()` / telemetry / audit-approve, this is a guest tool. (§"web_search via vsock proxy".) |
| `monitor`           | unavailable  | One-shot RPC vs. streaming mismatch; returns `ToolUnavailable` (§"monitor exclusion"). |
| `lsp`               | unavailable  | The current `lsp` tool lives in `crates/pi-coding-agent/src/native/lsp/tool.rs`, ABOVE `pi-sandbox` in the dependency graph — `pi-sandbox` cannot dispatch to it without a circular dep or a lower-layer rewrite. **v1 microvm marks `lsp` unavailable** (returns `ToolUnavailable` with a startup-time stderr banner if the user has LSP integration enabled). The `LspWriteTool` write-decoration path is similarly unavailable: under microvm, `write` runs guest-side without LSP post-processing. Operators who need LSP under sandbox use `--sandbox-provider=local-process`. A future RFD can re-home `lsp` into a lower crate and reclassify it; that's not in Commit G's scope. |
| Future tools        | TBD per RFD  | Each new tool RFD MUST classify into one of `guest` / `unavailable`. There is no `host-direct` disposition in v1 — tools that need host-side resources (network, host filesystem outside `<host_cwd>`, hardware access) must define their own narrow vsock-proxied protocol like `pi-search-proto` does. The default (if a tool RFD forgets) is `unavailable`. |

#### `monitor` exclusion (decided)

`pi-tools::monitor` spawns a long-running observer that streams partial output to the agent. The v1 wire protocol in §3 is one `ToolRequest` → one `ToolResponse`, JSON-line-framed, single round trip. **Streaming is incompatible with the v1 protocol shape.** Two paths considered:

- **(a) Add streaming responses to v1.** Adds significant complexity to the host and worker (state machine for partial messages, EOF detection, cancellation), and `monitor` is the only consumer.
- **(b) Exclude `monitor` from `pi-tools-core`.** It stays in pi-tools but is not reachable through `microvm`. A microvm-mode session that calls `monitor` returns a clean error (`tool not available under --sandbox-provider=microvm; use --sandbox-provider=local-process or RFD 0026 remote backends`).

**Decision: (b).** Telemetry from existing pi sessions can quantify monitor usage; if it's > 5% of tool calls in real coding sessions, v1.1 of this RFD adds streaming responses. Until then, the protocol stays one-shot.

This decision is upstream of Commit A3 (the protocol crate). It must land here, not deferred.

#### `web_search` via vsock proxy (decided)

`web_search` is a network-egress query — it contacts external search engines / LLM-as-search backends and returns text. It does NOT execute untrusted code on the host; it queries data. The microVM's job is to contain *code execution* (untrusted bash, file mutation, process spawn), not data queries; agents lose real capability if `web_search` is excluded entirely.

**The constraint** is that the guest itself has no network in v1 (a deliberate v1 invariant, see §"Goals/non-goals"). Earlier drafts (≤v0.32) resolved this by partitioning tools into "guest-bound" and "host-bound", with `web_search` running directly on the host via `pi-tools-net`. That worked but **broke the agent's mental model**: from the model's perspective, "tools run in the sandbox" became "*most* tools run in the sandbox, except this one, which runs on the host with full network access". The asymmetry leaked into telemetry (`dispatch_path = "guest" | "host-direct"`), required a `HostBoundToolDispatcher` trait abstraction for one tool, and made the architecture's security posture harder to reason about.

**v0.34 fix.** `web_search` registers in the guest worker like every other tool. Its handler is *not* a direct HTTP call; instead it forwards a typed `WebSearchRequest` over a long-lived host-initiated vsock channel and reads back `WebSearchResponse`. From every upstream surface the call is a guest tool dispatch:

- `tool_disposition("web_search") → SandboxToolDisposition::Guest`
- `SandboxAction.dispatch_path` is no longer recorded — there is no longer a meaningful split (every microvm tool dispatches to the guest)
- Auto-approve policy (RFD 0027 H4) gates the call at the host's `MicroVmProvider::execute_tool` entry, identically to bash/edit/etc.
- Telemetry attributes the duration to `guest_duration_ms` (which now correctly includes the inner vsock round-trip + host's HTTP round-trip)

**Wire shape** (`crates/pi-search-proto`, new in Commit G):

```rust
pub const SEARCH_PROXY_PROTOCOL_VERSION: u32 = 1;
pub const VSOCK_SEARCH_PROXY_PORT: u32 = 5003;

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WebSearchRequest {
    pub proto_version: u32,
    pub call_id: String,            // matches the outer ToolRequest.call_id
    pub query: String,
    pub max_results: u32,           // worker-truncated to host-policy cap
    pub locale: Option<String>,     // optional; host-side default if None
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WebSearchResponse {
    pub call_id: String,
    pub results: Vec<SearchResult>,  // host-side typed; identical to pi-tools-net's shape
    pub error: Option<String>,       // populated iff results is empty AND search failed
}
```

One JSON line per direction, `\n`-framed.

**Topology — three actors, two hops, every vsock host-initiated:**

```
[ host ] -- vsock 5003 (host-initiated) -- [ pi-vm-search-bridge ]  -- /run/pi-bridge/search.sock --  [ pi-sandbox-worker ]
   ^                                       (long-lived child of                                       (re-execed by reset;
   |                                        PID 1 = pi-cfs-init)                                       short-lived UDS client)
   per-VM async task in `pi-sandbox`
   (host's pi-tools-net + rate limit)
```

**Guest process tree (canonical, normative).** `/init` execs `pi-cfs-init`; `pi-cfs-init` is the **sole PID 1**. It:
1. Performs the contextfs/virtio-fs setup described in §3.5.9 (Linux/Firecracker managed mode) or the simpler virtio-fs setup (macOS/Windows v1).
2. Launches `pi-vm-search-bridge` as a long-lived child and remembers its PID.
3. Launches `pi-sandbox-worker` (also as a child of PID 1) and remembers its PID.
4. Reaps zombies, supervises both children. If `pi-vm-search-bridge` exits unexpectedly, `pi-cfs-init` writes `PI_FAIL: search-bridge-died exit=<N>` to the serial console and exits non-zero (the host's launcher reads this and destroys the VM — same pattern as boot/reset failures). The bridge is not auto-restarted in v1: a dead bridge means the search channel is gone and any in-flight `web_search` is unrecoverable, so destroy-and-replace is the right policy.
5. On reset, `pi-cfs-init` is asked by `pi-vm-reset` (the reset agent — also a child of PID 1, spawned from `pi-cfs-init` on demand) to terminate and re-spawn the worker after `pivot_root`. The bridge child is preserved across `pivot_root` by `move_mount`-ing a **dedicated bridge runtime tmpfs at `/run/pi-bridge`** into the new root. The bridge's UDS lives at `/run/pi-bridge/search.sock` (NOT in the general `/run` tree, so the survival list is precise: only `/run/pi-bridge` is preserved, not all of `/run`). The bridge process's cwd is set to `/run/pi-bridge` at boot so it remains valid after pivot_root; its open vsock fd and UDS listener fd are unaffected by the root switch (they're kernel-side socket objects, not path-bound). After `pivot_root` completes, `pi-cfs-init` exec-replaces the worker; the bridge keeps running with its open vsock connection and UDS listener untouched.

This makes ownership unambiguous: PID 1 is `pi-cfs-init`. The bridge and worker are both its children. The reset agent is also its child. No actor is "PID 1 / reset-stable" outside of `pi-cfs-init` itself.

**Bridge daemon (`pi-vm-search-bridge`)** — a tiny statically compiled helper (~80 LoC, owned by Commit B alongside the rootfs init). Its job: (a) accept the host-initiated connection on guest vsock port `VSOCK_SEARCH_PROXY_PORT = 5003`, (b) listen on a Unix-domain socket at `/run/pi-bridge/search.sock` (mode `0o600`, owned by `pi-worker:pi-worker` UID/GID), (c) forward UDS↔vsock framed `WebSearchRequest`/`WebSearchResponse` lines.
2. **Worker side** is stateless w.r.t. the channel. The worker's `web_search` handler does `connect("/run/pi-bridge/search.sock")` per call, writes one `WebSearchRequest`, reads one `WebSearchResponse`, closes. No long-lived fds in the worker; no fd-inheritance leak to bash subprocesses (UDS path is mode `0o600` owned by `pi-worker`, and `bash` runs under a different UID — see "Bash-can't-bypass" defense below).
3. **Host side**: at acquire time, after the main vsock to guest port 5001 succeeds, the launcher opens a *second* vsock connection to guest port 5003 (host-initiated, matches main protocol's pattern, avoids the macOS vfkit host-listen quirk). It hands the connected socket to a per-VM async task inside the pi binary process. The task reads `WebSearchRequest`, calls `pi-tools-net::web_search` with host-side auth, writes `WebSearchResponse`. Per-VM rate-limit state (default 30 calls / 60s, configurable). Multiple in-flight calls multiplex through the channel in serialise-then-respond order; v1 enforces one-in-flight-per-VM with a queue inside the bridge daemon (concurrent searches inside one VM are rare and serialise cleanly).

**Reset survives — channel does not break.** Because `pi-vm-search-bridge` is reset-stable (PID 1 / sibling daemon), the in-guest endpoint of the 5003 vsock connection persists across worker re-exec. The host's per-VM async task also persists (it's an in-process task inside the pi binary, not tied to the worker's lifecycle). Worker re-exec only resets the *worker's* UDS-client side, which is per-call anyway. **Net effect: a clean pooled release/reset/reuse cycle leaves the search channel fully usable on the next acquire** — the integration test for `web_search → reset → web_search → cold_boot=false` is required (see §Testing).

**No separate host binary.** The host-side handler lives inside the existing pi binary; same auth context, same process. No `pi-host-search-proxy` external dependency.

**Cancellation semantics.** If the worker's tool-level wall_timeout fires while a `web_search` call's HTTP round-trip is still outstanding on the host: the worker returns its 124-style timeout `ToolResult` to the agent loop and drops its UDS connection. The bridge daemon notices the UDS half-close and abandons the in-flight pair — the `WebSearchResponse` from the host (when it eventually arrives) is read and discarded by the bridge. The host-side async task itself completes naturally when its Tokio task is dropped (which happens on `release()` for a destroyed VM, or when the next request finishes for a pool-returned VM); HTTP cancellation on the host side rides `reqwest`'s drop semantics. No leaked outstanding HTTP requests.

**Queue / backpressure semantics.** The bridge daemon serialises searches: one in-flight per VM. If a second `web_search` arrives at the bridge UDS while a first is still in flight, the bridge:

1. Accepts the second UDS connection (so the worker doesn't block in `connect()`).
2. Holds it in a per-VM FIFO queue.
3. Processes it only after the first request's `WebSearchResponse` has been written back to the first caller.

Queue depth is bounded at **2** (one in-flight + one waiting). A third concurrent caller hits "queue full" and the bridge writes back `WebSearchResponse { call_id, results: [], error: Some("search-busy: another search is in flight") }` immediately, then closes that UDS connection. The worker turns this into a normal `ToolResult { is_error: true, model_output: "[search-error] busy: another search is in flight; retry in a moment" }` — the call did execute, it just got a busy signal.

If the second caller's tool wall_timeout fires while it's still queued: the worker's UDS-side drop is observed by the bridge (UDS half-close in the queued slot), and the bridge dequeues it without ever forwarding to the host. The first call continues unaffected; the second sees the worker's normal 124-timeout `ToolResult`. **In practice**, agents rarely issue concurrent `web_search` calls in a single VM (the typical pattern is sequential search-then-act); the queue exists for correctness, not throughput.

**Bridge-failure detection.** If `pi-vm-search-bridge` exits unexpectedly (segfault, OOM, unhandled error), `pi-cfs-init` (PID 1) catches the SIGCHLD, writes `PI_FAIL: search-bridge-died exit=<N>` to the serial console, and exits non-zero. The host launcher reads this and destroys the VM (no auto-restart in v1: a dead bridge means an in-flight search is unrecoverable, and destroy-and-replace is the right policy). Any concurrent `web_search` call in the worker times out via the channel-drop path described under "Error mapping".

**Why this is materially better than v0.32 host-direct:**

- Architecture has *one* dispatch path (everything goes through the guest worker). One mental model.
- Host's responsibility is exactly one well-typed RPC (`WebSearchRequest → WebSearchResponse`), not "any host-bound tool we might add". The trust boundary is narrower and verifiable.
- Telemetry has *one* shape — no `dispatch_path` enum to explain.
- Auto-approve policy fires on the same code path as every other tool — identical gate semantics.
- Adding a future "must-be-on-host" tool requires defining its own narrow vsock proxy with its own typed RPC and rate limits; there is no general-purpose host-tool registry to grow.

**Bash-can't-bypass-auto-approve defense (Linux/Firecracker; defense in depth).** Two layers, neither sufficient alone:

*Layer 1 — UID separation + UDS permissions:* the worker runs as `pi-worker:pi-worker`, `bash` (and any other tool subprocess) runs as `pi-tool:pi-tool` (a separate, unprivileged UID). The bridge daemon's UDS at `/run/pi-bridge/search.sock` is mode `0o600` owned by `pi-worker:pi-worker`, so `bash` cannot `connect()` to it (POSIX-level rejection). This is the primary defense.

*Layer 2 — seccomp on the bash spawn (closes the residual `AF_VSOCK` direct creation path):* `vsock` is a process-family socket. Even with UID separation, a malicious `bash` could in principle `socket(AF_VSOCK, ...)` and try to reach the host directly bypassing the bridge UDS entirely. The worker's bash-spawn pre-exec hook closes this. The mechanism MUST be:

- Worker `fork()`s; in the **child only**, between fork and `execve()`:
  1. `prctl(PR_SET_NO_NEW_PRIVS, 1)`.
  2. `seccomp(SECCOMP_SET_MODE_FILTER)` installs a BPF program that returns `SECCOMP_RET_ERRNO(EAFNOSUPPORT)` for **`socket(AF_VSOCK, ...)`** and **`socketpair(AF_VSOCK, ...)`**. These are the only syscalls where the family lives in a register and seccomp-bpf can match it directly. `connect`/`bind`/`listen` take a `sockaddr *` userspace pointer that seccomp **cannot** dereference, so we deliberately do NOT try to filter them — there's no need: if `socket(AF_VSOCK, ...)` is denied, the child has no way to obtain a vsock fd in the first place. Pure socket()/socketpair() filtering is sufficient, and is also what `bpf-helpers(7)`-style sandboxes can actually enforce. (Earlier drafts implied seccomp could filter on fd's underlying family on `connect`; that was wrong.)
  3. `setresuid(pi_tool_uid, pi_tool_uid, pi_tool_uid)` + `setresgid(...)` to drop privileges.
  4. `execve("bash", ...)`.
- The worker process (the parent) **never** installs the filter on itself — it stays free to talk to vsock and to `/run/pi-bridge/search.sock`. The "exempt itself" wording in earlier drafts was mechanically wrong; the correct primitive is "child-only pre-exec hardening". This is what `nix::unistd::Command::pre_exec` exposes in Rust.

Layer 2 is a **guest-Linux** defense (the bash subprocess always runs in the Linux guest rootfs regardless of which launcher booted the VM). It applies uniformly across Firecracker / vfkit / cloud-hypervisor as long as the guest kernel ships `CONFIG_SECCOMP_FILTER`. The rootfs build (Commit B) already ships an alpine-based guest kernel with this enabled; `pi sandbox doctor` checks the guest kernel config at probe time and fails loud on any launcher whose kernel image lacks the option.

**FD-inheritance hygiene (normative).** All fds the worker holds for sandbox-internal channels — vsock 5001 main, vsock 5002 reset control (held by reset agent, not worker), the UDS client connection to `/run/pi-bridge/search.sock` while a search is in flight — are opened with `FD_CLOEXEC` (`SOCK_CLOEXEC` on socket creation; `F_SETFD FD_CLOEXEC` on accept). Tool children spawned via `execve` therefore start with no inherited sandbox fds. A negative test (`tests/fd_isolation.rs`) runs `bash 'ls -l /proc/self/fd'` and asserts only stdin/stdout/stderr (and an explicit per-tool stdio set) appear; no vsock or sandbox UDS fd numbers leak in.

**Negative integration tests** (Commit D suite):
- `bash 'python3 -c "import socket; socket.socket(40, socket.SOCK_STREAM)"'` (AF_VSOCK = 40 on Linux) asserts `EAFNOSUPPORT` from inside bash; the worker's own search RPC still succeeds.
- `bash 'cat /run/pi-bridge/search.sock'` asserts `Permission denied` (UDS layer).
- `bash 'ls -l /proc/self/fd'` asserts no sandbox fds leak (CLOEXEC layer).
- `web_search → bash 'echo poison > /root/marker' → release(Clean)+reset → next acquire on same BootSpec → web_search` asserts second search still works AND no `/root/marker` survives (channel survives reset; fs reset works).

**Cross-launcher status.** Both Layers 1 and 2 are **guest-side** defenses inside the Linux rootfs and apply identically regardless of which host launcher (Firecracker on Linux, vfkit on macOS, cloud-hypervisor on Windows) booted the guest. There is no per-host-OS hardening gap: the bash subprocess is always confined inside the guest kernel, which always has `CONFIG_SECCOMP_FILTER`. The `pi sandbox doctor` probe verifies the guest kernel config and fails loud on any guest image without seccomp filter support — irrespective of the host.

**Threat-model note (unchanged):** search-result content can include prompt-injection material. That's a content-injection concern the microVM does not address either way (a model on `local-process` reads the same results); orthogonal hardening track (per-result content filter / quarantine, separate RFD).

§2's `MicroVmProvider::execute_tool()` sample is now uniform: `monitor` returns `ToolUnavailable`; everything else (including `web_search`) routes through the guest worker. The host runs the search-channel async task as part of the launcher's per-VM acquire flow; no separate process.

**Error mapping.** Failure modes from the search channel surface as:

- Channel-establish failure at acquire (host can't connect to port 5003 within `acquire_timeout_ms`): `AcquireError::HostTransportSetup { detail: "search channel connect failed: <e>" }` — the VM is destroyed; acquire fails.
- Channel drop mid-call (vsock RPC error): the worker's `web_search` handler returns a `ToolError::Other("search channel dropped")` to the inner tool dispatcher, which surfaces as a normal `ToolResult { is_error: true, model_output: "[search-error]" + detail, ... }`. The VM is marked `SuspectGuestState` because the persistent channel breaking is non-trivial.
- Provider-side auth failure on the host (`pi-tools-net` returns 401/403): `WebSearchResponse { results: [], error: Some("auth: <detail>") }` flows back over the channel; the worker turns it into `ToolResult { is_error: true, model_output: "[search-error] auth failure: …" }`. VM stays `Clean` (the channel's fine, the provider config isn't).
- Rate-limit hit (host's per-VM counter exceeded): `WebSearchResponse { results: [], error: Some("rate-limited: <window_ms>") }`; same shape as auth failure for the worker. VM stays `Clean`.
- Protocol-version mismatch (worker built against `pi-search-proto vN`, host on `vM`): host detects the mismatch on the first `WebSearchRequest`, responds with `error: Some("proto-version-mismatch: host=M, guest=N")`, then closes the channel. Worker treats as a one-shot failure; subsequent searches go to the channel-dropped path above.

**Proxy lifecycle across pooling.** The search channel is established by `MicroVmLauncher::acquire()` BEFORE `acquire()` returns: the launcher cold-boots (or pulls from pool) the VM, opens the main 5001 connection, opens the 5003 search channel, then returns the `VmHandle`. If the 5003 connection fails, the VM is destroyed and `acquire()` returns `Err(AcquireError::HostTransportSetup{..})` (no half-acquired VMs in the pool). When a VM is returned to the pool via `release(Clean)` + successful reset, the search channel survives — it's per-VM, not per-call. When the VM is destroyed (any non-`Clean` path), the host-side search task is joined and dropped along with the VM.

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
    /// "cloud-hypervisor". Stable across patch releases. Returned
    /// value is the same string written to `SandboxAction.launcher`.
    fn launcher_name(&self) -> &'static str;

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
    // The Broker* variants below fire ONLY on the
    // Linux/Firecracker `managed` transport mode (contextfs-mediated
    // /work). They are NOT remote-backend concerns — RFD 0026
    // remote vendors are out of scope for AcquireError. These
    // variants exist here because the local-Linux `managed` mode
    // negotiates with an in-host contextfs broker over a UDS, and
    // each is the broker's authoritative response to a misconfigured
    // pi-rs side. Kept on `AcquireError` (not split) so callers can
    // pattern-match a single error type per acquire.
    //
    // Mapping is 1:1 with contextfsd's StartError taxonomy
    // (crates/contextfsd/src/lib.rs:80-225); RFD-0025 §C.1 dropped
    // an earlier `BrokerTenantModeMismatch` variant — that case is
    // covered by `BrokerVmIdRequired` below — and added two new
    // variants from RFD-0024 PR 3 (`BrokerProtocolTooOld`,
    // `AuditInstanceClosedOnBroker`).
    #[error("contextfs broker rejected with master_epoch_too_old (configured epoch={configured_epoch}); refresh required")]
    BrokerMasterEpochTooOld { configured_epoch: u32 },
    #[error("contextfs broker requires vm_id for tenant {tenant_id} but daemon was started without one (config error; legacy-no-vm_id mode rejected)")]
    BrokerVmIdRequired { tenant_id: String },
    #[error("contextfs broker oidc denial: {reason}")]
    BrokerOidcRejected { reason: String },
    #[error("contextfs broker at {broker_socket} doesn't speak the daemon's wire protocol: {error}; upgrade broker to >= commit 6594c3f")]
    BrokerProtocolTooOld { broker_socket: PathBuf, error: String },
    #[error("contextfs broker has closed daemon_instance_id={daemon_instance_id} at seq={closed_at_seq}; rotate daemon_instance.id and restart")]
    AuditInstanceClosedOnBroker { daemon_instance_id: String, closed_at_seq: u64 },
    #[error("pool capacity exhausted (max={max})")]
    PoolExhausted { max: u32 },
}

#[derive(Debug, thiserror::Error)]
pub enum ExecuteError {
    #[error("guest tool failed: {0}")]
    GuestToolFailed(#[source] anyhow::Error),
    #[error("vsock RPC failure: {0}")]
    Rpc(#[source] anyhow::Error),
    /// Host-side wall-timeout overrun: the **worker itself** missed
    /// the deadline. Tool-level timeouts are NOT this error — they
    /// are returned as `Ok(VmExecution)` with
    /// `tool_result.is_error = true` (the worker drained the child
    /// per §"Timeout hygiene", reported `[exit 124]` in
    /// `model_output`, and is idle). `CallLimit` only fires when
    /// `wall_timeout + 1s` elapses without any worker response —
    /// the worker is unresponsive, the VM is suspect, the host
    /// hard-kills it. Always paired with destroy.
    #[error("worker missed wall_timeout + 1s: {detail}")]
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

/// Error-path return shape that **always carries telemetry** so the
/// runtime can record `provider`/`launcher`/`pool_disposition`/
/// `reset_status`/`release_reason` on failure rows, not only on
/// success rows. v0.37 used `Result<SandboxOutcome, SandboxError>`
/// which silently dropped telemetry on the error branch — operators
/// could see *that* an acquire failed but not which launcher,
/// whether a VM had been booted then destroyed, etc.
///
/// The provider's `execute_tool` now returns
/// `Result<SandboxOutcome, SandboxFailure>`. `SandboxFailure`
/// carries the same `SandboxTelemetry` shape as `SandboxOutcome`,
/// with all non-applicable fields set to `None`. The runtime emits
/// one `SandboxAction` row per tool call regardless of outcome,
/// populated from whichever arm fired.
#[derive(Debug)]
pub struct SandboxFailure {
    pub error: SandboxError,
    pub telemetry: SandboxTelemetry,
}

impl From<SandboxError> for SandboxFailure {
    /// Convenience for sites that have nothing to fill in beyond
    /// `provider`. Used at the very-early reject paths
    /// (Unavailable / ToolUnavailable) where no VM was acquired.
    fn from(error: SandboxError) -> Self {
        SandboxFailure {
            error,
            telemetry: SandboxTelemetry {
                provider: "microvm",
                launcher: None,
                acquire_to_ready_ms: None,
                guest_duration_ms: None,
                cold_boot: None,
                cost_usd: None,
                pool_disposition: None,
                reset_status: None,
                release_reason: None,
            },
        }
    }
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
    /// Aggregate budget across the worker's captured `stdout` +
    /// `stderr` (counted in bytes after UTF-8 truncation at the
    /// last valid char boundary). `model_output` is derived FROM
    /// that already-capped capture (e.g. `stdout + "\n" + stderr +
    /// "[exit N]"` for bash) — it is not budgeted independently.
    /// `display` is excluded from the cap (it carries small
    /// structured metadata; image `display.base64` is bounded by
    /// the image-read tool's own limits, not this one). Default
    /// 256 KiB. Per-call truncation is applied by the worker
    /// before the wire response is sent; the wire shape is
    /// already-truncated.
    pub max_output_bytes: u32,
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

    /// Release the VM. Returns a structured `ReleaseOutcome` so the
    /// caller can record the actual pool-vs-destroy decision and
    /// (when applicable) the reset-choreography result on the
    /// telemetry row. v1.0 in pooled mode = return to pool on
    /// `ExecuteOutcomeHint::Clean` AND a successful reset;
    /// non-pooled or any other input = shutdown. Errors during
    /// release are surfaced via `ReleaseOutcome.reset_status` /
    /// `reason`, not via panic — release is the cleanup path and
    /// must not unwind.
    async fn release(self: Box<Self>, exec_outcome: ExecuteOutcomeHint) -> ReleaseOutcome;

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
/// **Note:** `Clean` is a *necessary* condition for pool return,
/// not sufficient. Actual pool reuse requires the launcher's
/// per-call reset choreography (§"Post-call hygiene") to succeed
/// AND the worker to reconnect on the data-plane vsock port. The
/// final disposition is reported on `ReleaseOutcome`.
pub enum ExecuteOutcomeHint {
    Clean,                              // pre-reset hygiene + tool success → eligible for pool return iff reset succeeds
    SuspectGuestState,                  // tool panic / RPC midstream → destroy
    Cancelled,                          // ctx cancellation → destroy (state unknown)
}

/// Structured result of `release()`, recorded on the telemetry row
/// so operators can see which calls actually warmed the pool, which
/// destroyed VMs because of suspect state, and which destroyed
/// because the launcher-side reset choreography failed.
pub struct ReleaseOutcome {
    /// Did this VM survive into the warm pool, or was it destroyed?
    pub pool_disposition: PoolDisposition,
    /// Outcome of the per-call filesystem-reset choreography.
    /// `NotApplicable` for any guest-VM path that skipped reset
    /// (macOS/Windows v1 destroy-only; suspect/cancelled releases
    /// — those kill the VM without attempting reset). On the
    /// `SandboxTelemetry` row this maps to
    /// `Some(ResetStatus::NotApplicable)` so operators can
    /// distinguish "VM existed but no reset was attempted" from
    /// "no VM at all" (local-process — those rows carry
    /// `reset_status = None`).
    pub reset_status: ResetStatus,
    /// Short human-readable reason; populated only when the
    /// disposition was non-default. Surfaces in `pi sandbox doctor
    /// --recent` and in session-log post-mortems.
    pub reason: Option<String>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum PoolDisposition { ReturnedToPool, Destroyed }

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ResetStatus { Ok, Failed, NotApplicable }

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
    /// Provider-level configuration for the per-VM search-channel
    /// async task that handles `web_search` requests from inside
    /// the guest (§"web_search via vsock proxy"). Holds the host's
    /// `pi-tools-net` API credentials + per-VM rate-limit policy.
    /// The actual per-VM task is owned by `VmHandle` (so its
    /// lifetime matches the VM exactly): `acquire()` connects
    /// host→guest on `VSOCK_SEARCH_PROXY_PORT = 5003` and spawns
    /// the channel-handler task before returning; `release()`
    /// (destroy path) joins it. v1 ships `BuiltinSearchConfig`
    /// reading credentials from the host's existing pi auth
    /// storage. This is the ONLY host-side egress the guest can
    /// reach; adding another "must-be-on-host" tool requires its
    /// own dedicated narrow vsock-proxied protocol.
    search_config: Arc<SearchConfig>,
    /// Plan-time dispatch-class registry. Threaded from
    /// `RuntimeConfig` at provider construction so the planner and
    /// the provider's defense-in-depth assert (in `execute_tool`)
    /// share one source of truth. Maps tool name →
    /// `ToolDispatchClass`. Reaching the provider with a
    /// `RuntimeNative` tool would be a runtime bug, not a sandbox
    /// concern — the assert catches it.
    dispatch_class_registry: DispatchClassRegistry,
}

/// Frozen, owned map from tool name → ToolDispatchClass. Built once
/// at provider construction by walking the existing
/// `pi_tools::ToolRegistry` and asking each tool for its
/// `Tool::dispatch_class()` (the new method on the `Tool` trait).
/// Stored as a `BTreeMap<String, ToolDispatchClass>` for
/// deterministic iteration order in tests. Not a trait — there's
/// no v1 polymorphism need; if a future provider grows different
/// dispatch rules, `MicroVmProvider`'s registry stays a frozen map
/// and a separate provider type carries its own.
pub struct DispatchClassRegistry {
    inner: std::collections::BTreeMap<String, ToolDispatchClass>,
}

impl DispatchClassRegistry {
    /// Construct from the active `ToolRegistry` (RuntimeConfig
    /// already owns one). Each tool's `Tool::dispatch_class()`
    /// determines its entry. Tools not in the registry default to
    /// `Unavailable` per the safety rule in §"Plan-time API".
    pub fn from_tools(reg: &pi_tools::ToolRegistry) -> Self { /* iterate + collect */ unimplemented!() }
    pub fn lookup(&self, tool_name: &str) -> ToolDispatchClass {
        // Two-enum design clarification: `ToolDispatchClass` only
        // has `RuntimeNative` | `SandboxManaged` (orthogonal axis
        // — does the runtime handle it, or does the sandbox?).
        // Availability is a SEPARATE query on the provider via
        // `SandboxToolDisposition` (`Guest` | `Unavailable`). For
        // an unknown tool reaching this registry, the safe answer
        // is `SandboxManaged` (we don't know it's runtime-native;
        // assume not). The provider's `tool_disposition()` then
        // returns `Unavailable` per the §"Plan-time API" safety
        // default, and the unified match in `execute_tool` raises
        // `ToolUnavailable`. So unknown tools cannot reach guest
        // dispatch — but the gating happens via `tool_disposition()`,
        // not via this registry.
        self.inner.get(tool_name).copied().unwrap_or(ToolDispatchClass::SandboxManaged)
    }
}

// A parity test (`crates/pi-sandbox/tests/dispatch_class_parity.rs`)
// asserts that for every tool registered in `ToolRegistry::with_defaults()`,
// `DispatchClassRegistry::from_tools(&reg).lookup(name)` matches
// `Tool::dispatch_class()`. Drift is impossible when the registry
// is built from the same source; the test catches future
// regressions if `DispatchClassRegistry` ever grows a parallel
// initialization path.

/// Per-provider config for the host-side search channel handler.
/// One instance shared across all VMs from a given provider; the
/// rate-limit state is held inside each VM's channel-handler task.
pub struct SearchConfig {
    /// API credentials inherited from the host's `pi-tools-net`
    /// auth (same as `local-process` web_search uses).
    pub auth: Arc<dyn pi_tools_net::SearchAuthProvider>,
    /// Per-VM rate limit. Default 30 calls / 60s window.
    pub rate_limit: RateLimitPolicy,
}

#[derive(Clone, Copy)]
pub struct RateLimitPolicy {
    pub max_calls: u32,
    pub window: Duration,
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
    ) -> Result<SandboxOutcome, SandboxFailure> {
        // Runtime-native orchestration tools (task / subagent — anything
        // that drives the agent's outer loop, per RFD 0005) bypass
        // SandboxProvider entirely BEFORE we get here. The runtime
        // dispatcher checks `tool.dispatch_class()` first and never
        // calls SandboxProvider::execute_tool for `RuntimeNative`
        // tools (task/subagent/etc.). The provider is constructed
        // with a `dispatch_class_registry: DispatchClassRegistry`
        // that maps tool name → ToolDispatchClass; the assert below
        // is defense-in-depth only. Reaching this branch is a
        // runtime bug, not a sandbox concern. The registry is the
        // same one the runtime's planner uses, threaded into
        // `MicroVmProvider::new()` from `RuntimeConfig` to keep
        // both surfaces in sync.
        debug_assert!(self.dispatch_class_registry.lookup(tool_name) != ToolDispatchClass::RuntimeNative,
            "RuntimeNative tool {tool_name} should never reach SandboxProvider");
        // Unified disposition check. Every tool that reaches this
        // branch is `SandboxManaged`; we then ask the provider's
        // own classification: Guest (proceed) vs Unavailable
        // (return ToolUnavailable). This replaces v0.35's bespoke
        // `monitor` branch with the general mechanism.
        match self.tool_disposition(tool_name) {
            SandboxToolDisposition::Guest => { /* fall through to guest dispatch */ }
            SandboxToolDisposition::Unavailable => {
                return Err(SandboxFailure::from(SandboxError::ToolUnavailable {
                    tool: tool_name.into(),
                    reason: format!("unavailable under provider '{}'", self.name()),
                }));
            }
        }
        // Every Guest tool routes through the same guest dispatch path.
        // web_search is a Guest tool (its in-worker handler proxies
        // via the in-guest bridge daemon out to the host's per-VM
        // async task); from here it's indistinguishable from bash/read/etc.
        let spec = self.spec_for(ctx);
        let limits = self.build_call_limits(ctx, tool_name);
        let vm = self.launcher.acquire(&spec).await
            .map_err(|e| SandboxFailure {
                error: SandboxError::Acquire(e),
                // Acquire failed: no launcher succeeded fully, so
                // launcher name is still meaningful (which path was
                // attempted). No VM ever existed, so pool/reset are
                // None.
                telemetry: SandboxTelemetry {
                    provider: "microvm",
                    launcher: Some(self.launcher.launcher_name()),
                    acquire_to_ready_ms: None,
                    guest_duration_ms: None,
                    cold_boot: None,
                    cost_usd: None,
                    pool_disposition: None,
                    reset_status: None,
                    release_reason: None,
                },
            })?;
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
        // Tool-level timeouts (worker drained the child, returned a
        // 124-style ToolResult) come back as Ok(VmExecution) with
        // tool_result.is_error = true. Pool-vs-destroy is then a
        // function of the worker's post_call_state (the per-call
        // hygiene + filesystem reset proof).
        //
        // Worker-level timeouts (worker itself missed
        // wall_timeout + 1s) are Err(CallLimit) → always destroy:
        // the VM is unresponsive and we cannot prove guest state.
        // RPC errors → destroy for the same reason.
        let host_hint = match &exec_result {
            Ok(_) => ExecuteOutcomeHint::Clean,
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
        let release_outcome = guard.release(outcome_hint).await;  // disarms Drop; structured result
        let exec = exec_result.map_err(|e| SandboxFailure {
            error: SandboxError::Execute(e),
            // Execute failed mid-flight: VM did exist; release_outcome
            // already carries pool_disposition/reset_status/reason.
            telemetry: SandboxTelemetry {
                provider: "microvm",
                launcher: Some(self.launcher.launcher_name()),
                acquire_to_ready_ms: None,    // we have it from the prior acquire but pseudocode keeps it terse
                guest_duration_ms: None,
                cold_boot: None,
                cost_usd: None,
                pool_disposition: Some(release_outcome.pool_disposition),
                reset_status: Some(release_outcome.reset_status),
                release_reason: release_outcome.reason.clone(),
            },
        })?;
        Ok(SandboxOutcome {
            tool_result: exec.tool_result,
            execution: exec.execution,        // raw stdout/stderr/exit_status, separate from model_output
            telemetry: SandboxTelemetry {
                provider: "microvm",
                launcher: Some(self.launcher.launcher_name()),  // "firecracker" | "vfkit" | "cloud-hypervisor"
                acquire_to_ready_ms: Some(exec.acquire_to_ready_ms),
                guest_duration_ms: Some(exec.guest_duration_ms),
                cold_boot: Some(exec.cold_boot),
                cost_usd: None,
                pool_disposition: Some(release_outcome.pool_disposition),
                reset_status: Some(release_outcome.reset_status),
                release_reason: release_outcome.reason,
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
    pub async fn release(mut self, hint: ExecuteOutcomeHint) -> ReleaseOutcome {
        match self.vm.take() {
            Some(vm) => vm.release(hint).await,
            None => ReleaseOutcome {
                // Should never reach here on the happy path — taken
                // only if the guard was already disarmed. Treat as
                // destroyed-with-no-reset for telemetry honesty.
                pool_disposition: PoolDisposition::Destroyed,
                reset_status: ResetStatus::NotApplicable,
                reason: Some("release-guard-double-take".into()),
            },
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

// `search_config` carries provider-level config (auth + rate
// limits) for the per-VM search channel. The actual channel is
// owned by VmHandle: launcher.acquire() opens host→guest vsock to
// port 5003 + spawns an in-process async task that owns the
// connection for the VM's lifetime. The task reads
// WebSearchRequest, calls pi-tools-net, writes WebSearchResponse.
// On release(destroy) the task is joined. No separate binary;
// no host listening sockets. The seccomp filter on bash spawns
// blocks AF_VSOCK so tool subprocesses can't bypass.

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
    pub acquire_to_ready_ms: Option<u32>,// None for local-process, Some for microvm/remote
    pub guest_duration_ms: Option<u32>,
    pub cold_boot: Option<bool>,
    pub cost_usd: Option<f64>,           // remote-backend per-call billing
    /// Did the VM survive into the warm pool, or was it destroyed?
    /// Set from `ReleaseOutcome.pool_disposition`. None for
    /// local-process (no VM).
    pub pool_disposition: Option<PoolDisposition>,
    /// Outcome of the per-call filesystem-reset choreography
    /// (§"Post-call hygiene"). Set from `ReleaseOutcome.reset_status`.
    /// `None` only when no VM existed at all (local-process).
    /// Guest-VM paths populate this even when no reset was
    /// attempted: macOS/Windows v1 destroy-only +
    /// suspect/cancelled releases set `Some(NotApplicable)`. This
    /// asymmetry lets operators distinguish "no VM" from "VM, no
    /// reset" without inspecting other fields.
    pub reset_status: Option<ResetStatus>,
    /// Short human-readable reason; populated only when the
    /// disposition was non-default (destroy due to suspect state,
    /// reset failure, etc.).
    pub release_reason: Option<String>,
}
```

The pool ownership rule is **the launcher owns the pool**. This means:

- One `MicroVmProvider` instance ↔ one launcher instance ↔ one pool.
- **Caveat for `host_cwd`-keyed isolation:** `BootSpec.host_cwd` partitions warm rings only when the runtime actually supplies *different* cwds for different subagents. On `main` today, `crates/pi-coding-agent/src/native/task/tool.rs` warns that `isolated=true` is a no-op; `task` invocations all inherit the parent's cwd. Until RFD 0006's worktree wiring is active in the task executor, the partition key collapses to one ring per provider — which is correct from the RFD's perspective but defeats the intended cross-subagent isolation in practice. Documented so operators know what protection they actually have today.
- Subagents that inherit the parent's `Arc<dyn SandboxProvider>` (via `RuntimeConfig.sandbox_provider`) **share that pool**. The pool is normatively keyed by `BootSpec` — implementation is equivalent to `tokio::sync::Mutex<HashMap<BootSpec, VecDeque<WarmVm>>>`. Acquire looks up the warm-VM ring for the requesting `BootSpec`; release puts it back into the same ring. Two subagents with different `BootSpec`s (different `host_cwd` per RFD 0006 worktree, different `env_hash`, etc.) NEVER share a warm VM. If the matching ring is empty, the launcher cold-boots an ad-hoc VM for that `BootSpec`.
- A user who wants **per-subagent pool isolation** must construct a fresh `MicroVmProvider` for each subagent runtime — explicit, not implicit. Halo's RFD 0025 supervisor will configure this; documented in the halo integration notes.

This was previously misstated in v0.2's Open Question #2 ("each subagent's runtime gets its own MicroVmProvider"); v0.3 corrects it.

Telemetry rows extend the existing `SessionEntryKind::SandboxAction` from RFD 0022. The schema decision is **one union struct with all-optional new fields** (rather than splitting into `Local`/`Remote` variants), because it lets `pi-stats::aggregate::by_sandbox_provider()` keep its current rollup shape without per-variant code paths, and because all "new" fields are independently meaningful (a local pool-miss telemetry row has a `cold_boot` but no `cost_usd`; a remote E2B row has the inverse).

```rust
SandboxAction {
    provider: String,           // "microvm" | "local-process" | "e2b" | "sprites" | "daytona"
    tool_name: String,
    duration_ms: u64,           // total host-observed; sum of acquire + guest, or elapsed for local-process
    exit_status: i32,
    is_error: bool,
    // NEW (this RFD — local microVM, two-field split per v0.33):
    #[serde(default, skip_serializing_if = "Option::is_none")]
    launcher: Option<String>,   // "firecracker" | "vfkit" | "cloud-hypervisor" (microvm only)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    acquire_to_ready_ms: Option<u32>,  // host-observed time-to-first-byte (None for local-process)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    guest_duration_ms: Option<u32>,    // measured INSIDE the guest (Some for microvm; None for local-process, remote)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    cold_boot: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pool_disposition: Option<PoolDisposition>,  // returned_to_pool | destroyed
    #[serde(default, skip_serializing_if = "Option::is_none")]
    reset_status: Option<ResetStatus>,          // ok | failed | not_applicable
    #[serde(default, skip_serializing_if = "Option::is_none")]
    release_reason: Option<String>,             // populated only on non-default dispositions
    // NEW (RFD 0026 — remote):
    #[serde(default, skip_serializing_if = "Option::is_none")]
    cost_usd: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    round_trip_ms: Option<u32>,
}
```

The new fields are added as an **amendment to RFD 0022** (which is currently marked Implemented v1.0 — adding optional fields is non-breaking; existing telemetry rows deserialize fine because of `#[serde(default)]`). RFD 0022's revision history will be appended with an `(amended by RFDs 0023 + 0026)` note when those RFDs land. `pi-stats::ingest` adds the following nullable columns to the `sandbox_actions` SQLite table — one per new struct field: `launcher TEXT`, `acquire_to_ready_ms INTEGER`, `guest_duration_ms INTEGER`, `cold_boot INTEGER`, `pool_disposition TEXT`, `reset_status TEXT`, `release_reason TEXT`, `cost_usd REAL`, `round_trip_ms INTEGER`.

**JSONL backward compatibility** — pre-amendment rows lack the new fields; `#[serde(default)]` makes them deserialize as `None`, and `pi-stats::ingest` writes `NULL` to the new columns. **Migration ordering**: schema migration (`ALTER TABLE sandbox_actions ADD COLUMN ...` × 9 new) MUST run before any pi binary speaking the new fields ingests rows; otherwise the binary will refuse to start with `schema_migration_required`. v1 ships an integration test (`tests/sandbox_action_compat.rs`) that loads a fixture of pre-amendment rows and asserts both the old binary's rows are still readable AND the new binary's rows have the new fields populated. The `provider` field already exists on the struct in RFD 0022 and is not new here. (v0.33 dropped `dispatch_path` from the v0.32 schema — there's no longer a meaningful host-direct/guest split. Pre-amendment rows trivially deserialize regardless.)

**Public API impact.** Two surfaces, two audiences:

- **`SandboxProvider` implementers** (downstream embedders, alt-providers): source-breaking changes are `execute_tool(ctx, tool_use_id, tool_name, tool_input) -> Result<SandboxOutcome, SandboxFailure>` — the `tool_use_id: &str` parameter is new, and the error arm now carries telemetry instead of a bare `SandboxError` — and `SandboxOutcome` replaces RFD 0022's execution-only return (the previous `SandboxExecution` raw `stdout`/`stderr`/`exit_status` shape). Every `impl SandboxProvider for X` must adapt.
- **`MicroVmLauncher` implementers** (anyone shipping a new launcher backend): source-breaking change is `VmHandle::release(self, hint) -> ReleaseOutcome` (was `-> ()`). `SandboxProvider` implementers do NOT see this — `release()` is a launcher-internal trait.

`SandboxAction` / `SandboxTelemetry` field additions are backward-compatible serde/schema expansion (new optional fields with `#[serde(default)]`). Pre-amendment JSONL/SQLite rows deserialize cleanly; new rows populate the new fields.

`pi-sdk`'s `MockSandboxProvider` updates to return a synthetic `ReleaseOutcome { pool_disposition: ReturnedToPool, reset_status: NotApplicable, reason: None }` so existing test code keeps compiling without touching call sites. The two trait changes ride a pi-sdk MINOR bump (still 0.x); both `SandboxProvider` and `MicroVmLauncher` are documented as **unstable / sealed-by-convention** until `pi-sdk` 1.0, with `LocalProcessProvider`, `MicroVmProvider`, and `MockSandboxProvider` as the blessed in-tree implementers. Other downstream impls run at their own risk pre-1.0; the changelog calls out the break clearly.

### 3. The local microVM contract

#### Guest rootfs (one artifact, every host)

- alpine 3.19+ minirootfs as the base (~6 MB).
- `pi-sandbox-worker` binary (statically linked against musl, ~6–8 MB) at `/usr/local/bin/pi-sandbox-worker`.
- An init script at `/init` that mounts `/proc`, `/sys`, `/dev/vsock`, parses `/proc/cmdline` for `pi.proto_version=N` and halts on mismatch with a fatal diagnostic to the serial console, then **execs `pi-cfs-init`**. `pi-cfs-init` is the sole PID 1 and supervises every other process in the rootfs (see §"web_search via vsock proxy" — Guest process tree, and §3.5.9 — pi-cfs-init responsibilities). It launches `pi-vm-search-bridge` and `pi-sandbox-worker` as long-lived children, supervises both, and re-execs the worker on reset.
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

**Post-call hygiene (background daemon AND filesystem-reset problems).** v1 microvm does **not** support cross-call background daemons or services, AND does **not** preserve any guest-local writable state outside `/work` across calls. After every guest tool call — successful, errored, or timed out — the worker MUST run a hygiene probe before signaling the host that the VM is idle. The probe verifies:

1. **Process subtree empty.** Every descendant the per-call tool spawned (the `bash` shell, anything it forked) is gone. Implementation: bash runs as the leader of a fresh process group; the worker reaps it, then `kill(-pgid, 0)` returns `ESRCH`. On Linux the worker additionally consults a per-call cgroup v2 (`cgroup.procs` empty) — the cgroup is the authoritative test because a daemonized child reparented to PID 1 still appears in the cgroup. macOS/Windows use process-group reparenting detection as a coarser fallback (a known v1 gap; documented).
2. **Worker transient state cleared.** Per-call temp dirs (`$TMPDIR`/<call_id>) removed; per-call open file descriptors closed; `$PWD`/env scrubbed.
3. **Guest-local filesystem reset to baseline (launcher-owned via `pivot_root`).** The rootfs image is attached as a **read-only** block device by the launcher. The guest's `/init` mounts an initial overlay union for `/`: `lowerdir=<read-only-rootfs>`, `upperdir=/run/pi-overlay/upper`, `workdir=/run/pi-overlay/work`, both on a launcher-supplied tmpfs. **The reset cannot mutate the upperdir/workdir of a live overlay underneath it** — overlayfs's metadata cache makes that unsafe. Instead the reset performs a full root switch:

   1. After the worker reports `post_call_state` over the data-plane vsock (port 5001), the launcher issues a reset RPC on the **separate control-plane vsock port 5002**.
   2. The in-guest reset agent (`/sbin/pi-vm-reset`, statically compiled, owned by Commit B) creates `/run/pi-newroot` on a fresh tmpfs, mounts a brand-new overlay there with the same read-only lowerdir and a new pair of tmpfs upperdir/workdir, then bind-moves the mounts that MUST survive (`/proc`, `/sys`, `/dev`, `/dev/vsock`, `/work`, `/run/contextfs`, **`/run/pi-bridge`**) into the new root via `move_mount`/`mount --move`. The `/run/pi-bridge` survival is what keeps the search-bridge UDS (`/run/pi-bridge/search.sock`) reachable across reset cycles — without it the worker's UDS connect would fail after the first reset.
   3. `pivot_root /run/pi-newroot /run/pi-newroot/.old-root` switches the kernel's view of `/`.
   4. The agent re-execs the worker (the new `pi-sandbox-worker` PID is parented to PID 1 of the new root), then `umount -l /.old-root` lazy-unmounts the old overlay so its upperdir/workdir tmpfs instances are reaped after the kernel finishes its references.
   5. The new worker connects back to the host on port 5001; only after the host sees that connection is the VM marked `Clean` and eligible for pool return. Until then `release()` blocks.

   Any `bash` write to `/root/poison`, `/usr/local/bin/poison`, `/etc/passwd`, or any path outside `/work`/`/run/contextfs` lived in the old overlay's upperdir tmpfs and is gone after step 4. The ONLY surviving mutation is what the tool wrote to `/work` (host-mediated workspace) — the `read`/`write`/`edit` contract.

   **Reset-failure observability + fallback.** If any of steps 1–5 fails (control-plane RPC times out, `pivot_root` returns `EBUSY`, agent absent in older rootfs versions, the new worker doesn't reconnect within `reset_timeout_ms` default 2000 ms), the launcher (a) writes `PI_FAIL: reset-failed step=<N> errno=<E>` to the guest's serial console (mirrors the boot-failure path in §3.5.9), (b) records `SandboxAction.pool_disposition = "destroyed"`, `reset_status = "failed"`, `release_reason = "reset-failed step=<N> errno=<E>"`, and (c) destroys the VM. The pool is replenished asynchronously. There is no "best-effort partial reset" path — silent fall-through would re-introduce the leak.

   **Required negative tests** (Commit D integration suite):
   - `bash 'echo poison > /root/marker'` → next acquire on the same `BootSpec` ring (harness asserts `cold_boot=false` and that the launcher reused a specific warm-pool VM ID) finds no `/root/marker`.
   - `bash 'install -m755 /usr/bin/true /usr/local/bin/poison && /usr/local/bin/poison'` (forces upperdir copy-up of a system path; running it proves the file existed) → next acquire's `ls /usr/local/bin/` does NOT contain `poison`.
   - `bash 'echo bad >> /etc/passwd'` → next acquire's `cat /etc/passwd` matches the baseline image bit-for-bit.
   - Reset-failure path: simulate by stubbing the in-guest agent to return `EBUSY` → harness asserts the VM is destroyed, telemetry row has `pool_disposition="destroyed"` + `reset_status="failed"` + `release_reason` containing `"reset-failed"`, and the next acquire is a cold boot.

   macOS/Windows v1 inherit this guarantee transitively from the destroy-on-release rule (no overlay reset choreography needed; the VM itself is gone).

The worker reports the verdict on the wire as `ToolResponse.post_call_state` (`Clean` | `SuspectGuestState`). The tool itself may have succeeded; the post-call probe is what decides pool reuse. The host composes `final_hint = min(host_outcome_hint, post_call_state)` (`Clean < SuspectGuestState`) before calling `release(final_hint)`, so a daemonization leak forces destroy regardless of the tool's success bit. A missing `post_call_state` field defaults to `SuspectGuestState` — workers must *prove* cleanliness rather than implicitly assert it. The verdict is not surfaced to the model (the tool's `model_output` is unchanged); it's strictly a host/launcher signal.

**macOS / Windows v1: destroy-on-release, no pooling.** The cgroup-based probe is Linux-only. macOS and Windows launchers in v1 have a coarser `process-group orphan` check that does NOT detect `setsid()` / fully-detached daemons. Rather than ship a known false-`Clean`, the v1 normative rule for `VfkitLauncher` and `CloudHypervisorLauncher` is **always destroy on release**: the launcher's `release()` ignores `ExecuteOutcomeHint::Clean` and tears the VM down unconditionally. This costs the warm-pool latency benefit on those OSes. **No `--sandbox-microvm-pool=force` escape hatch in v1** — a knowingly-unsound pooling escape would muddy the security story for marginal gain (an earlier v0.24 draft mentioned that flag; v0.42 cuts it). The Linux/Firecracker path keeps the cgroup-based pool. A future RFD lifts the macOS/Windows restriction once each launcher has a proven-clean per-call container/cgroup analog.

**Tested cases.** The Commit D integration suite includes negative tests for: `bash 'sleep 999 &'`, `bash 'nohup foo &'`, `bash '(sleep 5; touch /work/marker) &'`, `bash 'mkdir -p /tmp/x && touch /tmp/x/leftover'`, plus the timeout path (`bash 'sleep 60'` with `timeout_ms=1000`). Each test asserts that (a) the affected VM does **not** return to the pool (telemetry row has `pool_disposition="destroyed"`), (b) the next acquire on the same `BootSpec` does NOT see the leftover process or `/tmp` residue, and (c) the row's `release_reason` contains a short rationale (e.g. `"post-call-hygiene-failed: descendant pid <N> alive in cgroup"`).

**Timeout hygiene before pool return.** Two distinct timeouts:

- **Tool-level timeout** (`tool_input.timeout_ms` exceeded). The **worker** handles cleanup in-guest: (1) `SIGTERM` to the spawned tool's process group; (2) drain stdin/stdout/stderr pipes to EOF or a 250 ms hard cutoff, whichever is first; (3) on cutoff, `SIGKILL` and continue draining; (4) emit a normal `ToolResponse` with `is_error = true`, `model_output` containing a short timeout marker, `exit_status = 124` (GNU `timeout` convention), and the post-call hygiene probe + filesystem reset already run. From the host's perspective this is `Ok(VmExecution { tool_result.is_error=true, post_call_state })` — pool reuse depends only on `post_call_state`, exactly like a normal call. **Tool-level timeouts do NOT raise `ExecuteError::CallLimit`.**
- **Worker-level timeout** (worker missed `wall_timeout + 1s`). The host has heard nothing from the worker — process probably wedged, kernel may be in trouble, guest state is unprovable. The launcher hard-kills the VM and `vm.execute()` returns `Err(ExecuteError::CallLimit { detail })`. The provider always destroys the VM in this branch; it never re-enters the pool.

This split removes the v0.27 contradiction: the typed API (`Ok` vs `Err(CallLimit)`) and the prose now describe the same pool semantics.

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
  write is mediated by contextfsd's PDP and recorded into the
  daemon's local audit chain (§3.5.4). Broker replication is
  asynchronous batched best-effort via `AuditPusher`; FUSE writes
  are NEVER gated on broker reachability.
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
  transport. Firecracker does not support virtio-fs (per
  rfd/0023-known-issues.md §"Issue 1" + upstream firecracker-microvm
  issue #1180). Use --sandbox-microvm-mode=managed (requires
  contextfs broker config) or --sandbox-provider=local-process.`

  **Note on the in-tree code.** `crates/pi-sandbox/src/microvm/firecracker.rs` on `main` still contains pre-known-issues plumbing for `virtiofsd_bin` resolution, a `virtiofsd` child process per VM, and a virtio-fs `/work` mount — that's the original Commit D shape from before the validation in `rfd/0023-known-issues.md` proved Firecracker drops the `fs` config block silently. Commit G is responsible for **removing or quarantining** that legacy path: either delete `virtiofsd_bin`/`fs_share_for` entirely, or cfg-gate it behind a `firecracker-virtiofs-experimental` feature with a stderr banner. The in-tree code is dead-on-Firecracker-v1.15.0 today; the RFD's "managed-only on Linux/Firecracker" stance is the v1 GA story.
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

#### 3.5.4 — Audit batching via `AuditPusher` (RFD-0024 PR 3)

> *Authoritative draft from the contextfs maintainer; spliced into
> v0.40 verbatim, then trimmed v2 to drop duplication with §3.5.6
> (operator force-close), §3.5.9 (failure-surface table), §3.5.10
> (caller_exe / sentinel free-surfaces). Reviewed against contextfs
> `main` @ `06e9531`.*

ContextFS PR 3 (commits `6594c3f → 06e9531`, "Embedder audit
tunnel") replaced the per-write `WriteAuditPing` pipeline with a
batched **AuditPusher** that runs as a background task in
`contextfsd`. The shape is fundamentally different from the audit-
ping it superseded: chain integrity is **local and reliable**, broker
push is **best-effort batched**, and write-class FUSE ops are never
gated on broker reachability. Pi-rs's `MicroVmProviderConfig` gains
no new fields — the AuditPusher is always-on whenever
`[broker].socket_path` is set, which §3.5.7 always sets in managed
mode.

##### Daemon-side flow (in-guest)

Every successful FUSE op (read, write, list, stat, xattr, …) lands
an HMAC-chained record in `audit_log_path`'s ndjson. A single
subscriber (the AuditPusher; subscriber count is enforced = 1 by
contextfs's broadcast primitive) consumes records, batches them,
and encrypts each batch as a `WriteLogPayload::AuditBatch` v2
envelope under the per-VM tenant secret derived in §3.5.2. The
batch ships as `Request::AuditPush` to the shared broker over the
control-plane channel (§3.5.5). Defaults: 256 records per push,
1 s coalesce window. The push is async — FUSE ops never await it.
On transport failure (broker UDS gone, timeout) the pusher drops
the in-flight batch, logs `audit_pusher_transport_dropped` with
the seq range, and continues consuming. The local audit chain
remains intact regardless; the broker sees a seq gap on the next
successful push, which the operator pane surfaces.

##### Broker-side acceptance (host-side)

The broker maintains an in-memory `AuditReplayState` keyed on
`(tenant_id, daemon_instance_id)` with a 5-row first-match-wins
acceptance table — `InstanceClosed → SeqRegression →
IdempotentRetry → NonceReuseEnvelopeMismatch → Accept`. Per-batch
HMAC verification uses the per-VM tenant secret bytes pi-rs
shipped to the broker via `--tenant-secret-path` (§3.5.6). On
accept, `high_watermark` advances to the batch's `audit_seq_max`
and the broker emits a `tracing::info!` record per accepted
record into `broker.log` carrying the daemon's
`(tenant_id, daemon_instance_id, audit_seq_min, audit_seq_max,
push_nonce)` plus the per-record `verify_write` decision fields
(see §3.5.10 for `caller_exe` / `caller_start_time`).

##### Startup handshake — `AuditResync`

At daemon boot, BEFORE any mount, the daemon dials
`Request::AuditResync` against the broker. The broker returns its
recorded `(high_watermark, closed_at_seq)` for the daemon's
`(tenant, vm_id, daemon_instance_id)` triple. The daemon then:

- Drops any local records with `seq <= high_watermark` (already
  accepted broker-side; re-pushing would produce a `SeqRegression`
  rejection).
- Resumes the chain at `high_watermark + 1`.
- Refuses to mount with typed
  `StartError::AuditInstanceClosedOnBroker` if the broker reports
  `closed_at_seq.is_some()`. Recovery: rotate
  `daemon_instance_id_path` (§3.5.7) and restart, which produces a
  fresh pair the broker treats as a new instance.

A pre-RFD-0024 broker (no `Hello`/`AuditResync` support) is caught
at the earlier `Hello` probe with typed
`StartError::BrokerProtocolTooOld` (mapped per §3.5.9). The daemon
refuses to mount rather than degrade silently.

##### Lag handling

If the daemon emits records faster than the pusher can drain (a
`cargo build` burst on a flaky broker connection is the typical
case), the bounded broadcast ring overflows. The pusher receives a
`RecvError::Lagged` event carrying
`(dropped_count, oldest_dropped_seq, newest_dropped_seq)` and
emits a chain-stamped `audit_subscribe_lagged` record into the
audit log via a no-broadcast writer (a `Weak`-backed handle that
can't recurse), rate-limited to one event per second. The chain
witness is HMAC-keyed and forge-evident; it appears in the operator
pane via the next successful AuditPush.

##### What pi-rs does NOT get from the audit-ping → AuditPusher swap

The old audit-ping `mode = "fail-closed"` knob blocked write-class
FUSE ops with `EIO` when the per-mount channel was saturated. The
AuditPusher has **no equivalent gate** — by design. The local
audit chain is the source of truth for forensic claims; broker
push is best-effort replication. RFD-0024's threat model treats
agent-blocking on broker liveness as a worse failure mode than
bounded audit-replication lag. Operators who want
broker-roundtrip-or-bust on every write build a watchdog on
`broker.log`'s `audit_pusher_transport_dropped` events plus the
operator HTTP force-close endpoint (§3.5.6) for incident response.

##### References

- contextfs RFD-0024 §"Refactor scope" items 1–6 (audit broadcast
  primitive, pusher loop, replay-state table, sentinel signing,
  AuditResync handshake, Hello probe).
- `crates/contextfs-core/src/audit_broadcast.rs` —
  single-subscriber bounded broadcast.
- `crates/contextfsd/src/audit_pusher.rs` — daemon-side batcher.
- `crates/contextfs-broker/src/audit_replay.rs` — 5-row replay
  acceptance table.
- `crates/contextfs-core/src/instance_close.rs` — sentinel HMAC
  (see §3.5.10 for the lifecycle).

#### 3.5.5 — Transport topology (Linux/Firecracker Stage 1) — two channels

The Linux Stage 1 design has **two separate cfs-mesh channels**: a
control plane (broker traffic) and a data plane (cfs-fs-server FUSE
backend traffic). One bridge/listener pair per channel; one
guest-local UDS per channel that contextfsd consumes. **The broker
is shared per pi process**, not per VM; the cfs-mesh bridge fans
multiple VMs into one broker instance using `--tenant-peer-uid`
SO_PEERCRED auth (one stable `tenant_id` per pi process; all
VMs in this process share that tenant_id and differ via `vm_id`;
the bridge process effective uid is `pi-sandbox-bridge`):

```
            ┌──────────────────────── HOST (per pi process) ─────────────────────┐
            │                                                                    │
            │   contextfs-broker --listen-uds /run/pi-rs/broker.sock             │
            │       --tenant-peer-uid <tenant_id>:<bridge_uid>                   │
            │       --tenant-mode <tenant_id>=embedder                           │
            │       --tenant-secret-path <master_secret_path>                    │
            │       (one process; one tenant; N VMs concurrently)                │
            │                                                                    │
            │   per-VM children of MicroVmLauncher (kill_on_drop):               │
            │                                                                    │
            │   for each acquired VM:                                            │
            │     cfs-fs-server --backend-root <host_cwd>                        │
            │         --socket /run/pi-vm-<vm_id>/fs.sock                        │
            │     cfs-mesh vsock-bridge --target-uds /run/pi-rs/broker.sock      │
            │         --vsock-cid <vm_cid> --vsock-port <P_b>                    │
            │     cfs-mesh vsock-bridge --target-uds /run/pi-vm-<vm_id>/fs.sock  │
            │         --vsock-cid <vm_cid> --vsock-port <P_f>                    │
            └──────────────────────────────┼─────────────────────────────────────┘
                                           │
                                           │ vsock CID = vm_id_to_cid(vm_id)
                                           │
            ┌──────────────────────────────┼─────────────── GUEST (per VM) ─────┐
            │                              ▼                                    │
            │   cfs-mesh vsock-listen --port P_b --uds /run/contextfs/broker.sock│
            │   cfs-mesh vsock-listen --port P_f --uds /run/contextfs/fs.sock   │
            │           ↑                          ↑                            │
            │           │ [broker].socket_path     │ [mount.remote_fs].target_uds│
            │   contextfsd ─── reads daemon.toml ─── mounts /work               │
            │   pi-sandbox-worker (cwd = /work)                                 │
            └───────────────────────────────────────────────────────────────────┘
```

**One shared broker per pi process** (was per-VM in earlier drafts; corrected per RFD-0025 §A and contextfs maintainer review of v0.39). Cheaper to operate (one broker process serving N VMs vs. N broker processes), matches `crates/contextfs-broker`'s native multi-tenant capability (`--tenant-peer-uid` is exactly that pattern), and the per-VM tenant secret derivation in §3.5.2 doesn't require process isolation between VMs — the broker derives the same per-VM secret from `(master, tenant_id, vm_id, master_epoch)` regardless of whether one or N broker processes are running.

**Identity model.** `tenant_id` is **stable per pi process** (operator-configured, e.g. `"pi-rs-default"` or one tenant per orchestrated workload). All VMs in one pi process share that tenant_id. **Per-VM differentiation is via `vm_id`** (RFD-0023 §5; pi-rs supplies a UUIDv4-based `pi-job-<uuid>` per VM in the daemon's `DaemonConfig`). The broker derives the per-VM secret from `(master_secret, tenant_id, vm_id, master_epoch)`. Pi-rs does NOT reconfigure broker tenants at runtime — the broker is started with stable per-tenant config; per-VM differentiation happens via `vm_id` in each daemon's TOML, verified broker-side using the shared master secret in `--tenant-secret-path`.

Host-side, per pi process (spawned at first acquire, joined at process exit):

- `contextfs-broker --listen-uds /run/pi-rs/broker.sock
   --tenant-peer-uid <tenant_id>:<bridge_uid>
   --tenant-mode <tenant_id>=embedder
   --tenant-secret-path <master_secret_path>
   --verify-write-oidc-issuer <issuer-url>
   --verify-write-oidc-audience <wi-audience>
   --verify-write-oidc-alg RS256 ...` — the shared broker.
  UDS auth is SO_PEERCRED via `--tenant-peer-uid <tenant_id>:<uid>`
  (colon separator per `<contextfs>/crates/contextfs-broker/src/main.rs:188-195`
  doc text "Format: `<tenant_id>:<uid>`"). The bridge process's
  effective uid is `pi-sandbox-bridge`. `--tenant-mode` uses the
  equals separator (different format from `--tenant-peer-uid`; see
  `<contextfs>/crates/contextfs-broker/src/main.rs:205` "Format:
  `<tenant_id>=embedder`") and opts the tenant into embedder mode
  (every request must carry `vm_id` + `master_epoch`).
  `--tenant-secret-path` points at the operator-managed master
  secret file the daemon's per-VM secret is derived from (§3.5.2);
  set ONCE at broker startup, NOT per-VM. All VMs in this pi process
  share this master secret.
- TCP-HMAC auth (`--auth-secret-path`) is **not used**; this is
  same-host only.

**No runtime broker reconfiguration.** Pi-rs's broker child is started once with the operator's stable per-tenant config and shut down once at pi process exit. Adding/removing VMs at runtime does NOT modify the broker's argv — VM lifecycle is daemon-side (each `contextfsd::start()` call carries its own `vm_id` in `DaemonConfig`); the broker accepts any valid `vm_id` for a configured tenant. Earlier draft text speculated about SIGHUP reload of the peer-uid table; that's incorrect — `contextfs-broker` has no SIGHUP handler (SIGHUP is for `contextfsd` policy reload only, per `<contextfs>/crates/contextfsd/src/lib.rs:8-33`).

Host-side, per VM (children of MicroVmLauncher, kill_on_drop):

- `cfs-fs-server --socket <run_dir>/<vm_id>/fs.sock
  --backend-root <host_cwd> --allowed-uid <bridge_uid> ...` —
  the file-server that fronts the user's cwd as the FUSE backend.
- `cfs-mesh vsock-bridge --cid <guest-cid> --port <P_b>
  --key <broker_bridge_key> --peer-pubkey-path <guest_broker_pubkey>
  --target-uds /run/pi-rs/broker.sock` — control plane bridge,
  fans this VM into the shared broker.
- `cfs-mesh vsock-bridge --cid <guest-cid> --port <P_f>
  --key <fs_bridge_key> --peer-pubkey-path <guest_fs_pubkey>
  --target-uds <run_dir>/<vm_id>/fs.sock` — data plane bridge,
  per VM (the cfs-fs-server is per VM since each VM has its own
  `host_cwd`).
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

The host-side broker is **shared across all VMs in one pi process** (per §3.5.5 corrected topology). Spawned at first acquire, joined at pi process exit. Operators can also force-close a daemon's instance via the broker's HTTP control plane (item 7a below).

```
contextfs-broker \
    --listen-uds /run/pi-rs/broker.sock \
    --tenant-peer-uid <tenant_id>:<bridge_uid>   # colon separator; format per crates/contextfs-broker/src/main.rs:188-195
    --tenant-mode    <tenant_id>=embedder        # equals separator; format per crates/contextfs-broker/src/main.rs:205
    --tenant-secret-path <master_secret_path>    # set ONCE at startup; pi-rs's per-VM secrets are daemon-side derivations
    --verify-write-oidc-issuer <issuer-url> \
    --verify-write-oidc-audience <wi-audience> \
    --verify-write-oidc-alg RS256 \
    [other flags]
```

No `--vsock*` flags (vsock termination is owned by the host-side
`cfs-mesh vsock-bridge` per §3.5.5); no `--auth-secret-path` (that
flag is the TCP-HMAC path and conflicts with the UDS topology).

`--tenant-secret-path` is the broker's master tenant secret. Pi-rs's
**daemon side** derives the per-VM secret from
`(master, tenant_id, vm_id, master_epoch)` (§3.5.2) and the broker
verifies AuditResync against the same derivation, reading the master
from this path. Set ONCE at broker startup (NOT per-VM — this is the
single-tenant flag from `<contextfs>/crates/contextfs-broker/src/main.rs:86-93`,
correctly used here because pi-rs's tenant model is one-tenant-per-pi-process
even though it's many-VMs-per-tenant). Without this flag, the broker
returns `verify_write_unavailable` on the daemon's first AuditResync
and the daemon refuses to mount.

`--tenant-mode <tenant_id>=embedder` is the operator's opt-in to
embedder mode. Without it the broker defaults to legacy mode and
refuses any request carrying `vm_id`/`master_epoch`. With it set,
every request must carry non-empty `vm_id` + `master_epoch` (typed
`BrokerVmIdRequired { tenant_id }` from `StartError` otherwise; pi-rs
maps to `AcquireError`). Pi-rs's MicroVmProvider asserts both are
set on its config at construction; absence is a hard error at
startup, not at first request.

`--tenant-peer-uid <tenant_id>:<bridge_uid>` pins SO_PEERCRED auth
on the broker UDS to the cfs-mesh-bridge's effective uid. v1.0 runs
the bridge as a dedicated `pi-sandbox-bridge` system uid; pi-rs's
provisioning sets up the uid at first run and the `MicroVmProvider`
holds the uid in its config. **No runtime reconfiguration**: the
flag is set ONCE at broker startup. Earlier draft text speculated
about SIGHUP reload of the peer-uid table; that's wrong —
`contextfs-broker` has no SIGHUP handler (SIGHUP is `contextfsd`
policy reload only). VMs come and go daemon-side; the broker
accepts any valid `vm_id` for the configured tenant without per-VM
broker reconfiguration.

**Operator force-close via HTTP control plane.** Per contextfs PR 3
part 7 (commit `655408d`), the broker exposes
`POST /tenants/<t>/daemons/<id>/instance-close` for "agent compromise
detected, kill the audit chain immediately" without waiting for VM
teardown. Pi-rs operators can hit this endpoint to mint a forced
`instance_closed` watermark; the daemon's next request will then
fail with `StartError::AuditInstanceClosedOnBroker` and pi-rs's
`AcquireError` mapping destroys the VM. Documented in `pi sandbox
doctor --runbook` for incident response.

#### 3.5.7 — Daemon TOML rendered at provisioning

The full operator-rendered `/etc/contextfsd/daemon.toml` inside the
guest, matching contextfsd's actual config shape:

```toml
tenant_id                = "tenant-a"
vm_id                    = "<pi_firecracker_uuid>"
master_epoch             = 7
tenant_secret_path       = "/var/run/cfs/tenant_secret"        # bind-mounted from host tmpfs
oidc_token_path          = "/var/run/secrets/token"             # bind-mounted WI token
audit_log_path           = "/var/log/contextfs/audit.ndjson"
daemon_instance_id_path  = "/var/lib/contextfs/daemon_instance.id"  # RFD-0024 PR 3 part 5: load-bearing across restarts; rotate to force fresh AuditResync

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
# audit_ping = { ... }   <-- REMOVED in v0.40. ContextFS PR 3 part 7 (commit 655408d)
#                            deleted the audit_ping mechanism and replaced it with always-on
#                            batched AuditPusher. TOML still containing this field will be
#                            REJECTED at daemon startup by `#[serde(deny_unknown_fields)]`.
#                            See §3.5.4 for the replacement.

  [mount.remote_fs]
  target_uds = "/run/contextfs/fs.sock"              # guest-local UDS, exposed by data-plane cfs-mesh vsock-listen (§3.5.5)
```

**Note on `daemon_instance_id_path`.** The default
(`<audit_log_path>.parent/daemon_instance.id`) works, but explicit
configuration lets operators rotate the file deterministically.
**Rotating the daemon_instance.id file forces a fresh AuditResync**
— the broker treats the new instance id as a fresh
`(tenant, daemon_instance_id)` pair with no prior watermark. Useful
for warm-pool VMs after master_epoch rotation, and for incident
response (operator force-close via the HTTP control plane in §3.5.6
mints a closed-watermark for the *current* instance id; next acquire
boots a new instance with a rotated file).

Commit G's `MicroVmProvider` renders this from a
`MicroVmProviderConfig` struct at construction. The TOML schema is
that of contextfsd; the canonical source is
`<contextfs>/crates/contextfsd/src/config.rs` and Commit G adds a
serde-aligned struct on the pi-rs side that fails loud on schema
drift. **Drift detection is runtime** (`#[serde(deny_unknown_fields)]`
catches new fields contextfsd added; missing-required-field catches
fields contextfsd removed; the git-rev pin per §3.5.8 catches
breaking changes when pi-rs's CI bumps its contextfs commit). It's
NOT compile-time — the config lives in a sibling repo with
independent semver, so a "broken match → compile-time error"
promise would be wrong. The guarantee is "fail-loud-at-config-load
before any guest is booted", which is enough for v1; combined with
the pinned-compat integration test (`tests/contextfs_schema_pin.rs`,
gated on `CONTEXTFS_REPO_PATH`) that runs in CI when both repos are
present.

#### 3.5.8 — Version compatibility

Per RFD-0025 §C.2: contextfs's `Cargo.toml` is at `version =
"0.0.1-dev"` with no git tags. **Pi-rs Commit G pins by git
revision** until contextfs cuts its first tagged release:

```toml
[dependencies]
contextfsd  = { git = "https://github.com/giuseppemassaro/contextfs", rev = "<commit-sha>", default-features = false, features = ["remote-fs"] }
```

When contextfs cuts `v0.1.0` (or whatever number is right), pi-rs
flips the pin to a `version = "X.Y.Z"` constraint and adds a CI
constraint check.

**Pre-RFD-0024 brokers (any contextfs build before commit
`6594c3f`) surface as `StartError::BrokerProtocolTooOld
{ broker_socket, error }`** from the daemon's `Hello` probe (RFD-0024
§"Refactor scope" item 6 mixed-fleet rollout signal). The daemon
refuses to mount; pi-rs's `AcquireError` mapping destroys the VM
and reports the operator action ("upgrade contextfs broker to
≥ commit 6594c3f"). No silent degradation.

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
| `/usr/local/bin/contextfsd`                    | contextfs static-musl build, pinned to the same git rev declared in `Cargo.toml` per §3.5.8 | FUSE mount + verify_write loop |
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
4. Polls readiness **without mutating `/work`**: (a) `statfs("/work")`
   returns `FUSE_SUPER_MAGIC`; (b) a control-plane health RPC to
   contextfsd's `/run/contextfs/broker.sock` returns `READY`. The
   user workspace is **never** written to as part of boot — boot/
   readiness MUST be observable without modifying `/work`. The
   readiness gate file (`/run/pi-cfs/.ready`, on the guest's tmpfs
   root, NOT `/work`) is what later steps wait on.
5. Spawns `pi-sandbox-worker` with `cwd = /work` only after step 4's
   `/run/pi-cfs/.ready` sentinel exists. This is the existing
   worker; no changes beyond the cwd.
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

**Boot must not mutate `/work`.** The readiness sentinel lives on the
guest's private tmpfs root (`/run/pi-cfs/.ready`), NOT on the user's
workspace mount. Earlier drafts (≤v0.25) wrote `/work/.cfs-ready` and
relied on its appearance to prove FUSE-up + broker-acceptance; that
silently mutated the host's session cwd at every boot. v0.26+ proves
readiness via the `statfs(/work) == FUSE_SUPER_MAGIC` check plus a
control-plane health RPC to contextfsd. The host's session cwd is
read-only-clean across boot.

Failure surface visible to `MicroVmProvider`:

| Trigger                                                  | `AcquireError` variant            |
| -------------------------------------------------------- | --------------------------------- |
| `cfs-mesh vsock-listen` fails (vsock CID/port collision) | `HostTransportSetup`              |
| `contextfsd` exits non-zero on startup                   | `GuestDaemonStartFailed { exit }` |
| `StartError::BrokerMasterEpochTooOld { configured_epoch }` from contextfsd | `BrokerMasterEpochTooOld { epoch }` (eviction per §3.5.2) |
| `StartError::BrokerVmIdRequired { tenant_id }` from contextfsd (covers what earlier drafts called `tenant_mode_legacy_no_vm_id`) | `BrokerVmIdRequired { tenant_id }` (operator alert: missing `vm_id` config) |
| `StartError::BrokerOidcRejected { reason }` from contextfsd | `BrokerOidcRejected { reason }`   |
| `StartError::BrokerProtocolTooOld { broker_socket, error }` from contextfsd (RFD-0024 §"Refactor scope" item 6 mixed-fleet rollout signal — broker is too old to speak the daemon's wire protocol) | `BrokerProtocolTooOld { broker_socket, error }` (operator alert: upgrade contextfs broker to ≥ commit `6594c3f`) |
| `StartError::AuditInstanceClosedOnBroker { daemon_instance_id, closed_at_seq }` from contextfsd (RFD-0024 §"Replay protection" — broker has marked this instance closed; operator force-close, sentinel emission, or 16-week TTL aged out) | `AuditInstanceClosedOnBroker { daemon_instance_id, closed_at_seq }` (operator alert: rotate `daemon_instance.id` per §3.5.7 + RUNBOOK; restart) |
| `/run/pi-cfs/.ready` not written within `boot_timeout`   | `ReadyTimeout { boot_timeout, last_dmesg }` |

**`BrokerTenantModeMismatch` removed.** v0.39 listed a row for this
variant; per RFD-0025 §C.1 + the contextfs maintainer review of v0.39
the variant doesn't exist in contextfs's shipping `StartError` enum —
the legacy-no-vm_id case is covered by `BrokerVmIdRequired { tenant_id }`
above. Pi-rs's `AcquireError` taxonomy drops `BrokerTenantModeMismatch`
in v0.40.

Schema-drift detection: pi-rs's `MicroVmProviderConfig` has a
`#[serde(deny_unknown_fields)]` mirror of contextfsd's `DaemonConfig`
shape. **Drift surfaces at TOML deserialize time** when the user's
contextfs-version pin changes — not as a compile-time error (the
config lives in a sibling repo with independent semver). v1 ships an
integration test (`tests/contextfs_schema_pin.rs`) that
`serde::from_str`s the rendered TOML against contextfsd's actual
config struct, gated on a `CONTEXTFS_REPO_PATH` env var so CI can
opt in.

#### 3.5.10 — Free-surface mentions from RFD-0024 PR 3

Two contextfs-side surfaces pi-rs's Linux Stage 1 inherits at zero
implementation cost:

- **Kernel-attested `caller_exe` + `caller_start_time` in `broker.log`**
  (contextfs commit `358bcb3`). Every `verify_write` decision the
  broker emits to `broker.log` now carries `caller_exe` (kernel
  symlink-target via `/proc/<pid>/exe`, not `comm`-spoofable) and
  `caller_start_time` (boot-relative process birth-tick). When pi-rs
  ingests `broker.log` (an operator-managed file separate from
  `SandboxAction` JSONL), these two fields can be cross-referenced
  against pi-rs's per-VM telemetry to confirm "the verify_write the
  broker observed came from this VM's daemon process, not a
  same-uid impostor". v1 surfaces this as an optional read-side
  enrichment in `pi --stats sandbox-actions`; not in the hot path.
- **`instance_closed` sentinel on graceful shutdown** (RFD-0024 PR 3
  part 6, contextfs commit `604f897`). When pi-cfs-init calls
  `handle.shutdown().await` on graceful VM teardown, contextfsd
  mints a signed sentinel that the broker pins as the close
  watermark for the `(tenant, daemon_instance_id)` pair. Hard kills
  (warm-pool eviction without a graceful shutdown — i.e. every
  destroy-on-non-Clean release in pi-rs's normal flow) skip the
  sentinel; the broker's `high_watermark` for that pair stays
  intact, and the operator force-close HTTP endpoint (§3.5.6)
  fills the gap if needed. Pi-rs's pool-return path *does* try
  graceful shutdown when a VM ages out (rotation cap), so warm-pool
  rotations under normal operation produce a clean audit chain
  with sentinels at every boundary.

### 4. Per-OS launcher impls

#### Linux: `FirecrackerLauncher` (`#[cfg(target_os = "linux")]`)

- Probes `/dev/kvm` and the `firecracker` binary at construction.
- Maintains a **warm pool of N (default 2) pre-booted VMs per `BootSpec` ring**, as `tokio::sync::Mutex<HashMap<BootSpec, VecDeque<WarmVm>>>`. `acquire(&boot_spec)` pops a warm VM from the matching ring in O(1); release returns it to the same ring. Two callers with different `BootSpec`s (e.g. different `host_cwd` per RFD 0006 worktree) get separate rings and cannot share a warm VM. Default 2/ring because real coding-agent tool calls are dominantly sequential (write → read → bash → read); pool=2 covers one parallel subagent burst at ~512MB resident. Telemetry on pool hit-rate per ring decides whether to bump to 4. Empty rings garbage-collect after their last VM is released and an idle TTL elapses (default 5 min) so a long-running session that drifts across many cwds doesn't leak unbounded ring entries.
- Pool refills opportunistically in the background.
- Each Linux/Firecracker VM gets its own firecracker process, API socket, vsock socket, plus a per-VM `cfs-fs-server` instance on a per-VM run-dir UDS (§3.5.5). The `contextfs-broker` is **shared across all VMs in one pi process** (per §3.5.5 corrected topology + RFD-0025 §A); it's spawned at first acquire on `/run/pi-rs/broker.sock` and joined at pi process exit. macOS/vfkit and Windows/cloud-hypervisor VMs use a per-VM virtio-fs share (those launchers expose virtio-fs; Firecracker does not). Rotation: VMs are torn down and replaced after N tool calls or T seconds (default 50 calls / 5 minutes) to bound cumulative state leakage.
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
                               cfs_mesh ✓, contextfs_cli ✓ (rev 06e9531)
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
- **Tool input crafted to escape the sandbox.** Two distinct boundaries to keep separate:
  1. **Structured file tools** (`read`/`write`/`edit`/`grep`/`find`/`ls`) — input paths come from JSON tool input. The HOST runs `resolve_beneath(host_cwd, requested, allow_missing_leaf)` BEFORE the path crosses the wire (per §"Path virtualization"); absolute paths or `..` segments outside `<host_cwd>` are rejected with `ToolError::InvalidInput`. This protection works identically across Linux/macOS/Windows because it's host-side and doesn't depend on the underlying `/work` mount.
  2. **`bash`-followed symlinks already on disk** — when bash inside the guest dereferences a path, the guest filesystem layer follows symlinks. On **Linux/Firecracker** the contextfs `cfs-fs-server --backend-root` does symlink-resolve-beneath at the FUSE layer: a symlink inside `/work` whose target escapes the backend root (e.g. `/work/escape → /etc`) is rejected at dereference time. On **macOS/vfkit** and **Windows/cloud-hypervisor** v1 the virtio-fs RW share has no equivalent beneath-root check — bash following a pre-existing escape symlink crosses out of the share into the host filesystem. **v1 limitation, documented and tested**: on those platforms the agent can theoretically read/write outside `/work` via a pre-existing symlink in the host cwd; the structured tool path is unaffected. v1.1 adds a guest-side resolve-beneath wrapper around bash on macOS/Windows; until then, operators on those platforms should not microvm-sandbox sessions where the host cwd contains adversarially-placed symlinks.
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
- **AuditPusher replication-lag window.** Between successful FUSE
  write and broker push, an in-guest attacker who crashes the VM
  mid-batch loses the unpushed records from the broker's view —
  but **not from the daemon's local audit chain**, which is the
  forensic source of truth. AuditPusher v1 has **no per-write
  fail-closed gate** (RFD-0024 PR 3 §"Refactor scope" item 1
  removed it deliberately — agent-blocking on broker liveness was
  judged a worse failure mode than bounded replication lag).
  Bounded by AuditPusher's coalesce window (default 1 s, 256
  records). Operators who want broker-roundtrip-or-bust posture
  monitor `broker.log` for `audit_pusher_transport_dropped` lag
  signals and can hard-close the chain via the operator HTTP
  `instance-close` endpoint (§3.5.6); pi-rs's `SandboxAction`
  telemetry surfaces these post-hoc.
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

- **Stage 1 (after Commits D + G merge):** `--sandbox-provider=microvm:firecracker` (explicit pin) goes live. The CLI flag and `MicroVmProvider` itself land in Commit G; Commit D ships only the `FirecrackerLauncher` + warm pool that G then wires up. Documented as "Linux only, beta." `pi sandbox doctor` works for the firecracker path. Maintainer dogfoods on Manjaro.
- **Stage 2 (after Commit E merges):** `--sandbox-provider=microvm:vfkit` (explicit pin) goes live. macOS users can dogfood.
- **Stage 3 (after Commit F merges):** `--sandbox-provider=microvm:cloud-hypervisor` goes live on Windows.
- **Stage 4 (Commit G + post-impl follow-ups):** `--sandbox-provider=microvm` (auto-pick) goes live with the cross-OS coverage promise. **Gating bar**: not just "all three launchers exist" — also requires guest-side symlink-resolve-beneath parity on macOS/vfkit and Windows/cloud-hypervisor when bash dereferences a pre-existing in-`/work` symlink (per §6 threat model item 2). The structured-tool path is host-side `resolve_beneath` and is already cross-OS uniform; the gap is bash-on-virtio-fs only. Until that parity ships:
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
- **Process lineage proofs as Cedar context.** Deferred contextfs-side (eBPF unavailable in target sandboxes; cooperative HMAC-token mode would need pi-sandbox-worker / pi-cfs-init as the trust root). Pi-rs revisits when its policies require ancestor-based gating; until then, pi-rs's UID-separation + UDS-perms + seccomp defenses (§6) are sufficient for the v1 threat model.

## Open questions

1. **Cloud-hypervisor as unified launcher across all three OSes?** It supports KVM/Hypervisor.framework/WHPX in one binary. v1.0 picks best-of-breed (Firecracker on Linux, vfkit on macOS, cloud-hypervisor on Windows); revisit after one quarter of telemetry.
2. ~~**Pool size default.**~~ **DECIDED, v0.3.** N=2 per `BootSpec` ring on Linux/Firecracker (memory: 2 × 256MB = 512MB resident; covers one parallel subagent burst). macOS/Windows v1 are destroy-on-release (no pool; v0.24). Operators can override with `--sandbox-microvm-pool-size=N`. The earlier draft's "N=4 host-side" line is superseded.
3. ~~**Tool selection for `pi-tools-core` (the guest-buildable subset).**~~ **DECIDED.** v0.8/v0.9 set the original split (`read`/`write`/`edit`/`bash`/`grep`/`find`/`ls` guest, `web_search` host-bound, `monitor` unavailable). v0.33+ then re-classified `web_search` as **guest-dispatched and host-proxied via the 5003 bridge channel** — there is no host-direct disposition in v1. Current matrix: guest tools = `read`/`write`/`edit`/`bash`/`grep`/`find`/`ls`/`web_search`; unavailable = `monitor`/`lsp`. See §"Tool availability under `microvm`" + §"web_search via vsock proxy".
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
  - Read + edit + verify host fs mutation through the `/work` mount (contextfs FUSE on Linux/Firecracker; virtio-fs on macOS/Windows).
  - bash with cwd boundary check (path traversal rejected).
  - Pool warm-vs-cold timing.
  - Resource limit honored (OOM killed at mem cap).
- `microvm_vfkit` (macOS, gated): same coverage where supported.
- `microvm_chv` (Windows, gated): same coverage where supported.
- Negative: rootfs sha mismatch refuses to boot; guest-side tool error surfaces a clean `is_error: true` ToolResult.

### Dogfood (per phase)

- After Phase 3 + G: `pi --sandbox-provider=microvm:firecracker "ls + read + edit a marker file"` on Manjaro. Confirm session JSONL has the expected `provider="microvm"`, `launcher="firecracker"`, `acquire_to_ready_ms`, and `cold_boot` telemetry fields, and the host file actually changed. Also exercise `web_search` and confirm the row has the same `provider`/`launcher`/`guest_duration_ms` shape as a `bash` row (no `dispatch_path` field — every microvm tool dispatches through the guest).
- Same dogfood on macos-14 + windows runner once D and F have landed.
- `pi --stats sandbox-actions` must show non-zero rows with `provider` populated, and where applicable `launcher` / `acquire_to_ready_ms` / `guest_duration_ms` / `cold_boot`.

## Revision history

- **v0.42 (2026-05-05):** rfd-critic v0.41 returned **READY**
  ("Critical issues: None for v0.41. Verdict: READY") — third
  clean READY in the iteration history (v0.31, v0.38, v0.41).
  Polish landing the suggested non-blocking deltas:
  (1) Cross-repo cite prefix added: `crates/contextfs-...` →
  `<contextfs>/crates/contextfs-...` for all 5 contextfs source
  references in §3.5.5/§3.5.6/v0.41 history (paths resolve in
  the sibling contextfs repo, not in pi-rs).
  (2) `--sandbox-microvm-pool=force` escape hatch cut from v1
  per critic's option (a). The flag was a knowingly-unsound
  pooling escape on macOS/Windows; v1 ships destroy-on-release
  only and a future RFD lifts it once each launcher has a
  proven-clean per-call container/cgroup analog.
  Critic's other suggestions (named integration test for
  task/allowlist path; commit-pinned permalinks before publish)
  deferred to publish-readiness pass.
- **v0.41 (2026-05-05):** rfd-critic v0.40 pass found 2 critical
  + 4 small. Both criticals real and closed. (1) **Broker CLI
  syntax was wrong**: §3.5.5/§3.5.6 had
  `--tenant-peer-uid <vm>=<uid>` (equals separator) and
  `<vm_a>=<bridge_uid>` per-VM. Verified against
  `<contextfs>/crates/contextfs-broker/src/main.rs:188-205`: the actual format
  is `--tenant-peer-uid <tenant_id>:<uid>` (colon) and
  `--tenant-mode <tenant_id>=embedder` (equals — different
  separator). Identity model also wrong: `tenant_id` is **stable
  per pi process**, not per VM; per-VM differentiation is via
  `vm_id`. Pi-rs runs ONE tenant per pi process, N VMs share that
  tenant. SIGHUP-reload claim deleted: `contextfs-broker` has no
  SIGHUP handler (`contextfsd` does, for policy reload — verified
  against `<contextfs>/crates/contextfsd/src/lib.rs:8-33`). No runtime broker
  reconfiguration; broker is started once with stable tenant
  config. (2) **Stale audit_ping/fail-closed sweep**: §3
  "Filesystem semantics" still said "every write mediated by
  contextfsd's PDP and audit-ping"; corrected to "PDP + local
  audit chain; broker replication is async best-effort via
  AuditPusher". §6 threat-model "Audit-ping loss window /
  fail-closed shrinks the window to zero" rewritten as
  "AuditPusher replication-lag window: no per-write fail-closed
  knob in PR 3; operators respond via broker.log lag signals +
  HTTP instance-close". Plus stale `v0.3.0`/`v0.3.2` mentions
  cleaned: §3.5.7 schema-drift paragraph + §3.5.9 rootfs manifest
  + §3.5.9 doctor sample now reference the git-rev pin per
  §3.5.8 ("rev 06e9531") instead of floating `v0.3.x` numbers.
- **v0.40 (2026-05-05):** contextfs maintainer dropped the §3.5.4
  draft (`rfd/0023-CONTEXTFS-REVIEW-v0.39-section-3.5.4-draft.md`),
  then a v2 trimmed-against-d6cdafa revision dropping duplication
  with §3.5.6 (operator force-close), §3.5.9 (failure-surface
  table), §3.5.10 (caller_exe / sentinel free-surfaces). v0.40
  splices the v2 verbatim. v0.40 closes all 7 review fixups
  (item 1 = this splice; items 2–7 + 8 landed in v0.40-prep).
  Re-running rfd-critic next.
- **v0.40-prep (2026-05-05):** contextfs maintainer reviewed v0.39
  against contextfs `main` @ `06e9531` (review at
  `rfd/0023-CONTEXTFS-REVIEW-v0.39.md`). Body of RFD is sound; §3.5
  needs 7 concrete edits before Commit G. Items 2–7 closed in this
  prep commit; **§3.5.4 left as a `PENDING contextfs draft`
  placeholder** because the maintainer offered to write the
  audit_ping → AuditPusher rewrite directly (it's contextfs-shaped
  content, RFD-0024 §"Refactor scope"). v0.40 will land as one
  atomic commit splicing in their §3.5.4 text.
  Closed in this prep:
  (2) §3.5.6 broker invocation gains `--tenant-secret-path
  <run_dir>/<vm_id>/cfs-tenant-secret` (RFD-0024 PR 3 part 5
  AuditResync requirement; without it broker returns
  `verify_write_unavailable`).
  (3) §3.5.5 topology corrected: **one shared broker per pi
  process** (was per-VM in earlier drafts; per RFD-0025 §A and
  contextfs maintainer review). Broker uses `--tenant-peer-uid`
  multi-tenant fan-in; one broker process serves N VMs with one
  tenant id per VM.
  (4) §3.5.9 failure-surface table: `BrokerTenantModeMismatch` row
  removed (variant doesn't exist in shipping contextfs `StartError`;
  the legacy-no-vm_id case is `BrokerVmIdRequired { tenant_id }`);
  added rows for `BrokerProtocolTooOld { broker_socket, error }`
  and `AuditInstanceClosedOnBroker { daemon_instance_id, closed_at_seq }`
  (the two new variants RFD-0024 PR 3 ships). `AcquireError` enum
  in §2 updated lockstep.
  (5) §3.5.7 TOML drops the `audit_ping = { ... }` line (would be
  rejected at daemon startup by `#[serde(deny_unknown_fields)]`);
  adds explicit `daemon_instance_id_path` (RFD-0024 PR 3 part 5
  load-bearing across restarts). Added rotation note: rotating
  the file forces a fresh AuditResync.
  (6) §3.5.8 version-pin section rewritten per RFD-0025 §C.2:
  pin by git rev now, flip to `version = "X.Y.Z"` when contextfs
  cuts its first tagged release. Pre-RFD-0024 brokers surface
  as `BrokerProtocolTooOld` (was a stale "v0.2.x reject" claim).
  (7) Three free-surface mentions added: §3.5.6 covers the
  operator HTTP `POST /tenants/<t>/daemons/<id>/instance-close`
  endpoint for incident response; new §3.5.10 covers
  `caller_exe`/`caller_start_time` in `broker.log` (kernel-attested,
  not `comm`-spoofable) and the `instance_closed` sentinel that
  graceful shutdown mints.
  (8) Process lineage proofs added to §"Out of scope / deferred"
  per the contextfs review pointer: deferred contextfs-side;
  pi-rs revisits when its policies require ancestor-based gating.
  Plus a bug fix: §4 "Per-OS launcher impls" had a stale
  "per-VM contextfs-broker" claim; corrected to "shared broker
  per pi process" lockstep with §3.5.5.
- **v0.39 (2026-05-05):** rfd-critic v0.38 returned **READY**
  ("Critical issues: None. The document is now implementable as
  written.") — second clean READY in the iteration history,
  after the v0.33 architectural pivot to vsock-proxied
  web_search forced a redesign that took 6 more iterations
  (v0.33 → v0.38). Polish landing the suggested non-blocking
  text deltas: (1) Public API impact paragraph signature
  updated `Result<SandboxOutcome, SandboxError>` →
  `Result<SandboxOutcome, SandboxFailure>` and clarified that
  `SandboxOutcome` replaces 0022's *execution-only* return
  (raw stdout/stderr/exit_status), not its bare `ToolResult`.
  (2) `MicroVmProvider::execute_tool` commentary now references
  `dispatch_class_registry: DispatchClassRegistry` (not
  `Arc<dyn>` — the trait was simplified to a frozen map in
  v0.37). External-citation tightening (Firecracker #1180,
  vfkit host-listen quirk, aegis line refs) deferred to a
  publish-readiness pass.
- **v0.38 (2026-05-05):** rfd-critic v0.37 pass: 2 critical
  (telemetry-on-error + Linux v1 framing) + small. Both real,
  both closed.
  (1) **Telemetry on error paths.** `execute_tool() -> Result<SandboxOutcome, SandboxError>` silently dropped `launcher`/`pool_disposition`/`reset_status`/`release_reason` on the Err arm — operators saw "an acquire failed" but not which launcher, whether a VM had been booted then destroyed, etc. Fixed: introduced `SandboxFailure { error: SandboxError, telemetry: SandboxTelemetry }`; `execute_tool()` now returns `Result<SandboxOutcome, SandboxFailure>`. The runtime emits one `SandboxAction` row per tool call regardless of outcome, populated from whichever arm fired. Pseudocode updated for the three error sites: ToolUnavailable (no VM, telemetry has only `provider`), Acquire failure (launcher attempted, no VM created), Execute failure (VM existed, release_outcome already known). `From<SandboxError> for SandboxFailure` provides the "no VM" convenience.
  (2) **Linux v1 framing.** Summary previously claimed "first real local microVM backend"; in reality Linux/Firecracker v1 is hard-coupled to contextfs broker + `cfs-fs-server` + `cfs-mesh` + per-VM OIDC, which ordinary `pi --sandbox-provider=microvm` users won't have. Narrowed Summary: Linux v1 is **operator-managed mode** (requires contextfs broker), macOS/Windows v1 are self-contained. CLI UX is honest: `pi sandbox doctor` flags this on a Linux host with no contextfs config and points at alternatives. Self-contained Linux mode deferred to follow-on RFD (when Firecracker gains virtio-fs OR pi-rs gains a non-contextfs guest-side FUSE proxy).
  (3) `DispatchClassRegistry::lookup` fallback comment clarified: unknown tools default to `SandboxManaged` (the registry's job is RuntimeNative-vs-SandboxManaged classification only); availability gating happens via the separate `tool_disposition()` query, which defaults to `Unavailable` for unknown tools per the §"Plan-time API" safety rule.
  (4) Stale `search_config` field doc cleaned up (was prefixed with the v0.32 `host_tools` paragraph).
- **v0.37 (2026-05-05):** rfd-critic v0.36 cleanup pass: small
  but real findings around the v0.36 redesign. (1) Bridge UDS
  was at `/run/pi-search-bridge.sock` but the reset survival
  list moved `/run/contextfs` only — bridge would silently
  break on first reset. Fixed: dedicated runtime tmpfs at
  `/run/pi-bridge` with the UDS at `/run/pi-bridge/search.sock`;
  added `/run/pi-bridge` to the reset survival list. Bridge's
  cwd set to `/run/pi-bridge` at boot; vsock and UDS fds are
  kernel socket objects, not path-bound, so survive
  `pivot_root` without re-binding. (2) `MicroVmProvider::execute_tool`
  pseudocode unified: replaced bespoke `monitor` branch with
  a `match self.tool_disposition(tool_name)` against
  `Guest`/`Unavailable`. `Unavailable` returns
  `SandboxError::ToolUnavailable` with a generic
  "unavailable under provider 'microvm'" message; works for
  `monitor`, `lsp`, and any future unavailable tool without
  per-tool branches. (3) `DispatchClassRegistry` simplified
  from a trait to a frozen `BTreeMap<String, ToolDispatchClass>`
  — no v1 polymorphism need; v1 has one provider type. Built
  by walking `ToolRegistry` at construction; parity test in
  `crates/pi-sandbox/tests/dispatch_class_parity.rs` asserts
  registry agrees with `Tool::dispatch_class()` for every
  default tool.
- **v0.36 (2026-05-05):** rfd-critic v0.35 pass: 2 critical
  (init-owner contradiction; seccomp mechanics overstated) +
  underspec. All real, all closed.
  (1) **Init / PID 1 ownership pinned.** v0.35 had three actors
  implicitly claiming PID 1 (`/init`, `pi-cfs-init`, the bridge
  daemon). Resolved canonically: `/init` execs `pi-cfs-init`;
  `pi-cfs-init` is the **sole PID 1** and supervises every
  other process — `pi-vm-search-bridge` (long-lived child),
  `pi-sandbox-worker` (re-execed by reset), `pi-vm-reset`
  (on-demand reset agent). Every previous "this thing IS PID 1"
  claim about the bridge or worker is gone. §3.2 `/init` block
  + §3.5.9 + §"web_search via vsock proxy" all reference one
  canonical process tree.
  (2) **Seccomp narrowed to syscall-arg filtering.** v0.35 said
  the filter blocked `bind`/`connect`/`listen` on
  "AF_VSOCK family sockets" — seccomp-bpf can't dereference a
  userspace `sockaddr*` to know an fd's family on those calls.
  Narrowed Layer 2 to **just `socket(AF_VSOCK, ...)` and
  `socketpair(AF_VSOCK, ...)`** (family is in a register;
  matchable). With those denied the child has no path to obtain
  a vsock fd, so `connect`/`bind`/`listen` filtering is
  unnecessary. Plus removed the wrong "Linux-host-only" framing:
  the defense is **guest-Linux** (the bash subprocess always
  runs in the Linux guest rootfs regardless of host launcher),
  so it applies uniformly across Firecracker/vfkit/cloud-hypervisor
  given `CONFIG_SECCOMP_FILTER` in the guest kernel — verified
  by the `pi sandbox doctor` probe.
  (3) **Bridge-death detection** specified: PID 1 (`pi-cfs-init`)
  catches SIGCHLD, writes `PI_FAIL: search-bridge-died exit=<N>`
  to the serial console, exits non-zero; host launcher destroys
  the VM. No auto-restart in v1 (dead bridge means in-flight
  searches unrecoverable; destroy-and-replace).
  (4) **Queue/backpressure** specified: bounded depth of 2
  (one in-flight + one waiting); a third concurrent caller gets
  `WebSearchResponse.error = "search-busy: ..."` immediately;
  queued worker timing out drops the UDS, bridge dequeues
  without forwarding.
  (5) Stale `pi-host-search-proxy` execute_tool comment cleaned
  up. OQ#3 sweep done — `web_search` is now described as
  "guest-dispatched and host-proxied via the 5003 bridge
  channel; no host-direct disposition in v1".
  (6) `MicroVmProvider` gains the missing
  `dispatch_class_registry: Arc<dyn DispatchClassRegistry>`
  field referenced by the v0.32 polish-pass assert.
- **v0.35 (2026-05-05):** rfd-critic v0.34 pass: 2 critical
  (seccomp mechanically wrong; channel-vs-reset lifecycle
  contradiction) + small. Both real, both closed.
  (1) **Seccomp wording was wrong.** v0.34 said the worker
  applied a filter and "exempted itself" — seccomp filters
  cannot be retroactively removed. Fixed: filter is installed
  in the **child** between `fork()` and `execve()` via a pre-exec
  hook (PR_SET_NO_NEW_PRIVS + seccomp(SECCOMP_SET_MODE_FILTER) +
  setresuid drop). Worker process never has the filter. Plus
  Layer 1 added: UID separation (worker = `pi-worker`, bash =
  `pi-tool`) + UDS perms (`/run/pi-bridge/search.sock` mode
  `0o600` owned by pi-worker:pi-worker) — primary defense, with
  seccomp as Layer 2 closing the residual `AF_VSOCK` direct
  connect. Plus FD_CLOEXEC normative requirement for all
  sandbox-internal fds; negative test `bash 'ls -l /proc/self/fd'`
  asserts no leaks.
  (2) **Channel-vs-reset lifecycle contradiction.** v0.34 said
  the 5003 channel "survives release(Clean)" but the reset
  choreography re-execs the worker; if the worker owned the
  vsock fd, that promise was false. Fixed: introduced
  **`pi-vm-search-bridge`** (~80 LoC, owned by Commit B's
  rootfs builder) — a tiny static helper that runs as PID 1 /
  reset-stable in the rootfs and owns the guest-side endpoint
  of vsock 5003 + a UDS at `/run/pi-bridge/search.sock`. The
  worker is now stateless w.r.t. the channel; it connects to
  the UDS per `web_search` call. Bridge survives `pivot_root`,
  so the host's vsock connection is uninterrupted across worker
  re-exec / VM reset / pool reuse. Required integration test:
  `web_search → reset → web_search` on same `BootSpec` ring
  asserts `cold_boot=false` AND second search succeeds.
  (3) macOS/Windows status: Layer 1 (UID + UDS perms) carries
  over; Layer 2 (seccomp) is Linux-only; documented gap; doctor
  output flags it; v1.1 lifts.
  (4) Cancellation semantics added: tool wall_timeout while
  HTTP outstanding → worker drops UDS → bridge half-close →
  host task drops `reqwest` future → no leaked HTTP.
  (5) §3.5.7 schema-drift wording corrected: contextfs config
  drift is **runtime fail-loud** at config load (via
  `deny_unknown_fields` + pinned-version constraint + CI compat
  test), NOT compile-time (sibling repo, independent semver).
  (6) Stale `pi-host-search-proxy` references in
  `tool_disposition()` prose, tool matrix, and "no host-direct
  dispatch" paragraph all swept.
- **v0.34 (2026-05-05):** rfd-critic v0.33 pass: 2 critical
  (security gap + platform-compat) + small. Both real, both
  closed by inverting the search-channel topology.
  (1) **Security:** `vsock` is process-family — a `bash` tool
  subprocess could `socket(AF_VSOCK, ...)` and connect to host
  port 5003 directly, bypassing auto-approve. Closed: `bash`
  tool spawn now applies a **seccomp-bpf filter** denying
  `AF_VSOCK` (`socket`/`bind`/`connect`/`listen` return
  `EAFNOSUPPORT`); the worker is exempt because it's the parent
  that applied the filter. Negative test:
  `bash 'python3 -c "import socket; socket.socket(40, ...)"'`
  asserts `EAFNOSUPPORT`. macOS/Windows analogues will need an
  equivalent on the bash spawn path before web_search-via-bash
  escape becomes a real concern there.
  (2) **Platform:** v0.33 had the guest connecting outbound to a
  host-listening port, but the main protocol explicitly avoids
  host-listen on macOS vfkit. Closed: search channel is now
  **host-initiated** — at acquire time, after the main 5001
  connection succeeds, the launcher opens a *second* host→guest
  vsock connection on guest port 5003. The connection is
  long-lived and full-duplex for the VM's lifetime; multiple
  search calls multiplex through it (one in-flight at a time
  per VM in v1). Both connections are host→guest now.
  (3) **No separate binary.** v0.33's `pi-host-search-proxy` is
  gone. The host-side handler is an async task in `pi-sandbox`
  that the launcher spawns alongside each VM and joins on
  release. Lives inside the existing pi binary process; same
  auth context. The `SearchProxyController` trait is replaced
  by a simpler provider-level `SearchConfig { auth, rate_limit }`
  shared across VMs, with the per-VM channel-handler task owned
  by the `VmHandle`.
  (4) **Error mapping** added: channel-establish failure →
  `AcquireError::HostTransportSetup`; channel drop mid-call →
  `ToolError::Other` + `SuspectGuestState`; provider auth/rate
  limit failures → `WebSearchResponse.error` → `is_error=true`
  ToolResult, VM stays `Clean`; protocol-version mismatch →
  one-shot error then channel-dropped semantics.
  (5) **Lifecycle pinned**: search channel established by
  `acquire()` BEFORE return — failure destroys the half-acquired
  VM, no half-states in the pool. Channel survives `release(Clean)`;
  joined+dropped on destroy. (6) Background A2 paragraph: guest
  worker deps now include `pi-search-proto` (~50 LoC of pure
  wire types; does NOT pull `pi-tools-net` or `reqwest` into
  the guest).
- **v0.33 (2026-05-05):** Architectural change at maintainer
  request: **remove host-direct dispatch entirely**. v0.32 routed
  `web_search` through a host-direct path so the guest stayed
  network-free; the maintainer flagged that as breaking the
  agent's mental model ("tools run in the sandbox" became "*most*
  tools run in the sandbox, except this one"). v0.33 keeps
  `web_search` registered in the guest worker like every other
  tool; its handler proxies the HTTP call out via vsock to a new
  host-side `pi-host-search-proxy` (~150 LoC, owned by Commit G)
  on `VSOCK_SEARCH_PROXY_PORT = 5003`, one process per VM with
  per-VM rate limits. Wire shape: new `pi-search-proto` crate
  (`WebSearchRequest { query, max_results, locale } →
  WebSearchResponse { results, error }`). Surface changes:
  removed `SandboxToolDisposition::HostDirect` variant; removed
  `HostBoundToolDispatcher` trait + `HostExecOutcome` shape;
  `SandboxTelemetry.dispatch_path` and `SandboxAction.dispatch_path`
  fields dropped (every microvm tool dispatches through the
  guest now — no meaningful split). `MicroVmProvider.host_tools`
  field replaced by `search_proxy: Arc<dyn SearchProxyController>`
  (lifecycle controller for the per-VM proxy child process).
  SQLite migration column count: × 9 new (was × 10 in v0.32 —
  `dispatch_path TEXT` removed). Net wins: one dispatch path,
  one telemetry shape, narrow well-typed host RPC instead of
  open-ended host-tool registry, identical auto-approve gate
  semantics for every tool. Tradeoff: one extra vsock round-trip
  per `web_search` (negligible vs the actual HTTP) and one new
  protocol crate to version. Will re-run rfd-critic now that
  v0.32 was READY-baseline.
- **v0.32 (2026-05-04):** rfd-critic v0.31 returned **READY**
  ("Critical issues: None.") after 28 iterations from v0.4. This
  polish pass lands the three non-blocking suggested deltas:
  (1) `MicroVmProvider::execute_tool` defense-in-depth assert now
  references an explicit `dispatch_class_registry: Arc<dyn
  DispatchClassRegistry>` field on the provider (threaded from
  `RuntimeConfig` so planner + provider share one source of
  truth), instead of a free-floating `tool_dispatch_class()`
  function. (2) `CallLimits.max_output_bytes` doc pinned: it's
  an aggregate cap across captured `stdout + stderr` (UTF-8
  boundary-safe truncation at the worker), `model_output` is
  derived from that already-capped capture (not budgeted
  independently), `display` is excluded. Wire shape is already-
  truncated. (3) Public API impact paragraph split into two
  audiences: `SandboxProvider` implementers see
  `execute_tool(.., tool_use_id, ..)` + return-type changes;
  `MicroVmLauncher` implementers see `release() -> ReleaseOutcome`
  (a launcher-internal trait — `SandboxProvider` impls don't see
  this). Both ride a pi-sdk MINOR bump under sealed-by-convention
  pre-1.0.
- **v0.31 (2026-05-04):** rfd-critic v0.30 pass found 2 real
  citation/staleness issues. (1) Background §"pi-tools dependency
  problem" used present tense ("Audit shows `pi-ai.workspace = true`
  ... unconditional") but on `main` today those entries have moved
  to `[dev-dependencies]` — past-tense the audit and add a current-
  state pointer. (2) §3.5 categorically claimed "Firecracker does
  not support virtio-fs" while `crates/pi-sandbox/src/microvm/firecracker.rs`
  on `main` STILL contains `virtiofsd_bin` resolution, a virtiofsd
  child per VM, and virtio-fs `/work` mount plumbing — that's the
  pre-known-issues Commit D shape. Reconciled: kept the v1 GA
  position (managed-only on Linux/Firecracker, per
  `rfd/0023-known-issues.md` §"Issue 1" + upstream firecracker-microvm
  issue #1180); added an explicit note that Commit G removes or
  quarantines the legacy virtiofsd path (delete entirely, or
  cfg-gate behind `firecracker-virtiofs-experimental` feature
  with a stderr banner). The in-tree code is dead-on-Firecracker-
  v1.15.0 today; v1 GA story is unchanged.
- **v0.30 (2026-05-04):** rfd-critic v0.29 cleanup pass found
  cascading staleness from v0.29's `release()` API change. (1)
  §"Post-call hygiene" still wrote
  `SandboxAction.outcome = "reset-failed"` and tests asserted
  `outcome=SuspectGuestState`; updated to assert
  `pool_disposition` / `reset_status` / `release_reason` instead.
  (2) Host-direct branch's `SandboxTelemetry` literal omitted
  the new fields; now explicitly sets `pool_disposition: None`,
  `reset_status: None`, `release_reason: None`. (3)
  `ReleaseGuard::release()` was still `-> ()`; now returns
  `ReleaseOutcome` (with a sane fallback for the
  guard-already-disarmed case). (4) `reset_status` representation
  fixed: `Some(NotApplicable)` for guest-VM destroy paths;
  `None` only for host-direct/local-process where no VM
  existed. Documented the asymmetry on both `ReleaseOutcome` and
  `SandboxTelemetry`. (5) Open Question #2 (pool size) closed —
  N=2 per BootSpec ring on Linux/Firecracker; macOS/Windows v1
  destroy-only. The earlier "N=4 host-side" was superseded in
  v0.3 but the OQ still listed it. (6) Public-API impact
  paragraph corrected: changing `release()` return type IS
  source-breaking; `SandboxProvider` is documented as
  unstable/sealed-by-convention pre-pi-sdk-1.0; this rides a
  pi-sdk MINOR with changelog mention.
- **v0.29 (2026-05-04):** rfd-critic v0.28 pass: 1 critical
  (release/reset telemetry not implementable as written) +
  3 underspec. All real, all closed. Critical: §"Post-call
  hygiene" promised `SandboxAction.outcome = "reset-failed"` and
  tests asserting `outcome=SuspectGuestState`, but `release()`
  returned `()` and `SandboxAction` had no field for it. Fix:
  `release()` now returns `ReleaseOutcome { pool_disposition,
  reset_status, reason }`; `SandboxTelemetry` and `SandboxAction`
  gain three nullable fields (`pool_disposition: Option<PoolDisposition>`,
  `reset_status: Option<ResetStatus>`, `release_reason: Option<String>`);
  migration text + column count updated (× 10 new now).
  `MicroVmProvider::execute_tool()` pseudocode threads
  `release_outcome` into the telemetry row. Public API impact
  paragraph added: pi-sdk's `MockSandboxProvider` returns a
  synthetic `ReleaseOutcome` so existing test code keeps
  compiling. Underspec: (a) `tool_filtered_out` event now has
  an explicit `SessionEntryKind::ToolFilteredOut` definition
  with field shapes + sample JSONL line; v1 is JSONL-only, no
  SQLite column. (b) Pool-partitioning caveat added: today's
  `task` tool warns `isolated=true` is a no-op, so per-subagent
  `host_cwd` rings only kick in once RFD 0006 wiring is active
  in the task executor. (c) `ExecuteOutcomeHint::Clean` doc
  clarified — it's a *necessary* condition for pool return,
  not sufficient; final disposition lives on `ReleaseOutcome`.
  Trivia: `ToolDispatchClass::HostDirect` typo →
  `SandboxToolDisposition::HostDirect` in the host-tools
  source-of-truth comment.
- **v0.28 (2026-05-04):** rfd-critic v0.27 pass: 2 critical
  (FS reset mechanism not technically believable; timeout policy
  contradicted itself) + 4 small. Both criticals real and closed.
  (1) **Reset choreography.** "Hot-swap upperdir/workdir of a
  mounted overlay" doesn't work — overlayfs metadata cache makes
  in-place upper swap unsafe. Replaced with explicit
  `pivot_root` choreography: agent creates `/run/pi-newroot`
  with a fresh overlay (same RO lower, new tmpfs upper/work),
  `move_mount`s `/proc`/`/sys`/`/dev/vsock`/`/work`/`/run/contextfs`
  into the new root, `pivot_root`s, re-execs the worker, and
  `umount -l`s the old root so its tmpfs is reaped. The host's
  `release()` blocks until the new worker reconnects on port 5001.
  Reset-failure observability added: `PI_FAIL: reset-failed step=N
  errno=E` to the serial console mirrors the boot-failure path;
  `SandboxAction.outcome = "reset-failed"`. Dropped the "~50 LoC"
  agent estimate (review nit). (2) **Timeout policy.** v0.27's
  prose said timed-out-but-cleaned VMs return to the pool, but
  the pseudocode treated `Err(ExecuteError::CallLimit)` as
  `SuspectGuestState`. Fix: `CallLimit` semantics narrowed to
  worker-level timeout (worker missed `wall_timeout + 1s` — VM
  unresponsive, always destroy). Tool-level timeouts are now
  `Ok(VmExecution { tool_result.is_error=true, exit_status=124,
  post_call_state })` — the worker drains in-guest and the VM is
  pool-eligible iff `post_call_state = Clean`. Doc + pseudocode
  + ExecuteError variant doc all aligned. Small: `code-reviewer`
  example generalised — bundled agents in tree don't all carry
  tool allowlists today, so paragraph reads "subagents may carry
  allowlists" with the bundle path as a real example location.
  Summary vs Stage 4 rollout text are consistent (explicit pins
  ship per-OS; unqualified auto-pick waits for all three GA);
  no change needed.
- **v0.27 (2026-05-04):** rfd-critic v0.26 pass: 1 critical
  (filesystem-reset implementation details) + 4 small. All real,
  all closed. (1) §"Post-call hygiene" item 3 was vague on
  ownership and on which paths get exercised. v0.27 makes the
  reset **launcher-owned** (not worker — a worker can't unmount
  its own running root): launcher attaches rootfs as RO block;
  guest `/init` mounts overlay (`lowerdir=ro-rootfs, upperdir/
  workdir=tmpfs`); after each call the launcher issues a host-side
  reset RPC over a separate vsock control port (5002) to a tiny
  in-guest agent (`/sbin/pi-vm-reset`, ~50 LoC, statically
  compiled) that swaps the upperdir/workdir tmpfs instances.
  Fallback: reset RPC failure / agent missing → destroy VM
  (never silent best-effort). Negative tests strengthened to
  three cases including a system-path copy-up
  (`install -m755 ... /usr/local/bin/poison && exec it`) and an
  `/etc/passwd` mutation, with explicit harness assertion that
  the next acquire **hit the warm pool ring** (not cold-booted).
  (2) §3.5.9 readiness sentinel was writing `/work/.cfs-ready` on
  every boot, mutating the host's session cwd. Now uses
  `statfs(/work) == FUSE_SUPER_MAGIC` + control-plane health RPC
  to contextfsd; sentinel moved to `/run/pi-cfs/.ready` on the
  guest's private tmpfs. Added explicit normative rule:
  "boot/readiness MUST NOT write `/work`". (3) Stage 1 ordering
  fixed: was "after Commit D"; the CLI flag and `MicroVmProvider`
  itself live in Commit G, so Stage 1 is "after Commits D + G".
  (4) §6 threat model symlink-escape language untangled into two
  separate boundaries: structured file tools protected by
  host-side `resolve_beneath` (cross-OS uniform), bash-followed
  pre-existing in-`/work` symlinks protected only on
  Linux/Firecracker via contextfs `cfs-fs-server --backend-root`;
  macOS/vfkit + Windows/cloud-hypervisor virtio-fs has no
  equivalent in v1 (documented gap). Stage 4 GA-bar wording
  updated to scope the gap precisely to bash-on-virtio-fs.
- **v0.26 (2026-05-04):** rfd-critic v0.25 pass: 1 critical
  (filesystem reset between pool reuses) + small. Critical was
  real and substantive. v0.25's post-call hygiene only covered
  process subtree + temp dirs; nothing prevented
  `bash 'echo poison > /root/marker'` or
  `bash 'cp evil /usr/local/bin/'` from contaminating the next
  reuse on the same `BootSpec`. Closed: §"Post-call hygiene" now
  pins **read-only rootfs + tmpfs upper in overlayfs**; the
  worker's hygiene step unmounts and remounts the tmpfs upper,
  restoring the rootfs to baseline bit-for-bit. The ONLY surviving
  guest mutation per call is `/work` (host-mediated workspace,
  intentional). Required test added:
  `bash 'echo poison > /root/marker; ls /usr/local/bin/'` then
  next-acquire on same BootSpec must not see `/root/marker`.
  macOS/Windows v1 inherit transitively from destroy-on-release.
  Small items: (a) `self.launcher_name()` →
  `self.launcher.launcher_name()` in pseudocode; (b) terminology
  table added in §"Terminology" — provider name × launcher name ×
  transport mode × dispatch path; (c) `microvm:local` /
  `microvm:managed` rationale paragraph corrected to use
  `--sandbox-provider=microvm:firecracker` +
  `--sandbox-microvm-mode=managed` (`local`/`managed` reserved for
  `TransportMode`); (d) `AcquireError::Broker*` variants annotated:
  fire only on Linux/Firecracker `managed` mode (NOT remote-backend
  concerns), kept on `AcquireError` for caller pattern-match
  ergonomics.
- **v0.25 (2026-05-04):** rfd-critic v0.24 pass: 1 critical
  (telemetry-contract self-contradiction) + 4 small. All closed.
  (1) `SandboxAction` struct field list was missing
  `guest_duration_ms: Option<u32>` even though the migration text
  listed it as a new SQLite column. Added between
  `acquire_to_ready_ms` and `cold_boot`. (2) Host-direct branch in
  `MicroVmProvider::execute_tool()` set
  `guest_duration_ms: Some(elapsed())` — wrong, no guest involved.
  Now `None`; `duration_ms` is the host-direct elapsed time.
  (3) Dogfood line `--stats sandbox-actions ... transport column`
  → `provider`/`launcher`/`dispatch_path`/`acquire_to_ready_ms`/
  `guest_duration_ms`/`cold_boot`. (4) Firecracker integration
  test bullet "verify host fs mutation through virtio-fs RW" →
  "through the `/work` mount (contextfs FUSE on Linux/Firecracker;
  virtio-fs on macOS/Windows)". (5) `MicroVmLauncher::transport_name()`
  → `launcher_name()` with a doc note that the return value is
  the same string written to `SandboxAction.launcher`. External
  vendor citation pinning (Firecracker virtio-fs, vfkit version
  floor, cloud-hypervisor + WHPX) deferred to a publish-readiness
  task per RFD policy — left as-is in Discussion.
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
