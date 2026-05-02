# RFD 0026 — Remote Sandbox Transports (E2B, Sprites, Daytona)

- **Status:** Discussion (v0.2 — stub)
- **Author:** pi-rs maintainers
- **Created:** 2026-05-02
- **Implemented:** (pending; depends on RFD 0023 landing first)

## Summary

Sister RFD to **RFD 0023** (local microVM sandbox). Where RFD 0023 ships local virtualization that converges three OS-specific launchers on one Linux guest rootfs, this RFD covers **remote microVM-as-a-service** vendors: E2B, Sprites, Daytona. They share with RFD 0023 only the public `SandboxProvider` trait surface; the rootfs / vsock / launcher infrastructure is not relevant to remote backends.

This is a **stub RFD**. The substance lands once RFD 0023 has shipped its `MicroVmLauncher` trait, the `pi-sandbox-protocol` crate, and the `pi-sandbox-worker` binary — because the cleanest design here likely **ships the same `pi-sandbox-worker` into the remote vendor's sandbox** so remote and local converge on the same RPC wire format. Without 0023's worker landed, this RFD cannot pin a non-vapor design.

### Why split this from 0023

- **No shared infrastructure.** Remote vendors are HTTP APIs to managed microVMs; nothing about the host-side launcher trait, the vsock IPC, the rootfs builder, or the per-OS `#[cfg]` machinery applies.
- **Different failure modes.** Remote = network errors, rate limits, billing, vendor downtime, region selection. Local = `/dev/kvm` access, vfkit binary missing, WHPX disabled.
- **Different telemetry.** Remote calls cost real money per second of compute. Local calls are free at runtime. The `cost_usd` field on telemetry is meaningful only here.
- **Independent landing cadence.** Each vendor integration is a self-contained 400–800 LoC commit; they do not gate each other or the local microVM story.

## Background

### What pi's tools need from a remote vendor

Pi's built-in tools split into three groups:

- **File operations** (`read`, `write`, `edit`, `ls`, `find`, `grep`) — need a filesystem mounted with the user's project content.
- **Process operations** (`bash`, `monitor`) — need a shell inside the remote environment.
- **Network** (`web_search`) — orthogonal; works the same regardless of sandbox.

A remote vendor that exposes "create a sandbox session, run shell commands inside it, read files from it" can satisfy all three groups, **provided** we have a way to materialize the user's cwd into the remote sandbox.

### Vendor API survey (to be completed before v0.2)

| Vendor   | Cold boot | Pricing | API style | Auth | Persistent sessions |
|----------|-----------|---------|-----------|------|----------------------|
| E2B      | ~1.5–3s   | per-second compute | HTTPS + WebSocket; SDKs in JS/Python | API key | yes (default lifetime, configurable) |
| Sprites  | TBD       | TBD     | TBD       | TBD  | TBD                  |
| Daytona  | varies (self-hosted vs cloud) | depends | gRPC + REST | API key + workspace token | yes (workspaces) |

(Pre-v0.2 work: confirm each vendor's exact API shape, regional availability, idempotency guarantees, and rate limits. The summary above is from public docs as of 2026-05; needs reconfirmation by hands-on probing.)

## Proposal (sketch — do not implement yet)

### 1. Architecture

```
pi_sandbox::SandboxProvider  (RFD 0022)
    │
    ├── LocalProcessProvider          (RFD 0022; no isolation)
    ├── MicroVmProvider               (RFD 0023; local microVM)
    └── RemoteProvider                (this RFD)
            │
            └─ holds Box<dyn RemoteTransport>
                  │
                  ├─ E2bTransport
                  ├─ SpritesTransport
                  └─ DaytonaTransport
```

### 2. Trait signatures (v0.1 sketch — interrogate before v0.2)

```rust
// crates/pi-sandbox/src/remote/transport.rs

#[async_trait]
pub trait RemoteTransport: Send + Sync {
    fn transport_name(&self) -> &'static str;          // "e2b" | "sprites" | "daytona"

    async fn probe(&self) -> Result<ProbeReport, SandboxError>;

    /// Open a long-lived remote session. Maps 1:1 to a pi session.
    async fn open_session(&self, spec: &RemoteSessionSpec)
        -> Result<Box<dyn RemoteSession>, SandboxError>;
}

#[derive(Debug, Clone)]
pub struct RemoteSessionSpec {
    pub host_cwd: PathBuf,
    pub upload_strategy: UploadStrategy,
    pub timeout: Duration,
    pub max_cost_usd: Option<f64>,
}

#[derive(Debug, Clone)]
pub enum UploadStrategy {
    /// rsync-equivalent of the host cwd to the remote sandbox at session open.
    /// File diffs flushed back to host on each tool call that mutated.
    SyncCwd,
    /// `git clone <url>` inside the sandbox; subsequent edits stay remote
    /// and surface back to the host as a structured patch.
    GitClone { url: String, rev: String },
    /// Empty sandbox — only useful for tools that don't touch the user's project.
    Empty,
}

#[async_trait]
pub trait RemoteSession: Send + Sync {
    async fn execute(
        &self,
        ctx: &ToolContext,
        tool_name: &str,
        tool_input: &serde_json::Value,
    ) -> Result<RemoteExecution, SandboxError>;

    async fn close(self: Box<Self>) -> Result<(), SandboxError>;
}

pub struct RemoteExecution {
    pub result: ToolResult,
    pub cost_usd: Option<f64>,           // appended to telemetry
    pub remote_duration_ms: u32,
    pub round_trip_ms: u32,
}
```

### 3. The "ship our worker into the remote" question (split with RFD 0023)

The single biggest design decision here. Two paths:

- **(A) Ship `pi-sandbox-worker` into the remote sandbox at session open.** Run it in the background, talk to it via the vendor's stdin/stdout/exec API. Remote and local then share the same per-tool RPC. Cost: ~7 MB upload at session open, ~30ms per call once running.
- **(B) Reimplement each tool against each vendor's primitives.** Cost: 7 tools × 3 vendors = 21 implementations + drift.

**Cross-RFD ownership** (clarified after RFD 0023 v0.4): the *worker binary* and its protocol are owned by RFD 0023. The worker is **explicitly designed to be transport-agnostic** at the JSON-line layer (any `AsyncRead`/`AsyncWrite`, not just vsock). RFD 0026 v0.2 picks Option A or B; if (A), it reuses RFD 0023's worker binary unchanged.

Strong default is (A) — converging the wire protocol across local + remote is the only way the agent loop sees a uniform world. (B) only wins if a specific vendor doesn't allow uploading + running an arbitrary binary, **and** we still want to support that vendor; in that case we accept the per-tool drift cost for that vendor only. The decision is made per-vendor in v0.2: each transport in §"Implementation schedule" picks A or B.

**Vendor API constraints to validate before v0.2** (which will determine A vs B per-vendor):
- E2B: confirm long-running processes survive between SDK calls (strongly suspected yes; needs hands-on confirmation).
- Sprites: vendor API not yet surveyed.
- Daytona: self-hosted variant likely permits arbitrary binaries; cloud variant TBD.

### 4. Auth and cost telemetry

- `SandboxAuthStorage`: separate from LLM-provider `AuthStorage`. On-disk encryption shares the same primitives. Keys: `(transport: "e2b", scope: "default")` etc.
- The `SandboxAction` telemetry variant gains `cost_usd: Option<f64>` and `round_trip_ms: Option<u32>`. These are already specified in **RFD 0023 v0.4 §2** as part of the union schema (one struct, all-optional new fields, non-breaking amendment to RFD 0022). RFD 0026 emits these fields; RFD 0023 ignores them. `pi-stats` ingests both into the same `sandbox_actions` table with nullable columns.
- A new aggregator `aggregate::by_remote_transport(...)` produces per-vendor cost rollups (filters rows where `cost_usd IS NOT NULL`).
- A new CLI verb: `pi --stats remote-cost`.

### 5. Cwd binding (the gap RFD 0023 doesn't have)

When a user runs `cd ~/myproject; pi --sandbox-provider=e2b "..."`, the remote sandbox doesn't have `~/myproject`. **Default strategy in v0.2 is `UploadStrategy::SmartSync`**, not the previous draft's `SyncCwd`: rsync-equivalent on session open with **aggressive default exclusions on top of `.gitignore`** to dodge the "500MB monorepo" trap that breaks first-time UX. Excluded by default regardless of `.gitignore`:

```
node_modules/   target/         .venv/      venv/         dist/
build/          __pycache__/    .next/      .nuxt/        .cache/
.gradle/        .terraform/     vendor/     bower_components/
*.pyc           *.class         *.o         *.so          *.dylib
```

Users override with `--sandbox-upload-include` / `--sandbox-upload-exclude` flags. Files larger than 100MB always require an explicit allow flag. First-call upload progress reported via `SandboxAction.duration_ms` and a session-level `RemoteSessionOpened` telemetry row (separate from per-call rows; future addition).

Three upload modes:

- **`SmartSync`** (default) — described above.
- **`GitClone { url, rev }`** — clone fresh inside the sandbox. Skips the upload entirely; faster session-open. Caveat: uncommitted local changes don't ship. Useful for read-only investigation tasks.
- **`Empty`** — no upload. Useful for tools that don't touch the user's project (synthetic tasks, web search, etc.).

**File mutation flushback**: when a guest tool writes to `/work/foo.rs`, the worker emits a `WriteFile` RPC over the same vsock-equivalent transport with `(path, contents, mode)`. The host adapter applies it to the user's local cwd. Partial-write semantics: each tool call's mutations form one batch; a single failed write fails the whole tool call (consistent with the RPC's all-or-nothing return).

## Implementation schedule

Three commits, each independent:

| # | Commit | Est. LoC |
|---|---|---|
| **G** | `RemoteTransport` trait + `RemoteProvider` + `E2bTransport` | 800 |
| **H** | `SpritesTransport` | 600 |
| **I** | `DaytonaTransport` | 700 |

Each ships its own CLI flag the moment it lands (`--sandbox-provider=e2b`, etc.). No cross-vendor coverage promise gates the flag — single-vendor failure modes are well-isolated.

## Open questions (to fill in before v0.2)

1. **Pick: ship our worker into remote (A), or per-vendor reimpls (B)?** Strong default A.
2. **Vendor API confirmation.** Hands-on probe each vendor's API before designing transports. Sprites' API in particular is unconfirmed.
3. **Cost-aware loop.** Should the agent see `cost_usd_remaining` and budget decisions accordingly? v1.0: telemetry-only. Future: yes.
4. **Multi-region selection.** E2B / Daytona have multiple regions. Default = closest? User-configurable?
5. **Rate limit handling.** Per-vendor: retry with backoff, surface as a `SandboxError::RateLimited` distinct from generic Unavailable.
6. **Default `UploadStrategy`** — `SyncCwd` is the safe default; `GitClone` is opt-in.
7. **Air-gapped users.** Remote backends are fundamentally network-dependent. `PI_SANDBOX_OFFLINE=1` must refuse remote transports cleanly.

## Out of scope

- **Self-hosted Daytona deployment.** This RFD covers the SDK / API integration; provisioning a Daytona instance is the user's problem.
- **Cross-vendor migration.** No "switch from E2B to Sprites mid-session." Sandbox provider is fixed at session start.

## Revision history

- **v0.2 (2026-05-02):** In-repo `rfd-critic` pass + cross-RFD coordination with 0023 v0.4. Reframed the worker-shipping (A vs B) decision as **per-vendor**, not RFD-global; clarified that the worker binary + protocol are owned by RFD 0023, not 0026. Aligned telemetry on RFD 0023 v0.4's union schema (one `SandboxAction` struct, all-optional new fields). Renamed `UploadStrategy::SyncCwd` → `SmartSync` with aggressive default exclusions (node_modules, target, .venv, …) to dodge the "500MB monorepo" trap. Specified file-mutation flushback semantics (per-call batch, all-or-nothing). Numbered as RFD 0026 (was 0024 — collision with ratatui-tui-rewrite caught by rfd-critic).
- **v0.1 (2026-05-02):** Initial stub split out of RFD 0023 v0.2 per critical review feedback. Substance pending: vendor API confirmation, worker-shipping decision, hands-on cost telemetry validation.

## References

- **RFD 0022** — Sandbox Execution for Tool Decisions.
- **RFD 0023** (sister) — Local MicroVM Sandbox.
- **E2B** — https://e2b.dev/docs.
- **Sprites** — (URL TBD, confirm before v0.2).
- **Daytona** — https://daytona.io/docs.
