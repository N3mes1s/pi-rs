# RFD 0026 — Remote Sandbox Transports (E2B v1 reference, Sprites/Daytona deferred)

- **Status:** Discussion (v0.24)
- **Author:** pi-rs maintainers
- **Created:** 2026-05-02
- **Implemented:** (pending)

## Summary

Sister RFD to **RFD 0023** (local microVM sandbox). Where RFD 0023 ships local
virtualization that converges three OS-specific launchers on one Linux guest
rootfs, this RFD covers **remote microVM-as-a-service vendors**. The dependency
on RFD 0023 is now cleared: `MicroVmLauncher` (commit 77184cc),
`pi-sandbox-protocol` (commit aa624e0), and `pi-sandbox-worker` (commit dcd37cd)
all landed in main at v0.42.

**v1 scope.** E2B is the v1 reference vendor; Sprites and Daytona move to
"future vendors" until their APIs are hands-on validated. A remote sandbox is
a `SandboxProvider` implementation (the same trait at
`crates/pi-sandbox/src/provider.rs:62`) that talks to the vendor over TLS/HTTPS
instead of vsock.

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
| **E2B**  | ~1.5–3 s  | $0.000084/s compute + $0.000225/s storage (public pricing as of 2026-05) | HTTPS REST; official Python/JS SDKs; no official Rust SDK but API is documented | API key (`E2B_API_KEY`) | **v1 reference** |
| Sprites  | TBD (API not yet hands-on validated) | TBD | TBD | TBD | **Deferred post-v1** |
| Daytona  | ~5–15 s (workspace create); resuming from snapshot ~2–4 s | workspace-hour billing (cloud) or self-managed (on-prem) | gRPC + REST; official Go/Python SDKs | API key + workspace token | **Deferred post-v1** |

**Why E2B first.** E2B's HTTP API is fully documented, has an observable
pricing model with per-second granularity, preserves the sandbox filesystem
across multiple command invocations within the same sandbox ID (so the
one-shot-worker-per-call design can share `/work` state between tool calls),
and is the only vendor with a publicly-runnable sandbox trial. Sprites' API has
not been hands-on validated; Daytona introduces a two-level auth scheme
(per-workspace token on top of API key) that adds implementation complexity.
Both are deferred until post-v1.

## Architecture

### Trait placement

A remote sandbox implements the existing `SandboxProvider` trait
(`crates/pi-sandbox/src/provider.rs:62`) directly. **No new `RemoteTransport`
trait or `RemoteProvider` wrapper is added in v1.** The reasoning:

- `SandboxProvider` (`crates/pi-sandbox/src/provider.rs:62–97`) already captures
  everything the runtime needs: `name()`, `execute_tool()`, `cleanup()`,
  `honors_tool_dispatch()`.
- `MicroVmProvider` already does exactly the acquire→execute→release lifecycle
  internally without exposing that shape to the runtime.
- Adding a `RemoteTransport` layer would create a two-level dispatch chain
  (runtime → `RemoteProvider` → `Box<dyn RemoteTransport>`) for no concrete
  benefit in v1. If a second vendor is added post-v1, the shared code between
  two `SandboxProvider` impls will make the extraction point obvious.

The module layout ships as:

```
crates/pi-sandbox/src/remote/
    mod.rs           — pub mod e2b; pub mod upload;
    e2b.rs           — E2bProvider: SandboxProvider
    upload.rs        — SmartSync logic, exclusion lists
```

> **Note:** `crates/pi-sandbox/src/remote/` is a **future file** (does not
> exist yet). It is created in Commit G (the E2B implementation commit).

The `remote` module is always compiled (no `#[cfg]` gate); E2B calls are
behind a runtime API-key check, not a compile-time feature flag. A missing
`E2B_API_KEY` at session open returns `SandboxError::Unavailable(...)` with
a clear diagnostic.

### Worker-shipping strategy (Option A, per-vendor decision)

**E2B v1: Option A (ship `pi-sandbox-worker` into the remote sandbox).**

The `pi-sandbox-protocol` crate doc (`crates/pi-sandbox-protocol/src/lib.rs:1–13`)
states the design explicitly: the protocol is "carried over a vsock connection in
the local microVM case (RFD 0023) and over any AsyncRead/AsyncWrite transport in
the remote case (RFD 0026)." The worker binary (`crates/pi-sandbox-worker/src/main.rs`)
currently only supports vsock; the `--transport stdin` flag is added as part of
the E2B implementation commit (see §"Worker changes required").

Option A for E2B:

1. **Worker binary path is resolved lazily on the first `execute_tool` call**
   (not at construction). Both `from_auth()` and `with_key()` are cheap and
   infallible with respect to the worker path — they do not check
   `PI_SANDBOX_WORKER_BIN`. This design means:
   - Tests can construct `E2bProvider::with_key(...)` in unit/mock tests
     without needing `PI_SANDBOX_WORKER_BIN` set.
   - The failure ("worker binary missing") surfaces on the first real
     execute call, not at startup.
   - At first `execute_tool`: check `PI_SANDBOX_WORKER_BIN` env var (the
     **only** supported v1 path). If absent or not executable, return
     `SandboxError::Unavailable("PI_SANDBOX_WORKER_BIN not set; \
     pi-sandbox-worker must be built separately and its path set via this env var")`.
   - **There is no "adjacent to the `pi` executable" fallback in v1.**
     `pi-sandbox-worker` is distributed inside the rootfs artifact (see
     `RELEASING.md:71` and `crates/pi-sandbox-rootfs/build.sh:100–105`),
     not as a standalone binary next to `pi`. A normal `pi` install does
     not have a `pi-sandbox-worker` on `PATH` or adjacent to the binary.
   - v1 requires the operator to either (a) build `pi-sandbox-worker` from
     a checkout (`cargo build -p pi-sandbox-worker --target x86_64-unknown-linux-musl --release`)
     and set `PI_SANDBOX_WORKER_BIN` to its path, or (b) extract it from
     a rootfs artifact. Adding a standalone distribution path (shipping
     `pi-sandbox-worker` as an independent release artifact) is deferred
     to a post-v1 packaging improvement.
   - `pi sandbox doctor` (the existing diagnostic helper) should report
     whether `PI_SANDBOX_WORKER_BIN` is set and the file is executable when
     `--sandbox-provider=e2b` is in scope. Amending `doctor` is tracked as
     a follow-up; for v1 the env-var path is the canonical setup.
2. On first `execute_tool` call (lazy session open, after worker path check):
   - Create an E2B sandbox via `POST /sandboxes` with the `base-template` (alpine).
   - Upload `pi-sandbox-worker` (statically linked musl, ~7 MB) to the sandbox
     via `POST /sandboxes/{id}/files` with path `/usr/local/bin/pi-sandbox-worker`.
   - `chmod +x` the binary via `POST /sandboxes/{id}/commands` (sync command,
     polled to completion via `GET /sandboxes/{id}/commands/{cmd_id}`).
   - Upload the host cwd via SmartSync (see §"Cwd binding and file upload").
3. For each `execute_tool` call (including the first, after setup completes):
   launch a **fresh one-shot worker process** (not a background daemon) via
   `POST /sandboxes/{id}/commands` with the `ToolRequest` JSON line as `stdin`.
   The worker reads one request, dispatches the tool, writes one `ToolResponse`
   to stdout, and exits. The host polls `GET /sandboxes/{id}/commands/{cmd_id}`
   until `finished == true`, then parses `ToolResponse` from the `stdout` field.

**There is no background daemon and no READY handshake.** E2B preserves the
sandbox filesystem (including `/work`) across command invocations within the
same sandbox ID, so the one-shot model shares persistent `/work` state correctly
between tool calls without a long-lived process.

**Partial session-open failure cleanup.** If any step of lazy session open fails
after the sandbox is created (e.g. worker upload fails, `chmod` command times out,
or SmartSync hits a network error), the provider MUST best-effort issue
`DELETE /sandboxes/{id}` (errors from the DELETE are logged as warnings and
ignored), then clear the internally stored sandbox ID so the session state is
clean. The error is returned to the caller as `SandboxError::Provider(...)`. The
next call to `execute_tool()` will reattempt the full lazy session open from
scratch (a fresh `POST /sandboxes`). This is a clean retry-from-scratch policy;
the provider does NOT enter the poisoned state on setup failure (poison is reserved
for post-setup flushback divergences where host/guest cwd state is known to have
split).

**Why not Option B (per-vendor reimplementation)?**
Option B means 7 tools × N vendors = per-vendor drift. Option A requires a
one-time 7 MB upload but then every tool call goes through the same
`ToolRequest`/`ToolResponse` protocol as local microVM, so the test matrix
shared by the local path covers the remote path too.

**Sprites / Daytona: decision deferred.** Each deferred vendor will pick A or
B in its own implementation PR once the API is hands-on validated. There is no
guarantee Sprites allows running an arbitrary binary; if it doesn't, Option B
is accepted for that vendor only.

### Worker changes required

The current `pi-sandbox-worker` binary (`crates/pi-sandbox-worker/src/main.rs`)
is Linux-only and only supports vsock via `listener::serve(vsock_port, work_dir)`.
The E2B implementation commit adds a `--transport` CLI flag to `Cli`:

```rust
// crates/pi-sandbox-worker/src/main.rs — amendment
#[derive(clap::ValueEnum, Clone, Debug, Default)]
enum Transport {
    #[default]
    Vsock,
    Stdin,
}

#[derive(Parser, Debug)]
struct Cli {
    // ... existing fields ...
    #[arg(long = "transport", default_value = "vsock")]
    transport: Transport,
}
```

When `--transport stdin`, the main function calls a new
`listener::serve_stdio(work_dir)` helper instead of `listener::serve(vsock_port, work_dir)`.
`serve_stdio` wraps `tokio::io::stdin()` + `tokio::io::stdout()` in
`tokio::io::BufReader` / `tokio::io::BufWriter` and calls the same
`dispatch::dispatch_request` function the vsock path already uses. No changes
to `pi-sandbox-protocol` are required; the framing is identical.

### Transport: stdin/stdout via E2B command/poll

Local microVM uses vsock. Remote vendors don't expose vsock to the embedder.

The E2B API exposes a command execution model:
- `POST /sandboxes/{id}/commands` — start a command (sync or background).
  Returns a `cmd_id`.
- `GET /sandboxes/{id}/commands/{cmd_id}` — poll status; returns
  `{stdout, stderr, exit_code, finished}`.

**No WebSocket dependency.** The transport is pure HTTPS request/response using
`reqwest` (already at `Cargo.toml:35`). No `tokio-tungstenite` or equivalent
is required.

**Wire shape for per-tool execution.** For each `execute_tool` call, the host:

1. `POST /sandboxes/{id}/commands` with:
   - `cmd = ["/usr/local/bin/pi-sandbox-worker", "--transport", "stdin",
     "--work-dir", "/work", "--log-level", "warn"]`
   - `stdin = "<ToolRequest JSON line>\n"`
   Returns a `cmd_id`.
2. Poll `GET /sandboxes/{id}/commands/{cmd_id}` until the response body carries
   `"finished": true`. **Polling interval:** sleep 100 ms before the first
   check, then 200 ms between subsequent checks. **Poll timeout:** the
   `ToolRequest.timeout_ms` value plus a 5-second overhead for process
   startup/shutdown, after which the host returns
   `SandboxError::Timeout` (the command is not explicitly cancelled; E2B's
   own sandbox-level timeout serves as the backstop).
3. Parse `ToolResponse` from the response `stdout` field.
   If `stdout` is absent or does not parse as a valid `ToolResponse`,
   return `SandboxError::Provider("worker exited with code N; no valid ToolResponse in stdout")`.

The worker is **one-shot per call**: it reads one `ToolRequest` from stdin,
dispatches the tool, writes one `ToolResponse` to stdout, and exits. The
`serve_stdio` helper calls `dispatch_request` once and returns. No background
daemon; no READY handshake. State shared between tool calls (source files, build
artifacts, etc.) lives in the E2B sandbox filesystem at `/work`, which E2B
preserves across command invocations within the same sandbox.

**Worker mode in E2B v1: one-shot.** This design works because:
- E2B preserves the `/work` directory between commands in the same sandbox.
- The `/work` directory is populated once at session open (SmartSync upload).
- File mutations are returned inline via `file_writes` in the `ToolResponse`.

**Protocol version handshake.** Omitted in one-shot mode: the host sends the
`ToolRequest` directly and reads the `ToolResponse`. The `proto_version` field
in `ToolRequest` still carries `CURRENT_PROTOCOL_VERSION` for compatibility
checking; the worker validates it and returns `is_error: true` on mismatch.

**Session lifetime.** The E2B sandbox + its `/work` filesystem persist for the
lifetime of the pi session. Each `execute_tool` call fires a new command into
the same sandbox. There is no per-call sandbox create/destroy.

### File-mutation flushback: scope and protocol extension

#### v1 scope: explicit-tool mutations only

**`bash` tool is excluded from `file_writes` in v1.** Detecting arbitrary
filesystem mutations from `bash` commands requires either:
(a) a before/after directory tree diff, or (b) an inotify/fanotify watcher.
Neither is implemented in `crates/pi-sandbox-worker/src/dispatch.rs` today (the
worker simply runs the tool and returns `ToolResponse`), and bolt-on detection
would need to handle deletes, renames, symlinks, directory creates/removes, and
`chmod`-only changes. That is v2+ scope.

**v1 covers only `write` and `edit` tool calls.** These tools produce a single
known output path via `tool_input.path` and write known content — exactly
what the `FileWrite` struct represents. The worker already validates
`tool_input.path` against `work_dir` in
`crates/pi-sandbox-worker/src/dispatch.rs:102` (`validate_paths`), so the path is
trusted after that check. After the tool's `invoke()` returns, the worker reads
the file at the validated path and attaches it to `ToolResponse.file_writes`.

The host advertises v1 file-writes support to the agent at session startup.
When `--sandbox-provider=e2b` is selected, the `install_sandbox_from_flag`
function in `crates/pi-coding-agent/src/startup.rs` appends the following note
to `cfg.system_prompt` (the `RuntimeConfig` field assembled before session
construction). This is where all other runtime-specific system-prompt additions
land (e.g. sandbox mode notes); `SandboxProvider` itself has no prompt hook.

```
Note: working in a remote E2B sandbox. File mutations made via the
`write` and `edit` tools are synced back to your local directory.
Shell (`bash`) changes to files are NOT automatically synced — use
`write` or `edit` to persist changes that need to be reflected locally.
```

**Protocol version bump.** The `deny_unknown_fields` constraint on both
`ToolRequest` and `ToolResponse` (`crates/pi-sandbox-protocol/src/lib.rs:28,48`)
makes adding `file_writes` to `ToolResponse` a **BREAKING** change, not a
MINOR-additive one. Resolution: bump `CURRENT_PROTOCOL_VERSION` from 1 to 2 in
the same commit.

```rust
// crates/pi-sandbox-protocol/src/lib.rs — amendment
pub const CURRENT_PROTOCOL_VERSION: u32 = 2;  // was 1

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct ToolResponse {
    pub call_id: String,
    pub stdout: String,
    pub stderr: String,
    pub exit_status: i32,
    pub guest_duration_ms: u32,
    pub is_error: bool,
    /// File mutations produced by this tool call.
    /// v1: only `write` and `edit` tools populate this field.
    /// `bash` always emits an empty vec.
    /// The local microVM path always emits an empty vec.
    #[serde(default)]
    pub file_writes: Vec<FileWrite>,
}

/// A single file mutation from the guest worker.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct FileWrite {
    /// Path relative to `/work` (the guest's working directory).
    pub path: String,
    /// File contents, base64-encoded.
    pub contents_b64: String,
    /// Unix permission bits (e.g. 0o644).
    pub mode: u32,
}
```

**Backward compatibility.** The local `pi-sandbox-worker` binary (vsock path)
is updated in the same commit: it bumps its `proto_version` field to 2 and
emits `file_writes: []` (empty) in every response. Local `MicroVmProvider`
sessions ignore `file_writes`; no behavioral change.

**Version gate.** The host's `framing::read_request_with_max`
(`crates/pi-sandbox-protocol/src/framing.rs:51`) already validates
`proto_version == CURRENT_PROTOCOL_VERSION`. After the bump, any old worker
(v1) talking to a new host (v2) gets `ProtocolError::VersionMismatch`.

#### Size cap and fallback

`file_writes` is an inline base64 payload in the `ToolResponse` JSON line. The
existing frame cap (`DEFAULT_MAX_LINE_BYTES = 64 KiB`,
`crates/pi-sandbox-protocol/src/framing.rs:32`) applies. For v1:

- **Per-file size limit enforced by the worker.** Before populating
  `file_writes`, the worker checks that the file size is ≤ 32 KiB unencoded.
  **Frame math:** 32 KiB × 4/3 (base64 overhead) ≈ 43 KiB for
  `contents_b64`, plus ~300 bytes for other JSON fields (`call_id`, `stdout`,
  `stderr`, `exit_status`, `guest_duration_ms`, `is_error`, field names),
  total ≈ 43.3 KiB — safely under the 64 KiB frame cap. For `write`/`edit`
  tools, `stdout` is a short confirmation string ("Written N bytes to path"),
  so the envelope is not at risk of exceeding the cap.
  If the file exceeds 32 KiB, the worker returns `is_error: true` with
  `stdout = "remote sync error: file too large for inline flushback (<N> bytes);
  use bash to split or compress first"`. The worker is the enforcement point
  because the `ToolResponse` is constructed in the worker process, not on the
  host. In v1 the host does not fall back to an out-of-band download; the error
  surfaces to the agent, which can retry with a smaller write. Out-of-band
  fallback (e.g. `GET /sandboxes/{id}/files?path=...`) is v2+ scope.
- **Normal case.** `write` and `edit` are bounded by the agent's negotiated
  `ToolRequest.max_output_bytes` limit and the tool's own input constraints.
  Typical edits are well under 32 KiB; the cap primarily guards against
  pathological inputs.

**Host flushback semantics.** After receiving a `ToolResponse`, `E2bProvider`
applies `file_writes` to the host cwd atomically (temp-write + rename). Files
are written to `<host_cwd>/<relative_path>` with the mode from
`file_writes.mode`.

**Flushback failure recovery: session poison.** If any host-side file write
fails (e.g. permission error, disk full), the tool call completes but the
provider **poisons the session** — it sets an internal `poisoned: bool` flag
and immediately calls `DELETE /sandboxes/{id}` (best-effort; errors logged as
warnings). All subsequent `execute_tool()` calls on a poisoned provider return
immediately with:

```
SandboxError::Provider(
    "E2B session desynced after flushback failure on '<path>': \
     <original error>. Restart the pi session to recover."
)
```

**Rationale.** A failed host flushback means the remote `/work` and the host
cwd have diverged: the remote already has the new file content while the host
does not. Continuing the session under these conditions would cause the agent to
operate on a false view of the host filesystem. The poison approach is the
simplest v1 contract that prevents silent data loss: it surfaces the divergence
immediately and forces a clean restart rather than masking the problem.

The agent sees a clear error on the next tool call and the session is gone. The
user is not left with a half-committed edit they cannot see.

**What is NOT a flushback failure.** A `ToolResponse` with `is_error: true` and
`file_writes: []` (e.g. from a `write` tool that failed inside the sandbox) is a
normal tool error and does NOT poison the session — there was no successful
guest-side write, so host and remote are still in sync.

### Provider/runtime contract amendments for remote telemetry

#### Problem

`SandboxExecution` (`crates/pi-sandbox/src/provider.rs:15–22`) currently
carries only `stdout`, `stderr`, and `exit_status`. The runtime emits
`SessionEntryKind::SandboxAction` directly from that surface
(`crates/pi-agent-core/src/runtime.rs:1714–1723`). There is no path for a
provider to feed `cost_usd` or `round_trip_ms` into telemetry.

#### Resolution: extend `SandboxExecution`

```rust
// crates/pi-sandbox/src/provider.rs — amendment
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SandboxExecution {
    pub stdout: String,
    pub stderr: String,
    pub exit_status: i32,
    /// Remote-only: E2B API call round-trip latency.
    /// `None` for local-process and microVM providers.
    #[serde(default)]
    pub round_trip_ms: Option<u32>,
    /// Remote-only: estimated per-call cost in USD
    /// (compute_rate × elapsed). `None` for local providers.
    #[serde(default)]
    pub cost_usd: Option<f64>,
}
```

`LocalProcessProvider` and `MicroVmProvider` continue to construct
`SandboxExecution` with `round_trip_ms: None, cost_usd: None`; no
behavioral change for existing tests.

The runtime's `invoke_via_sandbox` reads these fields and threads them into
`SessionEntryKind::SandboxAction`.

**Exact runtime.rs change required.** Today `invoke_via_sandbox` returns
`Result<ToolResult, String>` and discards the `SandboxExecution` metadata:

```rust
// crates/pi-agent-core/src/runtime.rs:1815–1830 (current)
async fn invoke_via_sandbox(
    &self, provider: &dyn SandboxProvider, ctx: &ToolContext, call: &ToolCall,
) -> Result<ToolResult, String> {
    match provider.execute_tool(ctx, &call.name, &call.input).await {
        Ok(exec) => Ok(ToolResult {
            tool_use_id: call.id.clone(),
            model_output: exec.stdout,
            display: None,
            is_error: exec.exit_status != 0,
        }),
        Err(e) => Err(e.to_string()),
    }
}
```

The amended signature returns the `SandboxExecution` alongside the `ToolResult`
so the telemetry emit site can read `cost_usd` / `round_trip_ms`:

```rust
// crates/pi-agent-core/src/runtime.rs — amendment
async fn invoke_via_sandbox(
    &self, provider: &dyn SandboxProvider, ctx: &ToolContext, call: &ToolCall,
) -> Result<(ToolResult, SandboxExecution), String> {
    match provider.execute_tool(ctx, &call.name, &call.input).await {
        Ok(exec) => {
            let result = ToolResult {
                tool_use_id: call.id.clone(),
                model_output: exec.stdout.clone(),
                display: None,
                is_error: exec.exit_status != 0,
            };
            Ok((result, exec))
        }
        Err(e) => Err(e.to_string()),
    }
}
```

The call site (currently `runtime.rs:1699–1704`) is updated to destructure the
tuple and extract telemetry metadata before the `SandboxAction` emit:

```rust
// crates/pi-agent-core/src/runtime.rs — call-site amendment
let res = if sandbox.honors_tool_dispatch() {
    match tool.dispatch() {
        ToolDispatch::Unavailable { reason } => Err(format!(...)),
        ToolDispatch::Guest => self.invoke_via_sandbox(sandbox.as_ref(), &tool_ctx, &call).await,
    }
} else {
    self.invoke_via_sandbox(sandbox.as_ref(), &tool_ctx, &call).await
};
let duration_ms = started.elapsed().as_millis() as u64;
let (exit_status, is_error, cost_usd, round_trip_ms) = match &res {
    Ok((r, exec)) => (
        exec.exit_status,   // use the real exit code from the provider
        r.is_error,
        exec.cost_usd,
        exec.round_trip_ms,
    ),
    Err(_) => (1, true, None, None),
};
let _ = self.cfg.session_manager.append(
    &self.id,
    SessionEntryKind::SandboxAction {
        provider: sandbox.name().to_string(),
        tool_name: call.name.clone(),
        duration_ms,
        exit_status,
        is_error,
        cost_usd,
        round_trip_ms,
    },
);
// Unwrap the ToolResult from the tuple for the caller:
let res = res.map(|(tool_result, _exec)| tool_result);
```

`LocalProcessProvider` and `MicroVmProvider` emit `SandboxExecution` with
`round_trip_ms: None, cost_usd: None`; the call-site destructure is a no-op
for them.

```rust
// crates/pi-agent-core/src/session.rs — amended SandboxAction variant
SandboxAction {
    provider: String,
    tool_name: String,
    duration_ms: u64,
    exit_status: i32,
    #[serde(default)]
    is_error: bool,
    #[serde(default)]
    cost_usd: Option<f64>,      // NEW: from SandboxExecution
    #[serde(default)]
    round_trip_ms: Option<u32>, // NEW: from SandboxExecution
}
```

The `sandbox_actions` table gains two nullable columns. Because SQLite's
`CREATE TABLE IF NOT EXISTS` does not alter existing tables, the Commit G
schema change uses an explicit migration:

```rust
// crates/pi-stats/src/schema.rs — amendment
pub const CURRENT_VERSION: i64 = 2;  // was 1

pub fn ensure(conn: &Connection) -> rusqlite::Result<()> {
    // Step 1: run the baseline DDL (creates tables if they don't exist).
    // The existing DDL contains `INSERT OR IGNORE INTO schema_version VALUES (1)`.
    // This runs on every `ensure()` call. After migration, `schema_version`
    // holds row 1 (inserted here) AND row 2 (inserted by the migration below).
    // Reading bare `SELECT version FROM schema_version` would return an
    // indeterminate row. The safe read pattern is `SELECT MAX(version)`.
    conn.execute_batch(/* ... existing DDL ... */)?;

    // Step 2: apply incremental migrations by version.
    // Use MAX(version) so the result is stable even when the bootstrap
    // INSERT OR IGNORE re-inserts row 1 after a previous migration.
    // `unwrap_or(0)` handles a genuinely empty table (fresh DB before
    // the INSERT OR IGNORE has run, which cannot happen in practice after
    // Step 1, but is defensive).
    let version: i64 = conn
        .query_row(
            "SELECT MAX(version) FROM schema_version",
            [],
            |r| r.get::<_, Option<i64>>(0),
        )
        .unwrap_or(None)
        .unwrap_or(0);

    if version < 2 {
        // SQLite ALTER TABLE ADD COLUMN is not idempotent; the `version < 2`
        // gate ensures this block runs exactly once per database.
        conn.execute_batch(
            r#"
            ALTER TABLE sandbox_actions ADD COLUMN cost_usd      REAL;
            ALTER TABLE sandbox_actions ADD COLUMN round_trip_ms INTEGER;
            INSERT OR REPLACE INTO schema_version VALUES (2);
            "#
        )?;
    }

    Ok(())
}
```

**Why `INSERT OR REPLACE INTO schema_version VALUES (2)` instead of `UPDATE`.** The
`schema_version` table has `version INTEGER PRIMARY KEY`. `INSERT OR REPLACE`
upserts: if a row with `version = 2` already exists (re-entrant call or defensive
guard fires), it is a no-op; otherwise it inserts a new row. The older row `1` is
left in place — that is fine because the migration gate uses `MAX(version)`, not
the presence of a specific row. `UPDATE schema_version SET version = 2` would
convert **all existing rows** to version 2, which causes a `UNIQUE constraint failed`
PRIMARY KEY conflict when (a) a later migration adds another row first and (b) the
`UPDATE` tries to collapse multiple rows to the same version value. `INSERT OR REPLACE`
avoids this by accumulating one row per version level.

**Idempotency under repeated `ensure()` calls:**
- First run (fresh DB): DDL creates tables + inserts row `1`. `MAX(version) = 1`.
  Migration fires: inserts row `2`. State: rows `{1, 2}`.
- Second run: DDL runs `INSERT OR IGNORE INTO schema_version VALUES (1)` — row `1`
  already exists, ignored. `MAX(version) = 2`. Migration gate (`< 2`) is false.
  No `ALTER TABLE` runs. Safe.
- N-th run: same as second run. ✓

`LocalProcessProvider` and `MicroVmProvider` emit `NULL` for both new
columns; no behavioral change for existing rows.

**v1 telemetry scope: per-tool rows only.** Synthetic session-level rows
(`<session_open>`, `<session_cost>`) are **cut from v1**. They require the
provider to receive a `SessionManager` handle in `cleanup()`, which the current
`SandboxProvider` trait does not support (`cleanup()` returns
`Result<(), SandboxError>`). The per-tool `cost_usd` field provides
per-call cost attribution.

**Setup cost is charged to the first tool row.** The lazy session open (sandbox
create, worker upload, `chmod`, SmartSync file upload) happens inside the first
`execute_tool()` call, before the one-shot worker command is issued. The
`round_trip_ms` and `cost_usd` fields on the first `SandboxAction` row are
measured from the **start of lazy session open** (i.e. the very beginning of
the first `execute_tool()` call, including `POST /sandboxes`, worker upload,
`chmod`, and SmartSync) through to the finished worker-command poll. This means:
- The first tool call's `round_trip_ms` includes setup overhead (typically 5–15 s
  for cold start + 7 MB upload); subsequent tool calls only include the command
  round-trip (typically 100 ms–15 s depending on the tool).
- The `cost_usd` estimate for the first tool call is correspondingly larger.
- The sum of `cost_usd` across all tool rows for a session accurately accounts
  for all **tracked** compute time (sandbox-open through final command) on
  **successful provider executions**, excluding storage. Provider-level failure
  paths (e.g. a session-open error after `POST /sandboxes` succeeds but upload
  fails) are not cost-attributed in v1 because `execute_tool` returns `Err(...)`,
  not a `SandboxExecution` with metadata.

Total tracked compute cost on successful provider executions (excluding storage
and failure-path overhead) is derivable by summing `cost_usd` across all rows
with a given `session_file` in `sandbox_actions`. Actual vendor cost is higher
by (a) storage charges ($0.000225/s × sandbox lifetime) and (b) any provider
errors during setup or execution that consumed compute time without emitting a row.

**`pi --stats remote-cost` verb.** Cut from v1 for the same reason.
Adding `StatsVerb::RemoteCost` requires the session-cost rows that v1 does
not emit. Future work once per-session accounting is wired.

### Session lifecycle and cleanup

#### The problem

There is currently **no runtime call site** for
`SandboxProvider::cleanup()` — `grep -n "cleanup" crates/pi-agent-core/src/runtime.rs`
returns nothing. An `E2bProvider` session that ends without `cleanup()` leaks
the remote sandbox until E2B's own `timeout` field fires.

#### Resolution: explicit cleanup at mode exit

The `SandboxProvider::cleanup()` call must be wired at **mode function exit**,
not after a single `session.prompt()` call. This is because:

- In **print mode** (`crates/pi-coding-agent/src/modes/print.rs`) and
  **JSON mode** (`crates/pi-coding-agent/src/modes/json.rs`), the mode function
  calls `session.prompt()` exactly once and then returns. Cleanup after the
  single `prompt()` call is correct for these modes.
- In **interactive mode** (`crates/pi-coding-agent/src/modes/interactive.rs`),
  the user issues multiple prompts across the session lifetime. Cleanup must
  happen when the interactive loop exits, not after each `prompt()`.
- In **RPC mode** (`crates/pi-coding-agent/src/modes/rpc.rs`), the same
  multi-prompt concern applies.
- The **task executor** (`crates/pi-coding-agent/src/native/task/executor.rs`)
  runs child runtimes. In v1, `task` is unavailable under remote sandboxes
  (see §"Subagents"), so this path is not reached.

**Implementation shape.** Each mode's `run()` function extracts the
`sandbox_provider` from `startup.runtime_config` before the prompt loop, and
calls `cleanup()` just before returning:

```rust
// Pseudocode pattern for each mode's run() function
pub async fn run(startup: Startup) -> anyhow::Result<()> {
    let sandbox_provider = startup.runtime_config.sandbox_provider.clone();
    // ... existing prompt loop ...
    // At mode exit (all modes):
    if let Some(sp) = sandbox_provider {
        if let Err(e) = sp.cleanup().await {
            tracing::warn!(err = %e, "E2B sandbox cleanup failed");
        }
    }
    Ok(())
}
```

The actual implementation in Commit G modifies each of the four mode files:
`print.rs`, `json.rs`, `rpc.rs`, and `interactive.rs`. Each adds the cleanup
call at its natural exit point (after the final `printer.await.ok()` for
print/json, after the event loop for interactive/rpc).

#### Concurrent prompt draining before cleanup (interactive and RPC modes)

**The race.** In interactive mode
(`crates/pi-coding-agent/src/modes/interactive.rs`), prompts are submitted via
`tokio::spawn(async move { let _ = s.prompt(text).await; })`. In RPC mode
(`crates/pi-coding-agent/src/modes/rpc.rs`), the same pattern applies. A naive
mode-exit `sandbox_provider.cleanup()` that fires while a spawned prompt task
is still running can issue `DELETE /sandboxes/{id}` while the worker binary is
mid-execution, causing the tool call to fail with a vendor 404 or connection
error.

**v1 resolution: abort then cleanup.** The interactive and RPC cleanup sites
call `session.abort().await` before `sandbox_provider.cleanup()`.

**What `session.abort()` actually does** (`crates/pi-agent-core/src/runtime.rs:1050`):
it sets `self.inner.lock().await.aborted = true`. This flag is checked at the
**top of each prompt-loop iteration** (`runtime.rs:1181`). Concretely:

- If no `session.prompt(...)` task is running at the time `abort()` is called,
  `cleanup()` can proceed immediately; no race.
- If a `tokio::spawn`'d prompt task is currently blocked inside a tool call
  (e.g. waiting for the E2B poll response), `abort()` does NOT interrupt that
  in-flight tool call. The task will complete its current tool call, then check
  the abort flag at the next loop boundary and exit. During the window between
  `abort()` returning and the spawned task finishing its last tool call,
  `cleanup()` may issue `DELETE /sandboxes/{id}` while the tool call is still
  waiting for an E2B poll response. The E2B command-poll request will get a
  404/410 response, which the `E2bProvider` maps to
  `SandboxError::Provider(...)`. Since the session is already being torn down,
  this error is logged as a warning and discarded — the agent won't act on the
  result.

The `E2B_SANDBOX_TIMEOUT_SECS` backstop (below) makes leaked sandboxes from
`kill -9` or other non-graceful exits safe.

```rust
// Pattern for interactive.rs and rpc.rs at loop exit:
session.abort().await;
if let Some(sp) = sandbox_provider {
    if let Err(e) = sp.cleanup().await {
        tracing::warn!(err = %e, "E2B sandbox cleanup failed (in-flight tool call may have raced)");
    }
}
```

This is a **best-effort cleanup**: it closes the window for normal exits while
accepting a small race window for quit-during-tool-call. The alternative
(tracking `JoinHandle`s for spawned prompt tasks and joining them before cleanup)
would require invasive changes to the mode event loops and is v2 scope.

Print and JSON modes call `session.prompt()` directly and `await` it (no
`tokio::spawn`), so no concurrent draining is needed; they call `cleanup()`
after `printer.await.ok()`.

**`E2B_SANDBOX_TIMEOUT_SECS` backstop.** Even without an explicit cleanup call
(e.g. `kill -9`), the sandbox terminates when its `timeout` parameter expires
(default 3600 s, override via `E2B_SANDBOX_TIMEOUT_SECS`). This bounds the
cost of leaked sandboxes.

**`E2B_MAX_COST_USD`.** Cut from v1: cost-cap enforcement requires cost polling
after each tool call, which requires the provider to track elapsed time and
compute running cost estimates. This is second-pass material.

### Subagents, tool coverage, and host-bound tools

#### v1 position: `task` is unavailable under remote sandboxes

The `task` tool spawns a child runtime on the host
(`crates/pi-coding-agent/src/native/task/executor.rs:255–256`). Under a remote
sandbox, the child runtime would inherit the same `Arc<dyn SandboxProvider>`
as the parent. This creates a lifecycle problem:

- The Arc sharing is correct for the *local* microVM case where `Arc::clone()`
  is a refcount bump on an in-process state machine.
- For E2B, the same Arc clone means child tasks share the same remote sandbox
  — but the parent sandbox might be cleaned up while a child is still running
  (or vice versa), since child runtimes run concurrently and their lifetimes
  are not ordered relative to the parent.
- Cleanly solving this (e.g. a rendezvous Arc-count-zero → cleanup protocol)
  is out of scope for v1.

**v1 resolution: `task` and `todo` are rejected by provider-side precheck in
`E2bProvider::execute_tool`.** This is the same pattern used for `web_search`,
`ask`, and the autoresearch tools (see §"Tool set for remote v1").

**Why not global `ToolDispatch::Unavailable` overrides in `task/tool.rs` and
`todo/tool.rs`?** `ToolDispatch` is tool-global — it has no provider parameter.
`MicroVmProvider` uses the trait-default `honors_tool_dispatch() -> true`
(it does not override the method; `crates/pi-sandbox/src/provider.rs:94` shows
the default). A global `dispatch() -> Unavailable` override on `TaskTool` or
`TodoTool` would therefore also mark them unavailable under `MicroVmProvider`
and any other future sandbox provider, cutting across the RFD 0005 subagent
story. The correct scope for this restriction is **E2B-specific**, not
tool-global.

The E2B implementation commit adds `"task"` and `"todo"` to the list of
tool names checked in `E2bProvider::execute_tool` before routing to the
guest worker:

```rust
// crates/pi-sandbox/src/remote/e2b.rs — inside execute_tool()
const PROVIDER_SIDE_UNAVAILABLE: &[&str] = &[
    "web_search", "ask",
    "init_experiment", "run_experiment", "log_experiment",
    "task", "todo",
];
if PROVIDER_SIDE_UNAVAILABLE.contains(&tool_name) {
    return Err(SandboxError::Provider(format!(
        "`{}` is not available in the E2B remote sandbox", tool_name
    )));
}
```

This returns a clean `SandboxError::Provider(...)` — surfaced to the agent as
an informative error — without any changes to `task/tool.rs` or `todo/tool.rs`.
The rejection is E2B-specific; other sandbox providers are unaffected.

Note: `task` and `todo` are registered in the *host* runtime's tool registry,
not in the worker binary's registry. The worker uses
`ToolRegistry::with_unsafe_extras()` (`crates/pi-tools-core/src/lib.rs:117`)
which does not include them. The provider-side precheck is the first line of
defense; if it were bypassed, the worker would also fail with "unknown tool".
The precheck makes the failure fast and readable at the provider level.

#### Tool set for remote v1

The worker uses `ToolRegistry::with_unsafe_extras()`
(`crates/pi-sandbox-worker/src/dispatch.rs` → `crates/pi-tools-core/src/lib.rs:117`),
which includes: `read`, `write`, `edit`, `bash`, `grep`, `find`, `ls`. These
are the v1 guest tools.

**`web_search` is unavailable in remote v1.** The current dispatch path for
`web_search` in the worker (`crates/pi-sandbox-worker/src/dispatch.rs:390–391`)
proxies the call out to the host via **vsock** (vsock CID + port, Linux-only).
Remote E2B sandboxes have no vsock channel to the host — the transport is
one-shot HTTP commands, not a persistent vsock connection. The vsock proxy path
would fail with a connection error at runtime.

**Fix:** the E2B implementation commit adds a guard in `dispatch_request`
(`crates/pi-sandbox-worker/src/dispatch.rs`) that, when `--transport stdin`,
short-circuits `web_search` with an explicit error response:

```rust
// crates/pi-sandbox-worker/src/dispatch.rs — amendment
if req.tool_name == "web_search" && IS_STDIN_TRANSPORT {
    return ToolResponse {
        call_id: req.call_id,
        stdout: "web_search is not available in remote sandbox mode".to_string(),
        stderr: String::new(),
        exit_status: 1,
        guest_duration_ms: 0,
        is_error: true,
        file_writes: vec![],
    };
}
```

where `IS_STDIN_TRANSPORT` is a **module-level `static AtomicBool`** (not
thread-local; the flag is process-wide) set once at worker startup from the
`--transport` flag before any tool dispatch occurs:

```rust
// crates/pi-sandbox-worker/src/dispatch.rs — amendment
static IS_STDIN_TRANSPORT: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);

/// Called once from main() before the first dispatch.
pub fn set_stdin_transport(v: bool) {
    IS_STDIN_TRANSPORT.store(v, std::sync::atomic::Ordering::Relaxed);
}
```

This produces a clean "not available" error rather than a cryptic vsock
connection failure.

**Important:** `web_search`, `ask`, `init_experiment`, `run_experiment`,
`log_experiment`, `task`, and `todo` do **not** get global
`ToolDispatch::Unavailable` overrides on their respective tool structs — that
would affect all modes, not just the remote sandbox path. (`ToolDispatch` has no
provider parameter; a global override would also disable tools like `task` under
`MicroVmProvider`, since `MicroVmProvider` inherits the trait-default
`honors_tool_dispatch() -> true`.) For all these tools, the rejection is
**provider-specific**: `E2bProvider::execute_tool` checks for the tool name
before dispatching and returns
`SandboxError::Provider("<tool> not available in E2B remote sandbox")`.
The worker-side `IS_STDIN_TRANSPORT` guard for `web_search` is defense-in-depth
only (the provider precheck fires first).

**Host-bound tools that are NOT routed to the remote worker:**

| Tool | Reason | Dispatch |
|------|--------|----------|
| `lsp` | Requires host-side language server processes | `ToolDispatch::Unavailable` (already in `crates/pi-coding-agent/src/native/lsp/tool.rs:101–115`) |
| `monitor` | Requires streaming protocol incompatible with sandbox shape | `ToolDispatch::Unavailable` (already in `crates/pi-tools-core/src/monitor.rs:203–210`) |
| `task` | Spawns a child runtime on the host; subagent lifecycle unsafe in v1 | Provider-side precheck in `E2bProvider::execute_tool` — **new** in Commit G. No tool-level `dispatch()` override (would affect microvm and other providers). |
| `todo` | Writes `<cwd>/.pi/todo.json` on the host filesystem | Provider-side precheck in `E2bProvider::execute_tool` — **new** in Commit G. No tool-level `dispatch()` override. |
| `web_search` | vsock-proxied in worker; vsock unavailable in E2B | Provider-side precheck in `E2bProvider::execute_tool` + worker-side stdin-transport guard (defense-in-depth). **Not** a global `ToolDispatch` override (would break microvm path). |
| `ask` | Interactive TUI; host-side prompt I/O | Provider-side precheck in `E2bProvider::execute_tool` — **new** in Commit G. No tool-level `dispatch()` override (would affect non-remote modes). |
| `init_experiment`, `run_experiment`, `log_experiment` | Autoresearch: write to host repo and `autoresearch.jsonl` | Provider-side precheck in `E2bProvider::execute_tool` — **new** in Commit G. No tool-level `dispatch()` override. |

**Effective remote v1 tool set:** `read`, `write`, `edit`, `bash`, `grep`,
`find`, `ls`. All other tools return a clean "unavailable in remote sandbox"
error before reaching the worker.

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

Files larger than 100 MB are excluded by the hard size cap and not uploaded
(v1: no override for individual large files; this is a v1 limitation).
The `--sandbox-upload-include` / `--sandbox-upload-exclude` override flags
are deferred to post-v1 (see §"Open questions" #13).

**Upload mechanism.** `POST /sandboxes/{id}/files` accepts a single file path +
body per request. For a session open with a typical project (~50 files / ~2 MB
after exclusions), the host walks the directory, filters per the exclusion list,
and fires concurrent file-upload requests (concurrency limit: 8, configurable
via `E2B_UPLOAD_CONCURRENCY`). Total session-open overhead target: ≤ 3 s for
a ≤ 5 MB upload.

**v1 upload mode: SmartSync only.** `GitClone` and `Empty` modes are **cut
from v1** and deferred to post-v1 (see §"Open questions" #13). In v1, the
only upload mode is `SmartSync`. The internal `UploadMode` enum will have
one variant in v1:

```rust
pub enum UploadMode {
    SmartSync,
    // GitClone and Empty: deferred post-v1
}
```

**CLI flags:** `--sandbox-upload-include=<glob>` and
`--sandbox-upload-exclude=<glob>` are also **deferred from v1** to avoid
scope creep. In v1, `E2B_UPLOAD_CONCURRENCY` and the fixed built-in
exclusion list are the only customization surface. The flags are documented
here as the intended post-v1 extension points so the implementation campaign
doesn't wire them unnecessarily.

> **Why cut:** the `GitClone` mode requires running a `git clone` command in
> the sandbox (adding a command round-trip at session open), and `Empty`
> requires a CLI enum, startup parsing, and flag-plumbing across four modes
> with no clear v1 use case. SmartSync covers the primary scenario (local
> project work). `GitClone` and `Empty` can be added as a < 100 LoC
> follow-up once the base provider is stable.

**Upload mode summary:**

| Mode | v1 status | Behaviour |
|------|-----------|-----------|
| `SmartSync` | **v1** | Upload filtered cwd at session open; flushback `write`/`edit` mutations via `ToolResponse.file_writes` |
| `GitClone { url, rev }` | **post-v1** | `git clone <url>` inside the sandbox; no host-cwd upload |
| `Empty` | **post-v1** | No upload; `/work` starts empty |

### Wire example: acquire → execute_tool("bash") → execute_tool("write") → cleanup

```
// 1. Session open
POST https://api.e2b.dev/sandboxes
  {"templateID": "base", "timeout": 3600, "metadata": {"pi_session": "<uuid>"}}
→ {"sandboxID": "abc123", ...}

// 2. Worker upload
POST https://api.e2b.dev/sandboxes/abc123/files?path=/usr/local/bin/pi-sandbox-worker
  Content-Type: application/octet-stream
  Body: <7 MB binary>

// 3. chmod the worker (sync command, poll to completion at 100ms/200ms intervals)
POST https://api.e2b.dev/sandboxes/abc123/commands
  {"cmd": ["chmod", "+x", "/usr/local/bin/pi-sandbox-worker"]}
→ {"cmdID": "cmd_0", ...}
// sleep 100ms, then:
GET  https://api.e2b.dev/sandboxes/abc123/commands/cmd_0
→ {"cmdID": "cmd_0", "finished": true, "exitCode": 0, "stdout": "", "stderr": ""}

// 4. File upload (SmartSync, concurrent, cwd = ~/myproject)
POST .../files?path=/work/src/main.rs   ← file body
POST .../files?path=/work/Cargo.toml    ← file body
... (8-way concurrent)

// 5. execute_tool("bash", {"command": "cargo test --lib 2>&1 | tail -5"})
  Host dispatches:
POST https://api.e2b.dev/sandboxes/abc123/commands
  {"cmd": ["/usr/local/bin/pi-sandbox-worker", "--transport", "stdin",
           "--work-dir", "/work", "--log-level", "warn"],
   "stdin": "{\"proto_version\":2,\"call_id\":\"c1\",\"tool_name\":\"bash\",
              \"tool_input\":{\"command\":\"cargo test --lib 2>&1 | tail -5\"},
              \"max_output_bytes\":262144,\"timeout_ms\":60000}\n"}
→ {"cmdID": "cmd_1", ...}

// sleep 100ms, then poll (may need multiple polls for long-running cargo test):
GET https://api.e2b.dev/sandboxes/abc123/commands/cmd_1
→ {"cmdID": "cmd_1", "finished": false, "exitCode": null, "stdout": "", "stderr": ""}
// sleep 200ms, then poll again:
GET https://api.e2b.dev/sandboxes/abc123/commands/cmd_1
→ {"cmdID": "cmd_1", "finished": true, "exitCode": 0,
   "stdout": "{\"call_id\":\"c1\",\"stdout\":\"test result: ok. 3 passed; 0 failed\",
               \"stderr\":\"\",\"exit_status\":0,\"guest_duration_ms\":4100,
               \"is_error\":false,\"file_writes\":[]}\n",
   "stderr": "...worker tracing logs..."}
  // Host parses ToolResponse from stdout field

// 6. execute_tool("write", {"path": "/work/src/new_module.rs", "content": "..."})
POST https://api.e2b.dev/sandboxes/abc123/commands
  {"cmd": ["/usr/local/bin/pi-sandbox-worker", "--transport", "stdin",
           "--work-dir", "/work", "--log-level", "warn"],
   "stdin": "{\"proto_version\":2,\"call_id\":\"c2\",\"tool_name\":\"write\",
              \"tool_input\":{\"path\":\"/work/src/new_module.rs\",\"content\":\"fn foo() {}\"},
              \"max_output_bytes\":4096,\"timeout_ms\":10000}\n"}
→ {"cmdID": "cmd_2", ...}

// sleep 100ms, then poll:
GET https://api.e2b.dev/sandboxes/abc123/commands/cmd_2
→ {"cmdID": "cmd_2", "finished": true, "exitCode": 0,
   "stdout": "{\"call_id\":\"c2\",\"stdout\":\"Written 12 bytes to src/new_module.rs\",
               \"stderr\":\"\",\"exit_status\":0,\"guest_duration_ms\":8,
               \"is_error\":false,
               \"file_writes\":[{\"path\":\"src/new_module.rs\",
                                 \"contents_b64\":\"Zm4gZm9vKCkge30=\",\"mode\":420}]}\n"}

// 7. Host applies file_writes: writes "fn foo() {}" to
//    ~/myproject/src/new_module.rs (atomic temp-write + rename)

// 8. cleanup (called at mode exit after the session completes)
DELETE https://api.e2b.dev/sandboxes/abc123
→ 200 OK
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
- Each pi session maps to one E2B sandbox (one-to-one at the root session level).
  `task` is unavailable in v1, so there is exactly one active session per sandbox.
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
`POST /sandboxes/{id}/commands` returns a `cmdID`. Pi-rs polls
`GET /sandboxes/{id}/commands/{cmdID}` and checks `exitCode`. If the worker
binary exits non-zero (e.g. because it can't exec), the exit code is non-zero
and `stdout` will not contain a valid `ToolResponse` JSON line. The host detects
the missing/malformed response and returns `SandboxError::Provider("worker
exited with code N; no valid ToolResponse in stdout")`.

**Adversarial example: vendor API returns 200 but file write silently dropped.**
The `file_writes` flushback is verified on the host: after applying each write,
the host reads back the written file and compares its length against
`len(base64_decode(contents_b64))`. A **verification mismatch is treated as a
flushback failure** — identical recovery to a host-side apply failure: the
provider sets `poisoned`, issues a best-effort `DELETE /sandboxes/{id}`, and
returns `SandboxError::Provider("E2B session desynced after flushback failure on
'<path>' (verification mismatch). Restart the pi session to recover.")` for all
subsequent calls. The rationale is the same as for apply failures: the remote
`/work` and the host cwd have diverged (the guest wrote the file, the host did
not confirm receipt), so continuing the session would silently corrupt the agent's
view of the host filesystem. The session must be restarted cleanly. This is
best-effort defense; the host cannot verify guest-internal execution.

### Auth and key management

E2B requires a single API key. Key resolution order at `E2bProvider::from_auth()`:

1. **`E2B_API_KEY` env var** — read at startup. This is the v1 UX for CLI users.
   Consistent with how LLM provider keys are read (all providers read keys from
   env vars via `AuthStorage::from_env_explicit`,
   `crates/pi-ai/src/auth.rs:144`).
2. **`AuthStorage` lookup** — key `"e2b"` in the shared `AuthStorage` instance
   passed at construction (`AuthStorage` is at `crates/pi-ai/src/auth.rs:41`).
   This is the recommended path for SDK embedders that manage credentials
   programmatically.
3. **Fail** with `SandboxError::Unavailable("E2B API key not configured; set
   E2B_API_KEY env var")` if neither source has a key.

**v1: env-var only for the `pi` CLI.** There is currently no `pi auth set`
subcommand in the CLI (`crates/pi-coding-agent/src/cli.rs` has no `auth`
subcommand; adding one is out of scope for this RFD). CLI users set
`E2B_API_KEY` in their shell environment. SDK embedders use `AuthStorage`.

The key is never written to disk by pi-rs (it arrives via env or `AuthStorage`).
It is never included in uploaded files, command env, or any telemetry row.

The E2B implementation commit adds `("e2b", "E2B_API_KEY")` to
`AuthStorage::ENV_KEYS` (`crates/pi-ai/src/auth.rs:96`) so that
`AuthStorage::from_env_explicit(AuthStorage::ENV_KEYS.iter().copied())` picks
it up automatically when building an `AuthStorage` from env (the path used in
`crates/pi-coding-agent/src/startup.rs:107`). This is how all other providers
are handled; `E2B_API_KEY` follows the same pattern.

### Cost telemetry

**E2B billing signal.** E2B does not expose a live accumulated `cost_usd` field
on the sandbox resource in the REST API (the pricing is compute_rate × duration,
billed monthly). Pi-rs computes per-call cost estimates using the published rate:

```
cost_usd = E2B_COMPUTE_RATE_PER_SEC × (round_trip_ms / 1000.0)
```

where `E2B_COMPUTE_RATE_PER_SEC` defaults to `0.000084` (E2B's published compute
rate as of 2026-05; overridable via `E2B_COMPUTE_RATE_PER_SEC` env var for pricing
changes). The `round_trip_ms` is measured host-side:
- For the **first tool call**: from the start of lazy session open (the very first
  network request, i.e. `POST /sandboxes`) through to the finished worker-command
  poll. This includes sandbox create, worker upload, `chmod`, SmartSync, and the
  tool command itself. Setup cost is therefore folded into the first row.
- For **subsequent tool calls**: from command submit to finished response (no
  setup overhead).

The estimate is attached to each tool call's `SandboxExecution.cost_usd`.

**Storage cost not tracked in v1.** E2B also charges $0.000225/s for sandbox
storage (the running sandbox filesystem). This is a per-session fixed cost
(sandbox lifetime × storage rate) that pi-rs does not attribute to individual
tool calls. The per-tool `cost_usd` field underestimates true session cost.
Summing per-tool `cost_usd` yields tracked compute cost on successful provider
executions; actual vendor cost is higher because it also includes storage
charges (`sandbox_lifetime_secs × 0.000225`) and any failure-path compute
overhead that emitted no row. The storage rate is not currently tracked in
`sandbox_actions` rows.
Tracking it as a session-level row is future work (see §"Open questions" #4).

These per-call estimates accumulate in `sandbox_actions` rows and are queryable
directly with:

```sql
SELECT SUM(cost_usd) FROM sandbox_actions
WHERE session_file = ? AND provider = 'e2b';
```

**No synthetic session-level rows in v1.** `<session_open>` and `<session_cost>`
rows are not emitted (see §"Provider/runtime contract amendments"). Total
tracked compute cost on successful provider executions is derivable by summing
per-tool `cost_usd` rows; actual vendor cost is higher because storage charges
and failure-path overhead (lazy-open failures, flushback desync aborts, etc.)
are not captured in v1.

### Failure modes and `SandboxError` mapping

The existing `SandboxError` enum (`crates/pi-sandbox/src/provider.rs:26–45`) is
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
| Command exitCode non-zero, no valid ToolResponse | `Provider("worker exited with code N; no valid ToolResponse")` |
| File upload failure (session open) | `Provider("upload failed: <path>: <http-status>")` — sandbox is best-effort deleted and session state cleared; next `execute_tool()` retries from scratch |
| Worker upload / `chmod` / SmartSync failure (partial session open) | `Provider("session open failed: <step>: <err>")` — same as file upload failure: best-effort DELETE, clear sandbox ID, retry-from-scratch on next call; does NOT poison the session |
| Host flushback apply failure (after successful remote write) | Session poisoned; `Provider("E2B session desynced after flushback failure on '<path>': <err>. Restart the pi session to recover.")` — subsequent calls also fail immediately |
| Host flushback verification mismatch (length check fails after apply) | Session poisoned; same as apply failure: `Provider("E2B session desynced after flushback failure on '<path>' (verification mismatch). Restart the pi session to recover.")` — subsequent calls also fail immediately |

**Retry policy.** `RateLimited` triggers exponential backoff in the
`execute_tool` call path: 1 s, 2 s, 4 s, fail after 3 retries. All other error
variants fail immediately (no retry). Session open (`open()`) does not retry on
any error — the caller (the agent startup path) handles retry at a higher level
if desired.

**`PI_SANDBOX_OFFLINE=1`** (already used by the local path per
`crates/pi-sandbox-rootfs/README.md:107`) causes the provider to refuse all
remote activity. Because `with_key(key) -> Self` is infallible (the constructor
never returns an error; see §"Public API of `E2bProvider`"), `PI_SANDBOX_OFFLINE`
cannot be enforced at `with_key()` construction time. Instead, it is checked at
the top of the **first `execute_tool()` call** alongside the worker-path
resolution. If `PI_SANDBOX_OFFLINE=1` is set, `execute_tool` returns immediately
with `SandboxError::Unavailable("remote sandbox unavailable: PI_SANDBOX_OFFLINE=1")`
before any network activity. `from_auth()`, which is fallible
(`Result<Self, SandboxError>`), may also check `PI_SANDBOX_OFFLINE` eagerly at
construction — failing fast is desirable for that constructor since the API key
resolution already involves a fallible path. The net UX is identical for CLI
users (both constructors surface the error before any sandbox work starts), while
`with_key()` stays unconditionally infallible so test code can construct
`E2bProvider` without environment guards.

### contextfs: not applicable for remote

The contextfs glue (`crates/pi-sandbox/src/contextfs.rs`) and the contextfs
vsock proxy (`crates/pi-sandbox/src/microvm/contextfs_proxy.rs`) bridge UDS ↔
vsock for the local microVM case. Remote vendors do not support vsock from the
host side, and the E2B HTTP command transport replaces vsock entirely.
The `file_writes` field on `ToolResponse` (proto v2) covers the v1 file mutation
flushback case (`write` and `edit` tools only) without contextfs. Contextfs is
simply not wired into the remote path.

## Implementation plan

### Commit G (E2B reference implementation)

**Changed files (future files marked †):**

| File | Change | Est. LoC |
|------|--------|---------|
| `crates/pi-sandbox-protocol/src/lib.rs` | Add `FileWrite` struct; add `file_writes` field to `ToolResponse`; bump `CURRENT_PROTOCOL_VERSION` to 2 | +20 |
| `crates/pi-sandbox-worker/src/main.rs` | Add `--transport` flag; dispatch to `serve_stdio`; set `IS_STDIN_TRANSPORT` | +30 |
| `crates/pi-sandbox-worker/src/listener.rs` | Add `serve_stdio` one-shot helper | +30 |
| `crates/pi-sandbox-worker/src/dispatch.rs` | After `write`/`edit` tool returns, read output file (≤ 32 KiB) and populate `file_writes`; `bash` and others emit `[]`; `web_search` returns unavailable when `IS_STDIN_TRANSPORT` | +50 |
| `crates/pi-sandbox/src/lib.rs` | Add `pub mod remote;` and `pub use remote::e2b::E2bProvider;` | +3 |
| `crates/pi-sandbox/src/remote/mod.rs` † | `pub mod e2b; pub mod upload;` | +5 |
| `crates/pi-sandbox/src/remote/e2b.rs` † | `E2bProvider` + session impl, HTTP command dispatch, cost estimate, cleanup; **includes provider-side prechecks** that reject `web_search`, `ask`, `init_experiment`, `run_experiment`, `log_experiment`, `task`, `todo` before routing to `execute_tool` (returns `SandboxError::Provider("<tool> not available in E2B remote sandbox")`). No changes to `ask/tool.rs`, `task/tool.rs`, `todo/tool.rs`, or `autoresearch/tools.rs` — all rejections are in `E2bProvider::execute_tool`, not at the tool-dispatch level. A global `dispatch()` override would incorrectly affect `MicroVmProvider` and other providers. **Also implements:** (a) first-call `round_trip_ms` measurement starts at the top of the first `execute_tool()` call (before `POST /sandboxes`), so setup cost is folded into the first tool row; (b) `poisoned` flag — on any host-side flushback failure, sets poisoned, issues best-effort `DELETE /sandboxes/{id}`, and returns `SandboxError::Provider("E2B session desynced...")` for all subsequent calls. | +400 |
| `crates/pi-sandbox/src/remote/upload.rs` † | `SmartSync` + exclusion logic (v1 only; `GitClone`/`Empty` deferred) | +120 |
| `crates/pi-sandbox/src/provider.rs` | Add `RateLimited` + `BillingError` variants; add `round_trip_ms` + `cost_usd` to `SandboxExecution` | +15 |
| `crates/pi-agent-core/src/session.rs` | Add `cost_usd` + `round_trip_ms` to `SandboxAction` | +5 |
| `crates/pi-agent-core/src/runtime.rs` | Change `invoke_via_sandbox` return type to `Result<(ToolResult, SandboxExecution), String>`; update call sites at lines 1699–1704 to destructure tuple and populate `cost_usd`/`round_trip_ms` in `SandboxAction` emit | +20 |
| `crates/pi-stats/src/schema.rs` | Bump `CURRENT_VERSION` to 2; add version-gated `ALTER TABLE` migration for `cost_usd` + `round_trip_ms` | +15 |
| `crates/pi-stats/src/ingest.rs` | Thread nullable fields through `insert_sandbox_action` | +15 |
| `crates/pi-ai/src/auth.rs` | Add `("e2b", "E2B_API_KEY")` to `ENV_KEYS` | +1 |
| `crates/pi-coding-agent/src/startup.rs` | Add `"e2b"` arm to `install_sandbox_from_flag`; update error message | +15 |
| `crates/pi-coding-agent/src/modes/print.rs` | Add cleanup call after `printer.await.ok()` at mode exit | +6 |
| `crates/pi-coding-agent/src/modes/json.rs` | Add cleanup call after `printer.await.ok()` at mode exit | +6 |
| `crates/pi-coding-agent/src/modes/rpc.rs` | Add `session.abort().await` then cleanup at loop exit | +8 |
| `crates/pi-coding-agent/src/modes/interactive.rs` | Add `session.abort().await` then cleanup at loop exit | +8 |
| `crates/pi-coding-agent/src/native/task/tool.rs` | No change needed — rejection is provider-side in `e2b.rs` | 0 |
| `crates/pi-coding-agent/src/native/todo/tool.rs` | No change needed — rejection is provider-side in `e2b.rs` | 0 |
| `crates/pi-coding-agent/src/native/lsp/` (already has Unavailable) | No change needed | 0 |
| `crates/pi-sandbox/tests/e2b_mock.rs` † | axum mock HTTP server + lifecycle tests | +140 |

**Total:** ~933 LoC (source + tests).

**`crates/pi-sandbox/src/lib.rs` amendment (public root-module export):**

The existing `lib.rs` (`crates/pi-sandbox/src/lib.rs:9–22`) exposes `cache`,
`contextfs`, `local`, `microvm`, and `provider` — but not `remote`. Without
adding `pub mod remote;` here, no other crate (including
`crates/pi-coding-agent/src/startup.rs`) can name `E2bProvider`, making
Commit G unimplementable. The following two lines are added in Commit G:

```rust
// crates/pi-sandbox/src/lib.rs — amendment
pub mod remote;                            // new module declaration
pub use remote::e2b::E2bProvider;         // re-export for crate consumers
```

The `remote` module is always compiled (no `#[cfg]` gate). `E2bProvider`
construction is gated at runtime by API-key presence, not at compile time.

**Public API of `E2bProvider`:**
```rust
// crates/pi-sandbox/src/remote/e2b.rs  (future file †)
pub struct E2bProvider {
    api_key: String,         // resolved at construction
    base_url: String,        // default https://api.e2b.dev; override E2B_BASE_URL
    upload_mode: UploadMode,
    sandbox_timeout_secs: u32,
    // Note: worker_bin_path is NOT resolved at construction.
    // It is resolved lazily on the first execute_tool() call.
    // This keeps both constructors infallible/cheap and allows
    // tests to construct E2bProvider without PI_SANDBOX_WORKER_BIN set.
    // Timing: start_of_session_open tracks the Instant at which the first
    // execute_tool() began, so the first tool row's round_trip_ms covers
    // setup (POST /sandboxes + upload + SmartSync) + the tool command.
    // poison: once a host flushback fails, all subsequent calls are rejected.
    // (Fields are conceptual; exact names are left to the implementer.)
}

impl E2bProvider {
    /// Resolve API key from E2B_API_KEY env var or AuthStorage.
    /// Fails loudly if neither source has a key.
    /// Also fails if PI_SANDBOX_OFFLINE=1 (eager fail-fast for CLI users).
    /// Worker binary path is NOT resolved here — resolved lazily on first
    /// execute_tool() call.
    pub fn from_auth(auth: &AuthStorage) -> Result<Self, SandboxError>;

    /// Explicit key (for tests and SDK embedders that manage creds themselves).
    /// Unconditionally infallible: does not check PI_SANDBOX_OFFLINE or
    /// PI_SANDBOX_WORKER_BIN — both are checked lazily on the first
    /// execute_tool() call. This allows tests to construct E2bProvider
    /// without any environment state.
    pub fn with_key(key: String) -> Self;
}

// Implements SandboxProvider; session state (sandboxID) held in
// a Mutex<Option<String>> initialized on first execute_tool call.
// First execute_tool() performs, in order:
//   1. Check PI_SANDBOX_OFFLINE=1 → SandboxError::Unavailable if set.
//   2. Check PI_SANDBOX_WORKER_BIN → SandboxError::Unavailable if absent/not executable.
//   3. Lazy session open: POST /sandboxes, upload worker, SmartSync /work.
// Construction via with_key() is always cheap and environment-agnostic.
```

**`UploadMode` enum (v1 — SmartSync only):**
```rust
pub enum UploadMode {
    SmartSync,
    // GitClone and Empty: deferred to post-v1 (see §"Open questions" #13)
}
```

**CLI flag:** `--sandbox-provider=e2b` (not `remote:e2b`; the `startup.rs`
match arm is `"e2b"`). This is consistent with the existing pattern: `"local-process"`,
`"microvm:firecracker"` are the current values, and `"e2b"` slots in alongside
them as a flat string.

**env vars that gate behavior:**
- `E2B_API_KEY` — API key (v1 CLI UX; also picked up by `AuthStorage::from_env_explicit`).
- `E2B_BASE_URL` — override API endpoint (for tests against a mock server).
- `E2B_SANDBOX_TIMEOUT_SECS` — sandbox lifetime cap (default 3600).
- `E2B_COMPUTE_RATE_PER_SEC` — override published compute rate (default 0.000084).
- `E2B_UPLOAD_CONCURRENCY` — file upload parallelism (default 8).
- `PI_SANDBOX_WORKER_BIN` — path to `pi-sandbox-worker` binary.
- `PI_SANDBOX_OFFLINE=1` — refuse remote transports.

**No new workspace dependencies.** `reqwest` (already at `Cargo.toml:35`) handles
all HTTP. No WebSocket library is needed; the transport is HTTPS request/response.

**Test strategy:**
- **Live tests** gated on `E2B_API_KEY` env var (present in CI only on the
  self-hosted runner). Tests skip cleanly if the key is absent (mirror the
  `which::which("firecracker")` skip pattern in `crates/pi-sandbox/tests/
  firecracker_smoke.rs`).
- **`E2B_BASE_URL` mock server** — a minimal `axum` mock in
  `crates/pi-sandbox/tests/e2b_mock.rs` † that responds with fixture JSON for
  `POST /sandboxes`, `POST /sandboxes/{id}/files`,
  `POST /sandboxes/{id}/commands`, `GET /sandboxes/{id}/commands/{cmd_id}`,
  `DELETE /sandboxes/{id}`. Covers the upload and session lifecycle without a
  live API key.

  **IMPORTANT: the mock command endpoint must NOT call `dispatch_request()`
  directly.** `dispatch_request()` calls `pre_call_hygiene()` unconditionally
  (`crates/pi-sandbox-worker/src/dispatch.rs:357–370`, `377–384`), which
  recursively deletes entries under `/tmp`, `/var/tmp`, and `/root`. That
  deletion is safe inside a guest VM but is **destructive on the host test
  process**. The mock server must use **canned `ToolResponse` fixtures** that
  are hardcoded in the test:

  ```rust
  // In e2b_mock.rs — mock command handler
  axum::routing::post("/sandboxes/:id/commands", |/* ... */| async {
      // Return a pre-canned ToolResponse for the expected tool name.
      // Do NOT call pi_sandbox_worker::dispatch_request() here.
      let response_json = r#"{"call_id":"c1","stdout":"ok","stderr":"",
          "exit_status":0,"guest_duration_ms":5,"is_error":false,
          "file_writes":[]}"#;
      // Simulate finished=true on first poll by storing cmd fixture in
      // a shared HashMap<cmd_id, response_json>.
      Json(json!({"cmdID": new_cmd_id()}))
  })
  ```

  For tests that need realistic worker output (e.g. to verify `file_writes`
  round-trip), build the `ToolResponse` struct directly in the test and serialize
  it, without invoking any worker dispatch logic.
- **Protocol round-trip** — the existing `crates/pi-sandbox-protocol/tests/
  round_trip.rs` exercises `ToolRequest`/`ToolResponse` framing over a
  `tokio::io::DuplexStream`. After the proto v2 bump this test is updated to
  set `proto_version: 2` and assert `file_writes` round-trips correctly.

### Commit H and I (Sprites, Daytona) — deferred

These land as separate PRs after the E2B reference implementation ships and the
`remote/` module shape is validated. Each will independently pick Option A or B
for the worker strategy after hands-on API validation. The module layout is:

```
crates/pi-sandbox/src/remote/
    sprites.rs   (Commit H — future file †)
    daytona.rs   (Commit I — future file †)
```

Neither gates on the other or on E2B changes.

## CLI wiring

**v1 surface:**

```
pi --sandbox-provider=e2b
```

`--sandbox-provider=e2b` is added to `crates/pi-coding-agent/src/cli.rs`
(the same `sandbox_provider: Option<String>` field) alongside the existing
`"local-process"` and `"microvm:firecracker"` variants. The startup path
`install_sandbox_from_flag` in `crates/pi-coding-agent/src/startup.rs:570`
adds an `"e2b"` arm that calls `E2bProvider::from_auth(&cfg.auth_storage)`.

Note: `install_sandbox_from_flag` receives `cfg: &mut RuntimeConfig`, which
carries `cfg.auth_storage: AuthStorage` (wired at
`crates/pi-coding-agent/src/startup.rs:517` and defined at
`crates/pi-agent-core/src/runtime.rs:264`). There is no separate `auth`
local variable in scope in that function; the correct call is
`E2bProvider::from_auth(&cfg.auth_storage)`.

The error message in the `other => anyhow::bail!(...)` arm at
`crates/pi-coding-agent/src/startup.rs:615` is updated to include `"e2b"` in
the expected-values list.

**Post-v1 extension flags** (not wired in v1):
- `--sandbox-upload=smart-sync|git-clone|empty`
- `--sandbox-upload-include=<glob>`
- `--sandbox-upload-exclude=<glob>`

## Operating notes

> This section is populated as part of the implementation campaign (M3).
> It will document wall-time and per-call cost measurements from the
> first E2B dogfood run, pointer to the smoke test, and any operational
> gotchas discovered during the live integration.

**Integration test:** `crates/pi-sandbox/tests/remote_e2b_smoke.rs` (future
file †, added in M2 of the implementation campaign). Gated on `E2B_API_KEY`
env var; skips cleanly when absent.

**Dogfood script:** `scripts/dogfood-e2b-remote-sandbox.sh` (future file †,
added in M3 of the implementation campaign). Stages a toy Rust source tree,
runs `pi --sandbox-provider=e2b` against it, and verifies file flushback.
Skips cleanly without `E2B_API_KEY`.

**Estimated per-call cost (indicative):** at the published E2B compute rate
of $0.000084/s and a typical `cargo test --lib` round-trip of 5–15 s,
expect $0.0004–$0.0013 per tool call. A 10-call session costs on the order
of $0.01. (Actual measurements will be filled in here post-M3.)

## Open questions (v1 deferred)

1. **`bash` change detection.** Detecting arbitrary filesystem mutations from
   `bash` commands (deletes, renames, symlinks, dir creates) requires an
   inotify/fanotify watcher or before/after tree diff. Deferred to v2.
2. **Large-file out-of-band flushback.** When a file exceeds the 32 KiB
   inline cap, v1 returns an error. A `GET /sandboxes/{id}/files?path=...`
   fallback download path is v2 scope.
3. **Cost-aware agent loop.** Should the agent see `cost_usd_remaining` and make
   routing decisions? v1: telemetry-only. Future: yes.
4. **Session-level cost rows.** `<session_open>` / `<session_cost>` synthetic
   rows require the `SandboxProvider::cleanup()` signature to carry telemetry
   output. Deferred; total tracked compute cost on successful executions is
   derivable by summing per-tool `cost_usd` rows in v1; actual vendor cost is
   higher because storage charges and failure-path overhead are not captured.
5. **`pi --stats remote-cost` verb.** Deferred pending session-level rows.
6. **`E2B_MAX_COST_USD` cost cap.** Deferred; requires polling cost after each
   call which adds latency and complexity.
7. **Multi-region selection.** E2B supports multiple regions. Default = platform
   default; future `E2B_REGION` env var.
8. **Session warm pool.** E2B supports "template" sandboxes with a pre-warmed
   state. Future: pre-upload the worker binary into a custom template to avoid
   the per-session 7 MB upload cost.
9. **Sprites/Daytona API validation.** Deferred to Commits H/I.
10. **`pi auth set` CLI subcommand.** Adding an `auth` subcommand to the `pi`
    CLI is out of scope for this RFD. Deferred to a future RFD covering CLI
    credential management. v1 users use `E2B_API_KEY` env var.
11. **`task` under remote sandboxes.** v1 marks `task` unavailable. A future
    RFD may define a host-dispatch primitive allowing `task` to spawn child
    runtimes on the host while the parent uses a remote sandbox.
12. **`web_search` under remote sandboxes.** v1 marks it unavailable (vsock
    proxy incompatible). Future: a host-dispatch path (the host receives the
    request over HTTPS, executes the search, returns the result) would enable
    it. This is analogous to the vsock proxy but over the E2B HTTP transport.
13. **`GitClone` and `Empty` upload modes.** Cut from v1 to keep scope
    bounded. `GitClone` adds a command round-trip at session open; `Empty`
    requires additional CLI/startup plumbing. Both can be added as a < 100
    LoC follow-up after the SmartSync base is stable. `--sandbox-upload-include`
    and `--sandbox-upload-exclude` CLI flags are also deferred.
14. **`pi-sandbox-worker` standalone distribution.** v1 requires the operator
    to set `PI_SANDBOX_WORKER_BIN` explicitly. A future packaging improvement
    should ship a standalone Linux-musl `pi-sandbox-worker` binary alongside
    `pi` (e.g. as a GitHub release asset) so the env-var override is
    unnecessary for normal installs.

## Out of scope

- **Self-hosted Daytona deployment.** This RFD covers SDK/API integration;
  provisioning a Daytona instance is the user's problem.
- **Cross-vendor migration.** No "switch from E2B to Sprites mid-session."
  Sandbox provider is fixed at session start.
- **contextfs over remote.** Not needed; the `file_writes` flushback covers the
  `write`/`edit` mutation case without a FUSE-based bridge.
- **Sandbox snapshot/restore API.** E2B supports snapshots; not exercised in v1.
- **`bash` file change detection.** Out of scope for v1; see §"Open questions".

## Revision history

- **v0.24 (2026-05-25):** Fix final surviving cost-accounting overstatement.
  §"Cost telemetry" still contained "total actual cost = (sum of per-tool
  `cost_usd`) + (sandbox_lifetime_secs × 0.000225)" which directly contradicted
  the qualified language added in v0.23. Replaced with: "Summing per-tool
  `cost_usd` yields tracked compute cost on successful provider executions;
  actual vendor cost is higher because it also includes storage charges and any
  failure-path compute overhead that emitted no row."

- **v0.23 (2026-05-25):** Fix two remaining cost-accounting overstatements. Both
  remaining "total cost is the sum of per-tool rows" normative statements are
  replaced with qualified language: "total tracked compute cost on successful
  provider executions is derivable by summing per-tool `cost_usd` rows; actual
  vendor cost is higher because storage charges and failure-path overhead are
  not captured in v1." Applied in §"Cost telemetry" and §"Deferred/out-of-scope
  work" (open question #4).

- **v0.22 (2026-05-25):** Three blocking fixes from latest review: (1) **Flushback
  verification mismatch classified as flushback failure** — the adversarial-example
  section previously said a host-side verification mismatch (length check fails after
  apply) "marks the tool call as an error" without poisoning the session. At that
  point the remote `/work` has the new content but the host does not, which is the
  same split-brain condition as a host apply failure. Fixed: verification mismatch now
  triggers session poison + best-effort DELETE, identical to apply failure. Updated
  §"Security boundary" adversarial example and §"Failure modes" table. (2) **Lazy-open
  partial failure cleanup specified** — if sandbox create succeeds but worker upload,
  `chmod`, or SmartSync fails, the provider now MUST best-effort DELETE the orphaned
  sandbox, clear its sandbox ID, and allow the next `execute_tool()` to retry the full
  open from scratch. This is NOT a session poison (poison is reserved for post-setup
  state divergence); it is a clean retry-from-scratch policy. Added normative text in
  §"Worker-shipping strategy" and §"Failure modes" table. (3) **Cost terminology
  corrected** — the previous "accurately accounts for all compute time" claim was too
  strong: provider-level failure paths (e.g. session open fails after the sandbox is
  created) can consume vendor compute without emitting a `cost_usd` row. Changed to
  "tracked compute cost on successful provider executions"; added explicit note that
  actual vendor cost also includes untracked storage and failure-path overhead.

- **v0.21 (2026-05-25):** Two blocking fixes: (1) **Setup-cost accounting made
  consistent** — the RFD previously claimed total cost is derivable by summing
  per-tool `cost_usd` rows, but setup operations (sandbox create, worker upload,
  `chmod`, SmartSync) happened before the first tool command and were accounted
  nowhere. Fixed by defining that `round_trip_ms` for the first tool call is
  measured from the start of lazy session open (including all setup network
  requests) through finished worker-command poll. Setup cost is charged to the
  first tool row. Added explicit subsection in §"Cost telemetry" and
  §"Provider/runtime contract amendments". (2) **Flushback failure no longer
  leaves session split-brained** — if host-side file write fails after a
  successful remote `write`/`edit`, the session previously continued in a
  diverged state (remote `/work` mutated, host cwd stale) with no recovery
  policy. Fixed by adding a session-poison protocol: on any flushback apply
  failure the provider sets an internal `poisoned` flag, immediately issues
  `DELETE /sandboxes/{id}` (best-effort), and returns
  `SandboxError::Provider("E2B session desynced...")` for all subsequent calls,
  forcing a clean session restart.

- **v0.20 (2026-05-25):** Minor citation fix: `invoke_via_sandbox` function declaration
  is at `runtime.rs:1815–1830` (the function body starts at 1815 with the `async fn`
  declaration; `1826` was an inner line). No design changes.

- **v0.19 (2026-05-25):** Two blocking fixes: (1) **Wrong commit hashes removed** —
  the Summary cited `b98e06c / 441aa85` as the commits that landed `MicroVmLauncher`,
  `pi-sandbox-protocol`, and `pi-sandbox-worker`; in the actual tree those resolve to
  unrelated commits. Replaced with the correct hashes: `77184cc` (MicroVmLauncher),
  `aa624e0` (pi-sandbox-protocol), `dcd37cd` (pi-sandbox-worker). (2) **`task`/`todo`
  rejection made provider-specific** — the previous draft added global
  `dispatch() -> ToolDispatch::Unavailable` overrides in `task/tool.rs` and
  `todo/tool.rs`. `ToolDispatch` has no provider parameter, and `MicroVmProvider`
  uses the trait-default `honors_tool_dispatch() -> true`, so those overrides would
  also disable `task`/`todo` under microvm, cutting across the RFD 0005 subagent
  story. Changed to provider-side prechecks in `E2bProvider::execute_tool` (same
  pattern as `web_search`, `ask`, autoresearch tools) — no changes to `task/tool.rs`
  or `todo/tool.rs` in Commit G. Updated host-bound tool table, Commit G changed-file
  table, and References accordingly.

- **v0.18 (2026-05-24):** Two blocking fixes: (1) **`invoke_via_sandbox` call-site
  uses real `exec.exit_status`** — the previous draft synthesized `if r.is_error
  { 1 } else { 0 }` for `exit_status` in `SandboxAction`, discarding the actual
  exit code carried by `SandboxExecution.exit_status` (e.g. `124` for timeout).
  Corrected to `exec.exit_status` so `SandboxAction` records the real exit code.
  (2) **`with_key()` / `PI_SANDBOX_OFFLINE` contract resolved** — the previous
  draft claimed `with_key(key) -> Self` (infallible) would "return
  `SandboxError::Unavailable`" when `PI_SANDBOX_OFFLINE=1`, which is a type
  contradiction. Resolved by deferring `PI_SANDBOX_OFFLINE` enforcement to the
  first `execute_tool()` call for `with_key()` (consistent with the worker-path
  lazy resolution). `from_auth()` — which is already fallible — may check
  `PI_SANDBOX_OFFLINE` eagerly. `with_key()` stays unconditionally infallible.
  Public API doc block and `PI_SANDBOX_OFFLINE` prose updated to be consistent.

- **v0.17 (2026-05-24):** Two blocking fixes: (1) **`invoke_via_sandbox` return
  type and call-site amendment made explicit** — the previous draft said "thread
  fields through" but never specified the exact `runtime.rs` code change. Now
  specifies that `invoke_via_sandbox` must return `Result<(ToolResult,
  SandboxExecution), String>` so the telemetry emit site can read `cost_usd` /
  `round_trip_ms` from the `SandboxExecution` metadata. Full before/after code
  blocks added with exact line references. (2) **Mock test must not call
  `dispatch_request()` directly** — `dispatch_request()` runs `pre_call_hygiene()`
  which recursively deletes `/tmp`, `/var/tmp`, `/root` (safe in guest VM,
  destructive on host). `e2b_mock.rs` test strategy updated to require canned
  `ToolResponse` fixtures or direct struct construction; calling `dispatch_request`
  from host test code is explicitly forbidden. Also: replaced `E2bProvider::new()`
  with `E2bProvider::from_auth()` in auth section; added explicit `PI_SANDBOX_OFFLINE`
  semantics for `with_key()` constructor.

- **v0.16 (2026-05-23):** Two targeted fixes: (1) **Commit G changed-file table
  made explicit about provider-side tool prechecks** — the `e2b.rs` entry now
  states that `E2bProvider::execute_tool` includes provider-side rejection for
  `web_search`, `ask`, `init_experiment`, `run_experiment`, `log_experiment`
  (returning `SandboxError::Provider(...)`), and explicitly notes that
  `ask/tool.rs` and `autoresearch/tools.rs` are NOT in Commit G because the
  rejection is provider-side, not tool-dispatch-level. (2) **Stats migration
  comments clarified** — Step 1 comment now correctly states that `schema_version`
  holds rows {1, 2} because row 1 is inserted in Step 1 (same call), then row 2
  is inserted by the migration in Step 2; the misleading "re-inserted by OR IGNORE
  on the next call" wording removed. The `INSERT OR REPLACE` vs `UPDATE` explanation
  now correctly describes the actual failure mode: `UPDATE` would collapse multiple
  version rows to the same value, causing a PRIMARY KEY conflict when a future
  migration runs with two or more existing rows.

- **v0.15 (2026-05-22):** Two blocking fixes: (1) **Stats migration made safe
  under repeated `ensure()` calls** — the existing `INSERT OR IGNORE INTO
  schema_version VALUES (1)` re-runs on every call; after migration the table
  holds rows `{1, 2}`. Replace bare `SELECT version` with `SELECT MAX(version)`
  so the gate always sees the highest version present, and use `INSERT OR REPLACE
  INTO schema_version VALUES (2)` instead of `UPDATE` so row `1` is left in
  place (no PRIMARY KEY conflict). Full idempotency analysis added to the
  migration section. (2) **Host-bound tool dispatch strategy aligned with
  implementation table** — `ask`, `init_experiment`, `run_experiment`, and
  `log_experiment` now specify provider-side precheck in `E2bProvider::execute_tool`
  (not a global `ToolDispatch::Unavailable` override, which would affect
  non-remote modes). (`task` and `todo` were described as retaining tool-level
  overrides here, but that was superseded by v0.19 which moved them to
  provider-side prechecks as well.) Table and prose updated to be consistent
  with the Commit G changed-file list.

- **v0.14 (2026-05-22):** Two blocking fixes: (1) drop out-of-scope
  `rfd-0026-implement-e2b-campaign.toml` from the finalize branch (will land
  separately after RFD merge); (2) add `crates/pi-sandbox/src/lib.rs` to the
  Commit G implementation plan — Commit G must add `pub mod remote;` and
  `pub use remote::e2b::E2bProvider;` to the crate root so downstream crates
  can name `E2bProvider`. Without that change Commit G is not implementable.
  Updated References section accordingly.

- **v0.13 (2026-05-22):** Trim verbose revision history (v0.1–v0.9 collapsed
  below) to reduce document length. No design changes.

- **v0.12 (2026-05-22):** Fix three implementation-blocking issues:
  (1) **Constructor inconsistency resolved** — `with_key(key) -> Self` is
  infallible; worker binary path resolution is deferred to the first
  `execute_tool()` call, not at construction. Mock tests can build
  `E2bProvider::with_key(...)` without `PI_SANDBOX_WORKER_BIN` set.
  (2) **Module layout aligned** — `mod.rs` is consistently `pub mod e2b;
  pub mod upload;` in both the RFD and campaign.
  (3) **Campaign M2/M3 skip contracts** — `PI_SANDBOX_WORKER_BIN` added
  alongside `E2B_API_KEY` as a required env var in live test contracts.
  "Total recorded compute cost (excluding storage)" wording aligned with
  §"Cost telemetry".

- **v0.11 (2026-05-21):** Fix four blocking issues: (1) worker binary
  distribution — no "adjacent to `pi`" fallback; `PI_SANDBOX_WORKER_BIN`
  env var required in v1. (2) Upload modes cut to SmartSync only; `GitClone`,
  `Empty`, and upload CLI flags deferred. (3) `from_auth(&cfg.auth_storage)`
  binding corrected. (4) Campaign build check expanded to all modified crates.

- **v0.10 (2026-05-20):** Remove "Wait —" drafting artifact from stats
  migration section; update campaign to match v0.9 RFD (line citations,
  `IS_STDIN_TRANSPORT` model, `session.abort().await` cleanup ordering).

- **v0.9 (2026-05-19):** Accurate `session.abort()` mechanics — abort sets
  `aborted = true` checked at loop top, not at tool-call boundaries; best-effort
  cleanup with documented race window. Fix `provider.rs:15` and `schema.rs:6`
  citation.

- **v0.8 (2026-05-19):** Citation fixes (five corrected line numbers); poll
  interval specified (100 ms initial, 200 ms subsequent); `IS_STDIN_TRANSPORT`
  corrected to module-level static `AtomicBool`; storage cost gap documented;
  frame cap arithmetic made explicit; implementation table updated.

- **v0.7 (2026-05-19):** Transport contradiction resolved (one-shot model,
  no background daemon); concurrent prompt drain subsection added; `web_search`
  rejection made provider-specific; prompt note wiring corrected to `startup.rs`;
  §"Operating notes" added.

- **v0.6 (2026-05-18):** Cleanup call site moved to mode exit; synthetic
  telemetry rows cut; subagent `Arc`-sharing contradiction resolved; `web_search`
  guard added; WebSocket dependency removed; stats migration added.

- **v0.1–v0.5 (2026-05-02 – 2026-05-17):** Initial stub (v0.1–v0.2);
  E2B pinned as v1 reference vendor (v0.3); `deny_unknown_fields` protocol
  version bump discovered and fixed (v0.4); `file_writes` scoped to
  `write`/`edit` only, `SandboxExecution` extended, subagent/auth/tool
  sections added (v0.5).

## References

- **RFD 0022** — `SandboxProvider` trait (`crates/pi-sandbox/src/provider.rs`).
- **RFD 0023** — Local MicroVM Sandbox; `pi-sandbox-protocol` + `pi-sandbox-worker`.
- **RFD 0027** — Pi-rs as a Self-Contained Rust SDK; `pi-sdk` façade crate.
- `crates/pi-sandbox/src/lib.rs:9` — current public module exports (amended in Commit G to add `pub mod remote;` + `pub use remote::e2b::E2bProvider;`).
- `crates/pi-sandbox/src/provider.rs:15` — `SandboxExecution` struct.
- `crates/pi-sandbox/src/provider.rs:26` — `SandboxError` enum.
- `crates/pi-sandbox/src/provider.rs:62` — `SandboxProvider` trait.
- `crates/pi-sandbox-protocol/src/lib.rs:20` — `CURRENT_PROTOCOL_VERSION`.
- `crates/pi-sandbox-protocol/src/lib.rs:28,48` — `#[serde(deny_unknown_fields)]` on `ToolRequest` / `ToolResponse`.
- `crates/pi-sandbox-protocol/src/framing.rs` — JSON-line framing helpers.
- `crates/pi-sandbox-protocol/src/framing.rs:32` — `DEFAULT_MAX_LINE_BYTES`.
- `crates/pi-sandbox-worker/src/main.rs` — guest worker binary (vsock mode today; stdin mode added in Commit G).
- `crates/pi-sandbox-worker/src/listener.rs` — vsock listener loop (basis for `serve_stdio`).
- `crates/pi-sandbox-worker/src/dispatch.rs` — per-request dispatch; `file_writes` population + `web_search` guard added in Commit G.
- `crates/pi-sandbox-worker/src/dispatch.rs:183–194` — current vsock-based web_search proxy (incompatible with remote; disabled via `IS_STDIN_TRANSPORT` guard in Commit G).
- `crates/pi-tools-core/src/lib.rs:117` — `ToolRegistry::with_unsafe_extras` (worker's tool set).
- `crates/pi-tool-types/src/lib.rs:56` — `ToolDispatch` enum.
- `crates/pi-coding-agent/src/native/task/tool.rs` — `task` tool (rejected via provider-side precheck in `e2b.rs`; no changes to this file in Commit G).
- `crates/pi-coding-agent/src/native/todo/tool.rs` — `todo` tool (rejected via provider-side precheck in `e2b.rs`; no changes to this file in Commit G).
- `crates/pi-coding-agent/src/native/lsp/tool.rs:101` — `lsp` tool already has `dispatch() -> Unavailable`.
- `crates/pi-tools-core/src/monitor.rs:203` — `monitor` tool already has `dispatch() -> Unavailable`.
- `crates/pi-coding-agent/src/native/task/executor.rs:255` — subagent inherits `sandbox_provider` Arc (not used in v1; `task` is rejected via provider-side precheck in `E2bProvider::execute_tool`).
- `crates/pi-coding-agent/src/modes/print.rs` — cleanup call added at mode exit in Commit G.
- `crates/pi-coding-agent/src/modes/json.rs` — cleanup call added at mode exit in Commit G.
- `crates/pi-coding-agent/src/modes/rpc.rs` — cleanup call added at mode exit in Commit G.
- `crates/pi-coding-agent/src/modes/interactive.rs` — cleanup call added at mode exit in Commit G.
- `crates/pi-ai/src/auth.rs:41` — `AuthStorage` struct definition.
- `crates/pi-ai/src/auth.rs:96` — `AuthStorage::ENV_KEYS` slice.
- `crates/pi-ai/src/auth.rs:144` — `AuthStorage::from_env_explicit`.
- `crates/pi-agent-core/src/session.rs:91` — `SessionEntryKind::SandboxAction`.
- `crates/pi-agent-core/src/runtime.rs:1714` — `SandboxAction` telemetry emit (full `append()` call at 1714–1723).
- `crates/pi-stats/src/schema.rs:6` — `CURRENT_VERSION` constant (bumped to 2 in Commit G).
- `crates/pi-stats/src/schema.rs:81` — `sandbox_actions` table DDL.
- `crates/pi-stats/src/ingest.rs:258` — `insert_sandbox_action` function.
- `crates/pi-stats/src/aggregate.rs:98` — `by_sandbox_provider` aggregation.
- `crates/pi-stats/src/cli.rs:8` — `StatsVerb` enum.
- `crates/pi-coding-agent/src/startup.rs:570` — `install_sandbox_from_flag`.
- `crates/pi-sandbox/src/contextfs.rs` — contextfs dep-pinning stub.
- `crates/pi-sandbox/src/microvm/contextfs_proxy.rs` — contextfs vsock proxy.
- `Cargo.toml:35` — `reqwest = { version = "0.12", ... }` workspace pin (no new WS dep needed).
- **E2B** — https://e2b.dev/docs.
- **Sprites** — (deferred; URL TBD).
- **Daytona** — https://daytona.io/docs.
