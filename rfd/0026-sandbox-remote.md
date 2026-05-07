# RFD 0026 — Remote Sandbox Transports (E2B v1 reference, Sprites/Daytona deferred)

- **Status:** Discussion (v0.4)
- **Author:** pi-rs maintainers
- **Created:** 2026-05-02
- **Implemented:** (pending)

## Summary

Sister RFD to **RFD 0023** (local microVM sandbox). Where RFD 0023 ships local
virtualization that converges three OS-specific launchers on one Linux guest
rootfs, this RFD covers **remote microVM-as-a-service vendors**. The dependency
on RFD 0023 is now cleared: `MicroVmLauncher`, `pi-sandbox-protocol`, and
`pi-sandbox-worker` are all present on `main` as of v0.42. (The specific landing
commits for the contextfs RW + Cedar broker + tests_only profile work are
`d8dbdfd` / `e5d35a8` / `b98e06c`; the pi-sandbox-worker vsock dispatch lives at
`crates/pi-sandbox-worker/src/dispatch.rs`.)

**v0.4 scope.** E2B is the v1 reference vendor; Sprites and Daytona move to
"future vendors" until their APIs are hands-on validated. A remote sandbox is
a `SandboxProvider` implementation (the same trait at
`crates/pi-sandbox/src/provider.rs`) that talks to the vendor over TLS/HTTPS
instead of vsock. This revision addresses the four critical gaps from v0.3:
tool-availability matrix (including `web_search` + `task`), subagent session
concurrency, telemetry/lifecycle plumbing, and SQLite schema migration.

## Why split this from RFD 0023

- **No shared infrastructure.** Remote vendors are HTTP APIs to managed
  microVMs; nothing about the host-side launcher trait, the vsock IPC, the
  rootfs builder, or the per-OS `#[cfg]` machinery applies.
- **Different failure modes.** Remote = network errors, rate limits, billing,
  vendor downtime, region selection. Local = `/dev/kvm` access, vfkit binary
  missing, WHPX disabled.
- **Different telemetry.** Remote calls cost real money per second of compute.
  Local calls are free at runtime. The `cost_usd` telemetry field is only
  meaningful here.
- **Independent landing cadence.** The E2B integration is a self-contained
  ~700 LoC commit that does not gate the local microVM story.

## Vendor selection rationale

| Vendor   | Cold boot | Pricing | API style | Auth | v1 status |
|----------|-----------|---------|-----------|------|-----------|
| **E2B**  | ~1.5–3 s  | ~$0.000084/s compute + ~$0.000225/s storage (estimate from public docs at https://e2b.dev/docs; verify before shipping) | HTTPS REST + WebSocket streaming; official Python/JS SDKs; no official Rust SDK but API is documented | API key (`E2B_API_KEY`) | **v1 reference** |
| Sprites  | TBD (API not yet hands-on validated) | TBD | TBD | TBD | **Deferred post-v1** |
| Daytona  | ~5–15 s (workspace create); resuming from snapshot ~2–4 s (estimate) | workspace-hour billing (cloud) or self-managed (on-prem) | gRPC + REST; official Go/Python SDKs | API key + workspace token | **Deferred post-v1** |

**Why E2B first.** E2B's HTTP API is fully documented, has an observable
pricing model with per-second granularity, persists processes between API calls
(critical for the shared-worker strategy), and is the only vendor with a
publicly-runnable sandbox trial. Sprites' API has not been hands-on validated;
Daytona introduces a two-level auth scheme (per-workspace token on top of API
key) that adds implementation complexity. Both are deferred until post-v1.

**Pricing note.** All per-second and per-GB figures are estimates based on
publicly available documentation and may change. The implementation must read
the vendor's documented pricing at the time of the commit rather than baking
these constants into the code.

## Architecture

### Trait placement

A remote sandbox implements the existing `SandboxProvider` trait
(`crates/pi-sandbox/src/provider.rs:62–95`) directly. **No new `RemoteTransport`
trait or `RemoteProvider` wrapper is added in v1.** The reasoning:

- `SandboxProvider` already captures everything the runtime needs: `name()`,
  `execute_tool()`, `cleanup()`, `honors_tool_dispatch()`.
- `MicroVmProvider` already does exactly the acquire→execute→release lifecycle
  internally without exposing that shape to the runtime.
- Adding a `RemoteTransport` layer would create a two-level dispatch chain
  (runtime → `RemoteProvider` → `Box<dyn RemoteTransport>`) for no concrete
  benefit in v1. If a second vendor is added post-v1, the shared code between
  two `SandboxProvider` impls will make the extraction point obvious.

The module layout ships as:

```
crates/pi-sandbox/src/remote/
    mod.rs           — pub use E2bProvider
    e2b.rs           — E2bProvider: SandboxProvider
    upload.rs        — SmartSync logic, exclusion lists
```

The `remote` module is always compiled (no `#[cfg]` gate); E2B calls are
behind a runtime API-key check, not a compile-time feature flag. A missing
`E2B_API_KEY` at session open returns `SandboxError::Unavailable(...)` with
a clear diagnostic.

### Remote tool availability matrix

The following table is the authoritative v1 answer for which tools are
available under `--sandbox-provider=e2b`. The runtime uses
`honors_tool_dispatch() = true` (the `SandboxProvider` default) so the
`ToolDispatch::Unavailable` short-circuit fires before any E2B call is made
for tools marked **Host-native** or **Unavailable**.

| Tool | `ToolDispatch` variant | E2B v1 status | Notes |
|------|------------------------|---------------|-------|
| `read` | `Guest` | ✅ Available | Standard guest tool |
| `write` | `Guest` | ✅ Available | Standard guest tool |
| `edit` | `Guest` | ✅ Available | Standard guest tool |
| `bash` | `Guest` | ✅ Available | Standard guest tool |
| `grep` | `Guest` | ✅ Available | Standard guest tool |
| `find` | `Guest` | ✅ Available | Standard guest tool |
| `ls` | `Guest` | ✅ Available | Standard guest tool |
| `monitor` | `Unavailable` | ❌ Unavailable | `MonitorTool::dispatch()` already returns `Unavailable` (`crates/pi-tools-core/src/monitor.rs:203–214`); streaming protocol incompatible with one-shot RPC |
| `lsp` | `Unavailable` | ❌ Unavailable | `LspTool::dispatch()` returns `Unavailable` (`crates/pi-coding-agent/src/native/lsp/tool.rs:101–115`); requires host-process language server state |
| `task` | `Guest` (default) | ⚠️ Host-native — see §"task and subagents" | `TaskTool` does not override `dispatch()` so it defaults to `Guest`; E2B implementation MUST override to `Unavailable` via `E2bProvider::honors_tool_dispatch()` bypass described below |
| `web_search` | `Guest` | ❌ Unavailable in v1 — see §"web_search" | vsock proxy path is not wired for remote transport; v1 marks as unavailable |
| `todo` | `Guest` | ✅ Available | Standard guest tool |
| `ask` | `Guest` | ✅ Available | Standard guest tool |

### `task` and subagents (Critical: addressed in v0.4)

**The problem.** `TaskTool` does not override `dispatch()`, so it defaults to
`ToolDispatch::Guest` (`crates/pi-tool-types/src/lib.rs:64–66`). With
`honors_tool_dispatch() = true`, the runtime would route `task` into the
remote sandbox — which fails because the worker has no concept of agent
spawning.

**The fix.** `E2bProvider` overrides `honors_tool_dispatch()` to return `false`
for tools that must remain host-native. This is implemented by extending the
dispatch short-circuit: before calling `execute_tool`, the runtime calls
`sandbox.is_host_native_tool(tool_name) -> bool`; if `true`, the tool is
invoked via the normal `tool.invoke()` path, bypassing the sandbox entirely.

**Concretely**, a new `fn host_native_tools(&self) -> &[&'static str]` method
is added to `SandboxProvider` (default: empty slice). `E2bProvider` overrides it
to return `&["task", "web_search"]`. The runtime checks this list before the
`honors_tool_dispatch()` path at `crates/pi-agent-core/src/runtime.rs:1688–1702`.

**Subagent session ownership.** When `task` runs host-natively, each child
runtime created by `executor::run_batch` (`crates/pi-coding-agent/src/native/
task/executor.rs:255–257`) inherits `sandbox_provider` from the parent's
`parent_cfg`. Each child runtime therefore gets its **own** `Arc<E2bProvider>`
clone. Each clone creates a **separate** E2B sandbox at its own `open()` call —
there is no shared WebSocket between parent and child sessions. Concurrency is
therefore safe: two parallel subagents each own a distinct `sandboxID`.

**Cost attribution.** Each child E2B session emits its own `SandboxAction`
telemetry rows under `provider = "e2b"`. The `pi --stats` cost query
aggregates by `provider` regardless of session depth, so total spend is
correctly captured without extra per-subagent logic.

### `web_search` (Critical: addressed in v0.4)

`pi-sandbox-worker`'s `dispatch()` function special-cases `web_search` and
proxies it to the host over vsock (`crates/pi-sandbox-worker/src/dispatch.rs:
183–261`, call site at lines 386–391`). This design relies on an open vsock
connection to the host and does not apply when the transport is
WebSocket/stdin/stdout.

**V1 decision: `web_search` is unavailable under `--sandbox-provider=e2b`.**

The `E2bProvider` adds `"web_search"` to its `host_native_tools()` list (see
§"`task` and subagents" above). The runtime invokes `web_search` via the
normal `tool.invoke()` path on the host, exactly as if no sandbox were
configured. The agent sees `web_search` results; those results are available
to subsequent `bash`/`write` tool calls inside the sandbox.

**Future path.** A post-v1 PR can add a reverse HTTP proxy inside the worker:
the worker forwards `web_search` requests to a configurable host endpoint
(similar to the vsock proxy design in `search_proxy.rs`) and the E2B session
receives results. This is deliberately out of scope for v1.

### Session lifecycle

**Lazy open.** `E2bProvider` is constructed at CLI/SDK startup (`from_auth()`).
The E2B sandbox is **not created** at construction time. `open()` is called on
the first `execute_tool()` invocation; subsequent calls reuse the same session.
This avoids the cold-boot cost on sessions that never need remote execution.

**Session state.** `E2bProvider` holds `Mutex<Option<E2bSandboxSession>>`.
`E2bSandboxSession` contains:
- `sandbox_id: String`
- `websocket: WebSocket` (the open stdin/stdout channel to the worker process)
- `execute_mutex: Mutex<()>` (serializes tool calls over the shared channel)

**Single-channel serialization.** All `execute_tool()` calls on one
`E2bProvider` instance are serialized through `execute_mutex`. This is correct
because the WebSocket stdin/stdout channel to the worker is a single
bidirectional stream — pipelining would require per-call correlation IDs in
the protocol (a future extension). For v1, sequential tool dispatch is
sufficient; `task` subagents get their own provider clone (see above) so
cross-subagent parallelism is not impeded.

**Cleanup caller.** `SandboxProvider::cleanup()` is called once at session
end. The caller is the `Drop` impl on the session object that wraps the
`AgentSessionRuntime` — specifically, cleanup is invoked in the `Drop` of
`AgentSession` which is currently a no-op in the base runtime
(`crates/pi-agent-core/src/runtime.rs`). **The E2B implementation commit must
wire cleanup explicitly.** The implementation plan (§"Commit G") requires:

1. Adding a `fn on_session_end(&self)` hook to `AgentSessionRuntime` that
   calls `sandbox_provider.cleanup().await` if a provider is configured.
2. Calling `on_session_end()` from `AgentSession::drop()` (or the explicit
   `session.close()` call in the top-level mode handlers, whichever lands first
   in the milestone branch).
3. If `cleanup()` fails, log at `tracing::warn!` and proceed — do not panic.

**Timeout-based safety net.** The `timeout` field on the E2B sandbox
(default: 3600 s, configurable via `E2B_SANDBOX_TIMEOUT_SECS`) bounds
runaway compute cost if the host process exits without calling `cleanup()`.
This is the only defense against host crashes; it is vendor-enforced.

**Abort path.** On `Ctrl-C` / SIGTERM, the Tokio runtime's shutdown future
drops active sessions before exit. The `Drop` impl above fires, calling
`cleanup()` synchronously (via `tokio::runtime::Handle::block_on`) to DELETE
the E2B sandbox and stop billing immediately.

### Worker-shipping strategy (Option A)

**E2B v1: Option A (ship `pi-sandbox-worker` into the remote sandbox).**

The `pi-sandbox-protocol` crate (`crates/pi-sandbox-protocol/src/lib.rs:7–9`)
states the protocol is "carried over a vsock connection in the local microVM
case (RFD 0023) and over any AsyncRead/AsyncWrite transport in the remote case
(RFD 0026)." The same worker binary runs over any compliant transport.

Option A for E2B:

1. At `E2bProvider::new()`, the host resolves the worker binary path:
   - `PI_SANDBOX_WORKER_BIN` env override, or
   - the binary adjacent to the running `pi` executable, or
   - fail loudly with `SandboxError::Unavailable("pi-sandbox-worker not found")`.
2. On first `execute_tool()` → `open()`:
   - Create an E2B sandbox via `POST /sandboxes` with the `base-template` (alpine).
   - Upload `pi-sandbox-worker` (statically linked musl, ~7 MB) to the sandbox
     via `POST /sandboxes/{id}/files` with path `/usr/local/bin/pi-sandbox-worker`.
   - `chmod +x` the binary via `POST /sandboxes/{id}/process/start`
     (`cmd = ["chmod", "+x", "/usr/local/bin/pi-sandbox-worker"]`).
   - Start the worker as a background process via `POST /sandboxes/{id}/process/start`
     with `cmd = ["/usr/local/bin/pi-sandbox-worker", "--transport", "stdin",
     "--work-dir", "/work", "--log-level", "warn"]`. The `--transport stdin` flag
     switches the worker into **stdin/stdout mode**. The startup response returns
     a `process_id`.
3. Worker readiness: poll `GET /sandboxes/{id}/process/{process_id}` until the
   process writes `READY\n` to stdout (≤ 2 s; fail after 5 s with
   `SandboxError::Unavailable("worker did not become ready")`).

**Why not Option B (per-vendor reimplementation)?**
Option B means 7 tools × N vendors = per-vendor drift. Option A requires a
one-time 7 MB upload but then every tool call goes through the same
`ToolRequest`/`ToolResponse` protocol as local microVM, so the test matrix
shared by the local path covers the remote path too.

**Worker packaging/discovery.** The `pi-sandbox-worker` binary is distributed
alongside the main `pi` binary in the release tarball. The discovery order
above (`PI_SANDBOX_WORKER_BIN` → adjacent binary) matches the existing
`PI_SANDBOX_WORKER_BIN` pattern already used in
`crates/pi-sandbox/src/microvm/provider.rs`. No new packaging machinery is
needed; the release bundle already contains both binaries.

### Transport: stdin/stdout via E2B process exec

Local microVM uses vsock. Remote vendors don't expose vsock to the embedder.
The E2B API exposes `POST /sandboxes/{id}/process/start` which opens a
WebSocket back-channel with the started process's stdin/stdout/stderr streams
as separate wire frames. This is the transport.

**Wire shape.** `pi-sandbox-worker` gains a `--transport stdin` flag
(defaulting to `vsock` for backward compat). In `stdin` mode:

- The worker reads `ToolRequest` JSON lines from its **stdin** (one object per
  line, newline-terminated — the same framing as the vsock path, implemented in
  `crates/pi-sandbox-protocol/src/framing.rs`).
- The worker writes `ToolResponse` JSON lines to its **stdout**.
- Stderr is the worker's tracing log; E2B forwards it via the WebSocket's `err`
  channel and the host logs it at `tracing::debug!`.

The host side opens a WebSocket connection to E2B's process endpoint and wraps
the stdin/stdout byte streams in a `tokio::io::DuplexStream` (or equivalent
adapter). The host then calls `pi_sandbox_protocol::framing::write_request` /
`read_response` exactly as in the local path. From the protocol crate's
perspective, the transport is opaque: any `AsyncRead + AsyncWrite`.

**Session lifetime.** The E2B sandbox + worker process persist for the lifetime
of the pi session. Each `execute_tool` call sends one `ToolRequest` and reads
one `ToolResponse` over the same open WebSocket. There is no per-call sandbox
create/destroy.

**Protocol version handshake.** The first thing the worker writes to stdout
after `READY\n` is a version line:
`{"proto_version":<N>}` where N = `pi_sandbox_protocol::CURRENT_PROTOCOL_VERSION` (1 as of v0.42).
The host verifies this and returns `SandboxError::Provider("protocol version mismatch...")` if it
disagrees.

### Cwd binding and file upload

**Default: `SmartSync`** — rsync-equivalent of the host cwd to the remote sandbox
at session open (`POST /sandboxes/{id}/files` in a batch). Aggressive default
exclusions on top of `.gitignore` to avoid the "500 MB monorepo" trap:

```
node_modules/   target/         .venv/      venv/         dist/
build/          __pycache__/    .next/      .nuxt/        .cache/
.gradle/        .terraform/     vendor/     bower_components/
*.pyc           *.class         *.o         *.so          *.dylib
```

Files larger than 100 MB each always require an explicit `--sandbox-upload-include` flag.
Upload progress is reported via the `SandboxAction` telemetry row emitted on
session open (`provider = "e2b"`, `tool_name = "<session_open>"`).

**Upload mechanism.** `POST /sandboxes/{id}/files` accepts a single file path +
body per request. For a session open with a typical project (~50 files / ~2 MB
after exclusions), the host walks the directory, filters per the exclusion list,
and fires concurrent file-upload requests (concurrency limit: 8, configurable
via `E2B_UPLOAD_CONCURRENCY`). Total session-open overhead target: ≤ 3 s for
a ≤ 5 MB upload.

**File mutation flushback.** When a guest tool writes to `/work/foo.rs`, the
worker emits a `WriteFile` entry in the `ToolResponse` (optional field
`file_writes: Vec<{path, base64_contents, mode}>` added to `ToolResponse` in
a MINOR-additive protocol extension). The host adapter applies each write to
the user's local cwd.

This `file_writes` field is NOT present in v0.42 of `pi-sandbox-protocol`.
Adding it is a MINOR-additive change (new `#[serde(default)]` field on
`ToolResponse`) that does NOT bump `CURRENT_PROTOCOL_VERSION` per the stability
contract in `crates/pi-sandbox-protocol/src/lib.rs:10–13`.

**Upload modes (v1: SmartSync only):**

| Mode | Behaviour | When to use | v1 status |
|------|-----------|-------------|-----------|
| `SmartSync` (default) | Upload filtered cwd at session open; flushback writes via `ToolResponse.file_writes` | Most tasks that touch local files | **v1** |
| `GitClone { url, rev }` | `git clone <url>` inside the sandbox; no upload | Read-only investigation; uncommitted changes not needed | **deferred v1.1** |
| `Empty` | No upload; `/work` is empty | Tasks that don't touch the user's project | **deferred v1.1** |

`GitClone` and `Empty` are deferred: they require a decision on write-flushback
semantics (do mutations persist locally or not?) that adds design surface without
being needed to validate the E2B integration. `SmartSync` alone is sufficient to
prove the architecture.

### Wire example: acquire → execute_tool("bash") → cleanup

```
// 1. Session open (lazy: triggered by first execute_tool call)
POST https://api.e2b.dev/sandboxes
  {"templateID": "base", "timeout": 3600, "metadata": {"pi_session": "<uuid>"}}
→ {"sandboxID": "abc123", ...}

// 2. Worker upload
POST https://api.e2b.dev/sandboxes/abc123/files?path=/usr/local/bin/pi-sandbox-worker
  Content-Type: application/octet-stream
  Body: <7 MB binary>

// 3. chmod + start worker (background)
POST https://api.e2b.dev/sandboxes/abc123/process/start
  {"cmd": ["/usr/local/bin/pi-sandbox-worker", "--transport", "stdin",
           "--work-dir", "/work", "--log-level", "warn"]}
  + WebSocket upgrade for stdin/stdout/stderr streams
→ {"processID": "proc_1", ...}

// 4. Worker readiness handshake (read from WebSocket stdout stream)
  ← "READY\n"
  ← "{\"proto_version\":1}\n"

// 5. File upload (SmartSync, concurrent, cwd = ~/myproject)
POST .../files?path=/work/src/main.rs   ← file body
POST .../files?path=/work/Cargo.toml    ← file body
... (8-way concurrent)

// 6. execute_tool("bash", {"command": "cargo build"})
  Host writes to WebSocket stdin channel (execute_mutex held):
    {"proto_version":1,"call_id":"c1","tool_name":"bash",
     "tool_input":{"command":"cargo build"},"max_output_bytes":262144,
     "timeout_ms":60000}\n

  Worker reads, dispatches pi-tools-core bash, writes to WebSocket stdout:
    {"call_id":"c1","stdout":"   Compiling myproject...\n","stderr":"",
     "exit_status":0,"guest_duration_ms":14200,"is_error":false,
     "file_writes":[]}\n

// 7. execute_tool("task", {"agent": "reviewer", ...})
  — TaskTool is in host_native_tools(); bypasses sandbox entirely.
  — tool.invoke() runs on host. Spawns child AgentSessionRuntime with
    its own Arc<E2bProvider> clone → own sandbox session.

// 8. execute_tool("web_search", {"query": "rust async"})
  — web_search is in host_native_tools(); bypasses sandbox entirely.
  — tool.invoke() runs on host.

// 9. cleanup (on session Drop or explicit on_session_end())
DELETE https://api.e2b.dev/sandboxes/abc123
```

### Security boundary

**Trust model.** Local microVM is a hardware boundary (KVM/HVF/WHPX): the guest
cannot read host memory or escape the VM. A remote sandbox with E2B is
**vendor-managed isolation** — we trust E2B to provide a clean per-session VM.
Pi-rs has no way to independently verify that the remote environment is
uncontaminated. This trust assumption is explicit in the design:

- Pi-rs does not bake credentials into uploaded files or environment variables
  passed to the worker binary. The worker receives only tool requests over stdin;
  it has no access to the host's `AuthStorage` or API keys.
- Each pi session maps to one E2B sandbox (one-to-one). Concurrent pi sessions
  in the same E2B account land in separate sandboxes.
- The sandbox's `timeout` field (set at sandbox create time) bounds runaway cost
  if the host crashes without calling `cleanup()`. Default: 3600 s. Override:
  `E2B_SANDBOX_TIMEOUT_SECS`.
- File uploads contain only the user's project files. `PI_` env vars, SSH keys,
  `~/.config/`, and any path outside the session cwd are never uploaded.

**Multi-tenant risk.** If multiple pi sessions run under the same E2B API key
(e.g., a shared CI account), each session is isolated in its own sandbox.
Sandbox A cannot read sandbox B's `/work` files — E2B's isolation is per-sandbox,
not per-account. Pi-rs does not add cross-session isolation on top of this;
embedders running a shared service should provision separate E2B API keys per
tenant (one key per user account is the E2B-recommended pattern).

**Adversarial example: vendor returns success but sandbox crashed.**
`POST /sandboxes/{id}/process/start` (worker start) returns 200 OK and a process
ID even if the binary can't be exec'd. Pi-rs defends via the readiness handshake
(step 4 above): if `READY\n` is not received within 5 s, the session open fails
with `SandboxError::Unavailable("worker did not become ready within deadline")`.
A worker that starts but crashes before processing a tool call will cause the
WebSocket to close; the host's `read_response` call returns
`ProtocolError::Eof`, which maps to `SandboxError::Provider("worker EOF")`.
The agent loop receives a failed tool execution and retries or reports the error.

**Adversarial example: vendor API returns 200 for DELETE but sandbox keeps running.**
Pi-rs does not verify sandbox termination after `cleanup()`. The sandbox's
server-side `timeout` is the safety net. If the vendor fails to honor DELETE,
the timeout caps the maximum cost exposure.

### Auth and key management

E2B requires a single API key. Key resolution order at `E2bProvider::new()`:

1. **`AuthStorage` lookup** — key `"e2b"` in the shared `AuthStorage` instance
   passed at construction (`AuthStorage` is the existing type at
   `crates/pi-ai/src/auth.rs`). This is the recommended path for embedders.
2. **`E2B_API_KEY` env var** — read at startup if `AuthStorage` has no `"e2b"`
   entry. Consistent with how LLM provider keys are read
   (`AuthStorage::from_env_explicit` pattern, `crates/pi-ai/src/auth.rs:144`).
3. **Fail** with `SandboxError::Unavailable("E2B API key not configured; set
   E2B_API_KEY or add key 'e2b' to AuthStorage")` if neither source has a key.

The key is never written to disk by pi-rs (it arrives via env or `AuthStorage`).
It is never included in uploaded files, `VmSpec.env`, or any telemetry row.

**CLI auth surface (future).** A `pi sandbox auth set e2b <key>` command would
store the key in `~/.pi/auth.json` via `AuthStorage::open(path)` at
`crates/pi-ai/src/auth.rs`. This CLI surface does **not** exist today and is
**not** part of the v1 implementation commit. V1 users configure via
`E2B_API_KEY` env var or the embedder `AuthStorage` API. The CLI polish is
deferred to a follow-up.

No new "SandboxAuthStorage" type is introduced; the existing `AuthStorage` is
sufficient (it's keyed by arbitrary provider name strings).

### Cost telemetry and lifecycle plumbing (Critical: addressed in v0.4)

**E2B billing signal.** E2B's API returns `cost_usd` in the `GET
/sandboxes/{id}` response (a per-sandbox accumulated cost since creation). This
is a polling endpoint, not a streaming one. Pi-rs reads it at `cleanup()` time
and emits a final `SandboxAction` row with `tool_name = "<session_end>"`.

**Current gaps and required changes.** Three changes are needed and are all
part of the E2B implementation commit (Commit G):

**1. Extend `SandboxExecution` with provider metadata.**

```rust
// crates/pi-sandbox/src/provider.rs — amendment
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SandboxExecution {
    pub stdout: String,
    pub stderr: String,
    pub exit_status: i32,
    /// Provider-supplied round-trip latency (remote sandboxes only; None for local).
    #[serde(default)]
    pub provider_round_trip_ms: Option<u32>,
}
```

`cost_usd` is not per-tool-call (E2B bills per-second for the whole sandbox);
it is only meaningful at session end. It does not belong on `SandboxExecution`.

**2. Extend `SessionEntryKind::SandboxAction`.**

```rust
// crates/pi-agent-core/src/session.rs — amendment
SandboxAction {
    provider: String,
    tool_name: String,
    duration_ms: u64,
    exit_status: i32,
    #[serde(default)]
    is_error: bool,
    #[serde(default)]
    cost_usd: Option<f64>,      // NEW: populated only on "<session_end>" rows
    #[serde(default)]
    round_trip_ms: Option<u32>, // NEW: from SandboxExecution.provider_round_trip_ms
}
```

**3. Wire `on_session_end()` in the runtime.**

Add `fn on_session_end(&self)` to `AgentSessionRuntime`. This function:
- Calls `sandbox_provider.cleanup().await` if a provider is configured.
- If `cleanup()` returns `Ok(())`, emits a `SandboxAction` row with
  `tool_name = "<session_end>"`, `cost_usd = Some(final_cost)` (fetched from
  `GET /sandboxes/{id}` before DELETE), `exit_status = 0`.
- If `cleanup()` returns `Err(e)`, logs at `tracing::warn!`, emits the row
  with `is_error = true`, `cost_usd = None`.

`on_session_end()` is called from:
- The top-level mode handlers (`modes/print.rs`, `modes/json.rs`) after the
  prompt loop returns, before process exit.
- The `Drop` impl on `AgentSession` as a best-effort fallback (calls
  `Handle::block_on`; fails silently if the runtime is already shut down).

**`pi --stats remote-cost` — deferred.** The CLI verb for aggregating remote
cost is deferred until the lifecycle + telemetry plumbing above are validated
in a real dogfood run. The data will be present in `sandbox_actions` rows;
a follow-up PR adds the aggregation query.

**Cost cap — deferred.** `E2B_MAX_COST_USD` polling is deferred to v1.1 to
keep the v1 scope manageable. The `timeout`-based safety net (vendor-side) is
sufficient for v1.

### Schema migration (Critical: addressed in v0.4)

**The problem.** `pi-stats` is schema version 1 and only does
`CREATE TABLE IF NOT EXISTS` (`crates/pi-stats/src/schema.rs:6–14`). That
does NOT add columns to existing databases. Existing installs will silently
miss the new columns.

**The fix.** The E2B implementation commit bumps `CURRENT_VERSION` to 2 in
`crates/pi-stats/src/schema.rs` and adds an explicit migration block:

```rust
// crates/pi-stats/src/schema.rs — amended ensure() function
pub const CURRENT_VERSION: i64 = 2;

pub fn ensure(conn: &Connection) -> rusqlite::Result<()> {
    // Phase 1: idempotent baseline (creates tables on fresh install).
    conn.execute_batch(BASELINE_DDL)?;

    // Phase 2: incremental migrations.
    let ver: i64 = conn
        .query_row("SELECT version FROM schema_version", [], |r| r.get(0))
        .unwrap_or(0);

    if ver < 2 {
        // Migration v1→v2: add remote-sandbox telemetry columns.
        // ALTER TABLE ADD COLUMN is idempotent in SQLite when the column
        // doesn't exist; we guard with a try so repeated runs are safe.
        let _ = conn.execute_batch(
            "ALTER TABLE sandbox_actions ADD COLUMN cost_usd      REAL;
             ALTER TABLE sandbox_actions ADD COLUMN round_trip_ms INTEGER;"
        );
        conn.execute("UPDATE schema_version SET version = 2", [])?;
    }

    Ok(())
}
```

`BASELINE_DDL` is the existing `CREATE TABLE IF NOT EXISTS` block (unchanged).
The migration runs on every `pi-stats` DB open. `ALTER TABLE ADD COLUMN` is
idempotent in SQLite (it fails if the column exists; the `let _ =` discards
that error). A fresh install runs the baseline DDL which already includes the
columns, then the migration is a no-op.

**`insert_sandbox_action`** in `crates/pi-stats/src/ingest.rs` is updated to
pass through the new nullable fields. Local microVM rows emit `NULL` for both;
the existing `IGNORE`-on-conflict insert stays valid.

**Test.** A new test in `crates/pi-stats/tests/migration_v2.rs` opens a v1
schema (no new columns), calls `ensure()`, and verifies both columns are present
and accept `NULL` values.

### Failure modes and `SandboxError` mapping

The existing `SandboxError` enum (`crates/pi-sandbox/src/provider.rs:26–46`) is
extended with two new variants:

```rust
#[error("remote sandbox rate limited; retry after {retry_after_secs}s")]
RateLimited { retry_after_secs: u32 },

#[error("remote sandbox billing exceeded or account suspended: {0}")]
BillingError(String),
```

All other vendor HTTP errors map to existing variants:

| HTTP / network condition | `SandboxError` variant |
|--------------------------|------------------------|
| 401 Unauthorized | `Unavailable("E2B API key invalid or revoked")` |
| 429 Too Many Requests | `RateLimited { retry_after_secs }` (from `Retry-After` header) |
| 402 Payment Required | `BillingError("account suspended or quota exceeded")` |
| 5xx / vendor down | `Unavailable("E2B API error: <status> <body>")` |
| Network timeout (> 30 s) | `Unavailable("E2B API request timed out")` |
| WebSocket closed unexpectedly | `Provider("worker EOF on stdin/stdout stream")` |
| File upload failure | `Provider("upload failed: <path>: <http-status>")` |

**Retry policy.** `RateLimited` triggers exponential backoff in the
`execute_tool` call path: 1 s, 2 s, 4 s, fail after 3 retries. All other error
variants fail immediately (no retry). Session open (`open()`) does not retry on
any error — the caller (the agent startup path) handles retry at a higher level
if desired.

**`PI_SANDBOX_OFFLINE=1`** (already used by the local microVM path) causes
`E2bProvider::new()` to return
`SandboxError::Unavailable("remote sandbox unavailable: PI_SANDBOX_OFFLINE=1")`.

### contextfs: not applicable for remote

The contextfs daemon (`crates/pi-sandbox/src/contextfs.rs`) bridges UDS ↔ vsock
for the local microVM case. Remote vendors do not support vsock from the host
side, and the E2B WebSocket stdin/stdout transport replaces vsock entirely. The
`file_writes` field on `ToolResponse` covers the file mutation flushback case
without contextfs. Contextfs is simply not wired into the remote path.

### RFD 0006 worktree compatibility

RFD 0006 worktrees operate at the git checkout level on the host. The
`--sandbox-provider=e2b` flag is orthogonal: the worktree's local cwd becomes
the `SmartSync` upload root. Each worktree-isolated task that requests E2B
gets its own `E2bProvider` (via the child runtime's inherited
`sandbox_provider` clone). This requires no special handling.

## Implementation plan

### Commit G (E2B reference implementation)

**New method on `SandboxProvider`:**
```rust
// crates/pi-sandbox/src/provider.rs
/// Tools that MUST be invoked host-natively, bypassing the sandbox,
/// even when `honors_tool_dispatch()` is true.
/// Default: empty (no host-native overrides).
fn host_native_tools(&self) -> &[&'static str] {
    &[]
}
```

**Runtime change** (`crates/pi-agent-core/src/runtime.rs`, near line 1688):
```rust
// Before the honors_tool_dispatch check, bypass tools that are host-native.
if sandbox.host_native_tools().contains(&call.name.as_str()) {
    // Invoke host-side, no sandbox telemetry row for this call.
    // (The tool — e.g. task, web_search — manages its own telemetry.)
} else if sandbox.honors_tool_dispatch() {
    // existing dispatch logic
}
```

**Module layout:**
```
crates/pi-sandbox/src/remote/
    mod.rs       (10 LOC — pub use E2bProvider)
    e2b.rs       (470 LOC — E2bProvider + E2bSandboxSession)
    upload.rs    (150 LOC — SmartSync + exclusion logic)
```

**Public API of `E2bProvider`:**
```rust
pub struct E2bProvider {
    api_key: String,
    base_url: String,        // default https://api.e2b.dev; override E2B_BASE_URL
    upload_mode: UploadMode,
    sandbox_timeout_secs: u32,
    session: Mutex<Option<E2bSandboxSession>>,
}

impl E2bProvider {
    pub fn from_auth(auth: &AuthStorage) -> Result<Self, SandboxError>;
    pub fn with_key(key: String) -> Self; // for tests and embedders
}

impl SandboxProvider for E2bProvider {
    fn name(&self) -> &'static str { "e2b" }
    fn host_native_tools(&self) -> &[&'static str] { &["task", "web_search"] }
    async fn execute_tool(...) -> Result<SandboxExecution, SandboxError>;
    async fn cleanup(&self) -> Result<(), SandboxError>;
}
```

**env vars:**
- `E2B_API_KEY` — API key fallback.
- `E2B_BASE_URL` — override API endpoint (for tests against a mock server).
- `E2B_SANDBOX_TIMEOUT_SECS` — sandbox lifetime cap (default 3600).
- `E2B_UPLOAD_CONCURRENCY` — file upload parallelism (default 8).
- `PI_SANDBOX_WORKER_BIN` — path to `pi-sandbox-worker` binary.
- `PI_SANDBOX_OFFLINE=1` — refuse remote transports.

**LoC estimate:** 630 LoC source + 200 LoC tests.

**Test strategy:**
- **Unit tests** gated on `E2B_API_KEY` env var (present in CI only on the
  self-hosted runner). Tests skip cleanly if the key is absent (mirror the
  `which::which("firecracker")` skip pattern).
- **`E2B_BASE_URL` mock server** — a minimal `axum` mock in
  `crates/pi-sandbox/tests/e2b_mock.rs` that responds with fixture JSON for
  `POST /sandboxes`, `POST /sandboxes/{id}/files`, `POST /sandboxes/{id}/process/start`,
  `DELETE /sandboxes/{id}`. Covers the upload and session lifecycle without a
  live API key.
- **Negative-path tests** (all in the mock server, no live key needed):
  - Missing API key → `SandboxError::Unavailable` at `from_auth()`.
  - `PI_SANDBOX_OFFLINE=1` → `SandboxError::Unavailable` at `from_auth()`.
  - Worker start timeout (mock returns 200 but never sends `READY\n`) →
    `SandboxError::Unavailable("worker did not become ready")` after 5 s.
  - `web_search` call → dispatched host-natively, not forwarded to E2B.
  - `task` call → dispatched host-natively, not forwarded to E2B.
  - `cleanup()` on normal exit → `DELETE /sandboxes/{id}` called exactly once.
  - `cleanup()` on `Drop` (abort path) → same, via `block_on`.
- **Protocol round-trip** — the existing `crates/pi-sandbox-protocol/tests/
  round_trip.rs` already exercises `ToolRequest`/`ToolResponse` framing over a
  `tokio::io::DuplexStream`; no new test needed for that layer.
- **Schema migration test** — `crates/pi-stats/tests/migration_v2.rs` opens a
  v1 schema and verifies `ensure()` adds the new columns without data loss.

### Commit H and I (Sprites, Daytona) — deferred

These land as separate PRs after the E2B reference implementation ships and the
`remote/` module shape is validated. Each will independently pick Option A or B
for the worker strategy after hands-on API validation. The module layout is:

```
crates/pi-sandbox/src/remote/
    sprites.rs   (Commit H)
    daytona.rs   (Commit I)
```

Neither gates on the other or on E2B changes.

## CLI wiring

```
pi --sandbox-provider=e2b [--sandbox-upload=smart-sync]
                          [--sandbox-upload-include=<glob>]
                          [--sandbox-upload-exclude=<glob>]
```

`--sandbox-provider=e2b` is added to `crates/pi-coding-agent/src/cli.rs`
alongside the existing `--sandbox-provider=microvm` variant. The startup path
in `crates/pi-coding-agent/src/startup.rs` instantiates `E2bProvider::from_auth`
when the flag is present.

## Open questions (v1 deferred)

1. **Cost-aware agent loop.** Should the agent see `cost_usd_remaining` and make
   routing decisions? v1: telemetry-only. Future: yes.
2. **`pi --stats remote-cost` CLI verb.** Deferred until dogfood validates the
   telemetry pipeline is populated correctly.
3. **Multi-region selection.** E2B supports multiple regions. Default = platform
   default; future `E2B_REGION` env var.
4. **Session warm pool.** E2B supports "template" sandboxes with a pre-warmed
   state. Future: pre-upload the worker binary into a custom template to avoid
   the per-session 7 MB upload cost.
5. **Sprites/Daytona API validation.** Deferred to Commits H/I.
6. **`GitClone` / `Empty` upload modes.** Deferred to v1.1; requires pinning
   write-flushback semantics for read-only vs. cloned sources.
7. **`web_search` remote host-bridge.** Deferred to post-v1; v1 runs
   `web_search` host-natively.
8. **`pi sandbox auth set e2b <key>` CLI.** Deferred to follow-up;
   v1 uses `E2B_API_KEY` env var.
9. **`E2B_MAX_COST_USD` cap polling.** Deferred to v1.1.

## Out of scope

- **Self-hosted Daytona deployment.** This RFD covers SDK/API integration;
  provisioning a Daytona instance is the user's problem.
- **Cross-vendor migration.** No "switch from E2B to Sprites mid-session."
  Sandbox provider is fixed at session start.
- **contextfs over remote.** Not needed; the `file_writes` flushback covers the
  mutation case without a FUSE-based bridge.
- **Sandbox snapshot/restore API.** E2B supports snapshots; not exercised in v1.

## Revision history

- **v0.4 (2026-05-17):** Addressed all critical issues from rfd-critic v0.3
  review. (1) Added remote tool availability matrix; pinned `task` and
  `web_search` as host-native via new `host_native_tools()` method on
  `SandboxProvider`; documented that `TaskTool` defaults to `ToolDispatch::Guest`
  and the fix for it. (2) Resolved subagent/concurrency: each child runtime gets
  its own `E2bProvider` clone → own sandbox session; single-channel serialization
  via `execute_mutex`. (3) Pinned telemetry/lifecycle plumbing: extend
  `SandboxExecution` with `provider_round_trip_ms`; extend `SandboxAction` with
  `cost_usd` + `round_trip_ms`; add `on_session_end()` runtime hook with
  explicit callers; defer `pi --stats remote-cost` and cost-cap polling to
  follow-up. (4) Fixed schema migration: bump `CURRENT_VERSION` to 2, add
  `ALTER TABLE` guards, add migration test. (5) Fixed broken commit citations
  (`b98e06c`/`441aa85` were wrong; replaced with actual landing commits and
  removed SHA claims for the RFD 0023 clearing event). (6) Moved CLI auth
  (`pi sandbox auth set`) to deferred. (7) Softened vendor pricing to estimates
  with doc URL. (8) Deferred `GitClone`/`Empty`/cost-cap/remote-cost CLI.
  Added cleanup abort-path, worktree compatibility, worker packaging notes.
- **v0.3 (2026-05-16):** Full specification pass. Pinned E2B as v1 reference
  vendor; moved Sprites/Daytona to "deferred post-v1" with explicit rationale.
  Fixed trait architecture: removed proposed `RemoteTransport` / `RemoteProvider`
  layers in favour of direct `SandboxProvider` implementation. Pinned transport
  to E2B WebSocket stdin/stdout. Locked worker-shipping strategy to Option A.
  Auth: reuses existing `AuthStorage`. Cost telemetry: specified `cost_usd`
  additions to `SandboxAction`. Failure modes: added `RateLimited` / `BillingError`
  variants. Added concrete wire example. Explicit security trust model +
  adversarial example.
- **v0.2 (2026-05-02):** In-repo `rfd-critic` pass + cross-RFD coordination
  with 0023 v0.4. Reframed the worker-shipping (A vs B) decision as per-vendor;
  clarified that the worker binary + protocol are owned by RFD 0023. Aligned
  telemetry on RFD 0023 v0.4's union schema. Renamed `UploadStrategy::SyncCwd`
  → `SmartSync`. Specified file-mutation flushback semantics.
- **v0.1 (2026-05-02):** Initial stub split out of RFD 0023 v0.2 per critical
  review feedback.

## References

- **RFD 0022** — `SandboxProvider` trait (`crates/pi-sandbox/src/provider.rs`).
- **RFD 0023** — Local MicroVM Sandbox; `pi-sandbox-protocol` + `pi-sandbox-worker`.
- **RFD 0005** — `task` tool / subagents (`crates/pi-coding-agent/src/native/task/`).
- **RFD 0006** — Worktree-isolated tasks.
- **RFD 0027** — Pi-rs as a Self-Contained Rust SDK; `pi-sdk` façade crate.
- `crates/pi-sandbox/src/provider.rs` — `SandboxProvider`, `SandboxError`, `SandboxExecution`.
- `crates/pi-sandbox-protocol/src/lib.rs` — `ToolRequest`, `ToolResponse`, `CURRENT_PROTOCOL_VERSION`.
- `crates/pi-sandbox-protocol/src/framing.rs` — JSON-line framing helpers.
- `crates/pi-sandbox-worker/src/dispatch.rs:183–261, 386–391` — `web_search` vsock proxy (host-native for remote path).
- `crates/pi-sandbox-worker/src/main.rs` — guest worker binary (vsock mode today; stdin mode added by Commit G).
- `crates/pi-tool-types/src/lib.rs:56–66` — `ToolDispatch` enum + default (`Guest`).
- `crates/pi-tools-core/src/monitor.rs:203–214` — `MonitorTool::dispatch()` returns `Unavailable`.
- `crates/pi-coding-agent/src/native/lsp/tool.rs:101–115` — `LspTool::dispatch()` returns `Unavailable`.
- `crates/pi-coding-agent/src/native/task/tool.rs:32` — `TaskTool` impl (no `dispatch()` override; defaults to `Guest`).
- `crates/pi-coding-agent/src/native/task/executor.rs:255–257` — child runtime inherits `sandbox_provider`.
- `crates/pi-ai/src/auth.rs` — `AuthStorage` (key resolution, 0o600 on-disk).
- `crates/pi-agent-core/src/session.rs:91` — `SessionEntryKind::SandboxAction`.
- `crates/pi-stats/src/schema.rs:6–14, 81–97` — schema v1 DDL, `sandbox_actions` table.
- `crates/pi-stats/src/ingest.rs:141` — `SandboxAction` ingestion.
- `crates/pi-stats/src/aggregate.rs:98` — `by_sandbox_provider` aggregation pattern.
- **E2B API docs** — https://e2b.dev/docs.
- **Sprites** — (deferred; URL TBD at Commit H time).
- **Daytona** — https://daytona.io/docs.
