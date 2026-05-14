# RFD 0027 — Pi-rs as a Self-Contained Rust SDK

- **Status:** Implemented (v0.7)
- **Author:** pi-rs maintainers
- **Created:** 2026-05-02
- **Implemented:** 2026-05-14 — Commits A–K (Track 1–3) merged on main; pre-crates.io polish trail in `crates/pi-sdk/CHANGELOG.md` [Unreleased]. Publication of `pi-sdk 0.1.0` to crates.io + enabling `cargo-semver-checks` CI gate remain as the release step.

## Summary

Pi-rs is currently **a binary that ships a coding agent**. The internals are mostly cleanly factored across nine crates (`pi-tool-types`, `pi-ai`, `pi-tools`, `pi-sandbox`, `pi-agent-core`, `pi-coding-agent`, `pi-stats`, `pi-orchestrate`, `pi-tui`), but no public, stability-committed SDK surface exists. Today, embedders who want to drop a pi-style agent loop into their own Rust app must:

- pick which crates to depend on (the README doesn't say which are "public");
- accept that any minor version can break their integration (workspace pinned to 0.1.0);
- mix concerns from `pi-coding-agent` (TUI, halo, evolve, slash commands, REPL modes) that aren't relevant to embedding;
- read the source to figure out what `BuildConfig` / `RuntimeConfig` / `AgentSessionRuntime` actually expects.

This RFD turns pi-rs into a **self-contained library**. A new `pi-sdk` façade crate becomes the single public entry point for Rust embedders. The `pi` binary becomes one consumer of the SDK alongside future ones (compiled agents per RFD 0028, third-party integrations, custom CI runners, embedded evaluators).

### Inspiration: Flue

Flue (homepage `flueframework.com` / repo `withastro/flue` per public materials surveyed at draft time, 2026-05-02 — verify before publishing this RFD; project is at v0.3.x and may move) is a TypeScript agent-harness framework in the same product category as pi-rs. **Pi-rs does not integrate with Flue and is not consumed by Flue** — separate research established structural mismatch (Flue is TS + Cloudflare Workers; pi-rs is native Rust). The framing below extracts Flue's *design patterns* as inspiration only; everything in this RFD stands on its own without any Flue dependency. If Flue is renamed or moves before this RFD ships, the design lessons cited below stay valid — they're patterns common to mature agent-harness frameworks (e.g., `@anthropic-ai/sdk`, `langchain`, `goose`), not Flue-specific.

What's worth stealing from Flue's design:

- **"Agent = Model + Harness"** as the framing. The harness is the SDK; the binary is one harness instance.
- **Programmable-first.** REPL and TUI are *consumers* of the harness, not the other way around. Headless and embeddable on day one.
- **Single SDK package** (`@flue/sdk`). Embedders add one dependency, not nine.
- **Plugin / connector pattern.** Sandbox backends (Daytona-shaped), custom tools (`ToolDef`), MCP — all reachable through stable trait surfaces.
- **`flue build`** verb. RFD 0028 is the pi-rs analogue.

### What's load-bearing

1. **Single façade crate `pi-sdk`** that re-exports the embed surface. Embedders depend on this and nothing else.
2. **Stability commitment** at 1.0 with `#[non_exhaustive]` blanket on every public struct + enum variant (so MINOR-additive field growth is non-breaking) and a security-CVE escape hatch (a High+ severity CVE in a stable trait may force a breaking change inside a MINOR with 30-day pre-notification).
3. **Clear boundary** between SDK material and binary-side material. Halo, evolve, TUI, slash commands, REPL modes stay in `pi-coding-agent` and are NOT in the SDK.
4. **Embed-in-N-lines examples** that compile, run, and reflect realistic embedder shape (not a "Hello World" brochure). First example shows context, tool-event surfacing, cost tracking, structured errors, cancellation.
5. **Safe-by-default surface.** The default constructors do NOT silently expose `bash` or read process env for credentials. `LocalProcessProvider` is renamed to `InProcessExecutor` so its name does not misrepresent isolation. `with_extras()` is split into `with_readonly_extras()` (no shell, no fs mutation) and `with_unsafe_extras()` (full set, name itself is the warning).
6. **Hardening contract.** The runtime invariants are written down: `catch_unwind` around tool invocation, stream-event validation, ToolGate session/turn context, AuthStorage 0o600 + atomic rename + scoped env access. These are not "polish" — they patch real bugs in the current implementation surfaced by adversarial review.
7. **Feature-flag matrix on day 1.** Provider gating (`anthropic`/`openai`/etc.) and tool gating (`tools-net`, `tools-process`) ship in 0.1, not "before 1.0." Default features = minimal viable embed (Anthropic + read-only tools); embedders opt in to more.
8. **No SDK 1.0 until RFD 0023 (microvm sandbox) and RFD 0026 (remote sandbox) settle**, AND the hardening contract from §4.5 is implemented in pi-rs itself, not just specified here.

### Threat model and trust boundaries

Pi-sdk inherits a substantial transitive trust footprint and exposes that footprint to embedders. v0.7 names this explicitly, prior versions did not.

- **What pi-sdk defends against**: the LLM model produces malformed responses, the tool input is adversarial JSON, a panic in one tool brings down the runtime, a streaming provider sends out-of-order or oversized events, a tool registers with a colliding name to shadow a real tool. Defense via runtime invariants in §4.5 below.
- **What pi-sdk does NOT defend against, by design**: a malicious crate the embedder pulls in (supply-chain attack on `pi-ai`'s transitive deps); a compromised LLM endpoint with valid TLS; an embedder running pi-sdk in a parent process whose environment is attacker-controlled (`from_env()` is a CWE-526 magnet, embedders are responsible for trusting their env); a model whose output is verbatim-displayed to a human operator in a trusted UI surface (`StreamInterceptor` consumers carry the "model content is untrusted" responsibility, see §4.5).
- **Shared trust with the embedder**: the embedder picks the `SandboxProvider`, the `ToolGate`, and the `AuthStorage` source. Pi-sdk ships safe defaults; the embedder is responsible for not opting out of them in production.
- **The SDK is open-source, the SDK's deps are not all audited.** Embedders running in compliance-sensitive environments MUST run `cargo audit` and `cargo deny`, pin lockfiles, and treat each `pi-sdk` MINOR upgrade as a supply-chain event. We commit to publishing a SECURITY.md, a coordinated-disclosure address, and `cargo audit` clean status for each release.

### What this is NOT

- **Not a Flue plugin.** Researched separately; structural mismatch (Flue is TS + Cloudflare Workers, pi-rs is native Rust). The Flue framing is inspiration only.
- **Not an FFI.** Python bindings (pyo3), Node bindings (napi-rs), WASM (`wasm-bindgen`) are all out of scope. If they ever happen, they wrap `pi-sdk`, not the other way around.
- **Not a "soft fork."** The existing `pi-coding-agent::sdk` re-export module (63 lines, `crates/pi-coding-agent/src/sdk.rs`) is the seed. We promote it to its own crate, reorganise, document, version.
- **Not a freeze of pi-coding-agent's CLI.** The 40-flag `pi --print` surface stays. Demoting it to "interactive REPL + admin verbs" happens incrementally as RFD 0028 (compiled agents) gives operators a better headless story.

## Background

### What pi-rs has today

The workspace is mostly SDK-shaped. Walking each crate:

| Crate | Public-surface readiness | Notes |
|---|---|---|
| `pi-tool-types` | **Ready.** | POD `ToolResult`, `ToolSpec`, `ToolError`. Just landed (RFD 0023 Commit A1). Zero deps beyond serde/serde_json/thiserror. |
| `pi-ai` | **Ready.** | `Provider` trait covers Anthropic, OpenAI, OpenAI-compat, Google, Bedrock, Azure. `EventStream`, `GenerateRequest`, `Message`, `ContentBlock`, cost types. |
| `pi-tools` | **Mostly ready.** | `Tool` trait + `ToolRegistry`. RFD 0023 will split into `pi-tools-core` (file/process) + `pi-tools-net` (web_search). The split is independent of this RFD. |
| `pi-sandbox` | **In flight.** | `SandboxProvider` trait stable from RFD 0022; `MicroVmLauncher` arrives with RFD 0023; `RemoteTransport` with RFD 0026. SDK 1.0 waits for both. |
| `pi-agent-core` | **Mostly ready.** | `RuntimeConfig`, `AgentSessionRuntime`, `AgentSession`, `ProviderFactory`, `ToolGate`, `StreamInterceptor`, `SessionManager`, `SessionEntry`. The runtime loop is the heart of the SDK. |
| `pi-coding-agent` | **Mixed.** | `src/sdk.rs` (63 lines) is the seed. Most of the rest (TUI, halo, evolve, modes, slash commands, picker, trajectory) is binary-only. |
| `pi-stats` | **Embed-friendly but not SDK-shaped.** | SQLite ingest + axum dashboard. Useful only when the embedder wants the full telemetry pipeline. SDK 1.0 surface includes the `SessionEntry` types (so embedders can produce telemetry) but NOT the dashboard. |
| `pi-orchestrate` | **Standalone.** | Campaign runner. Out of SDK scope; consumed by binaries. |
| `pi-tui` | **Not SDK material.** | Ratatui rendering, slash UI. Stays in the binary. |

The current `pi-coding-agent::sdk` (`crates/pi-coding-agent/src/sdk.rs:1-63`) re-exports a working but incomplete surface. It exposes `BuildConfig`, `build_runtime_config`, the runtime types, the AI types, the tool types — all reasonable, but:

- It lives inside the binary crate (any embedder pulls in the whole binary's deps including ratatui, crossterm, halo, evolve).
- It has no version policy.
- It has no examples.
- It's invisible to crates.io (the binary is `pi`, the lib name is `pi_coding_agent`).

### Why now

Three signals converging:

1. **RFD 0028 (compiled agents) needs an SDK.** The `pi-build my-agent.toml` codegen produces a `main.rs` whose imports must be stable across pi-rs versions. Without a versioned SDK contract, every pi-rs MINOR breaks every compiled agent.
2. **RFD 0022 + 0023 + 0026 stabilise the sandbox layer.** Once 0023 and 0026 land, `SandboxProvider` is settled. That removes the biggest source of SDK churn.
3. **Embedder demand is plausible.** Internal users (CI runners, evaluators, headless reviewers) and external users (dev-tool authors who want a Rust agent core not Python) are both real audiences once the surface is documented.

### What we're not doing

- **Not freezing the pre-1.0 surface.** SDK 0.x is explicitly experimental, can break with any pi-rs MINOR. 1.0 is when the contract solidifies.
- **Not creating a stability monoculture.** Some types stay internal forever (the runtime's private state, the session storage backend's SQLite specifics, internal helper types).
- **Not turning every internal struct into a public API.** Stability is a cost; pay it only for surfaces embedders genuinely depend on.

## Proposal

### 1. The `pi-sdk` façade crate

New crate `crates/pi-sdk/` with one job: be the single public entry point.

**Layout:**

```
crates/pi-sdk/
├── Cargo.toml
├── README.md            # the "embed-in-50-lines" doc, see §5
├── src/
│   └── lib.rs           # re-exports + a small amount of glue
└── examples/
    ├── 01_minimal.rs    # 50-line embed example (§5)
    ├── 02_custom_tool.rs
    ├── 03_custom_provider.rs
    ├── 04_custom_sandbox.rs
    └── 05_event_streaming.rs
```

**Cargo.toml dependencies:** ONLY workspace-aliased pi-rs crates that contribute to the public surface — `pi-tool-types`, `pi-ai`, `pi-tools-core` (post-A2; pre-A2 = `pi-tools`), `pi-sandbox`, `pi-agent-core`. No `pi-tui`, no `pi-coding-agent`, no `pi-orchestrate`, no `pi-stats`.

**Feature flag matrix (ships at 0.1, NOT deferred to 1.0).** Per multi-personality review, embedder compile time and supply-chain blast radius both demand provider/tool gating from day one:

```toml
[features]
default = ["provider-anthropic", "tools-readonly"]

# Providers — embedders pick what they need; pulling all six is opt-in.
provider-anthropic       = ["pi-ai/provider-anthropic"]
provider-openai          = ["pi-ai/provider-openai"]
provider-openai-compat   = ["pi-ai/provider-openai-compat"]
provider-google          = ["pi-ai/provider-google"]
provider-bedrock         = ["pi-ai/provider-bedrock"]      # pulls AWS SDK
provider-azure           = ["pi-ai/provider-azure"]

# Tools — readonly is the safe default; mutation/exec require explicit opt-in.
tools-readonly           = ["pi-tools-core/read", "pi-tools-core/grep", "pi-tools-core/find", "pi-tools-core/ls"]
tools-mutation           = ["pi-tools-core/write", "pi-tools-core/edit"]
tools-process            = ["pi-tools-core/bash", "pi-tools-core/monitor"]
tools-net                = ["pi-tools-net/web_search"]

# Sandbox launchers — gated until RFDs 0023+0026 stabilise; "unstable" until SDK 1.2.
sandbox-microvm-unstable = ["pi-sandbox/microvm"]
sandbox-remote-unstable  = ["pi-sandbox/remote"]

# Mock providers / sandbox for embedder tests (zero LLM cost in CI).
mocks                    = []
```

The `default` set is deliberately minimal: one provider + read-only tools. An embedder doing `cargo add pi-sdk` gets a buildable, safe-by-default agent that cannot shell out. Adding `bash` requires `--features tools-process`. Adding the AWS SDK requires `--features provider-bedrock`. The name itself is the warning.

Cross-cutting deps (serde, tokio, async-trait, thiserror, tracing) are unconditional. RFD 0023 and 0026 trait surfaces are gated on `sandbox-microvm-unstable` / `sandbox-remote-unstable` until those RFDs land + at least one MINOR of stability — see §3.

**Public surface (re-exports in `src/lib.rs`):**

```rust
//! `pi-sdk` — the public Rust API for embedding pi-rs in another application.
//!
//! See `examples/01_minimal.rs` for a 50-line embed example.

// ─── Provider / model ─────────────────────────────────────────────
pub use pi_ai::{
    AnthropicProvider, AuthMethod, AuthStorage, AzureOpenAiProvider,
    BedrockAnthropicProvider, ContentBlock, EventStream, FinishReason,
    GenerateRequest, GenerateResponse, GoogleProvider, Message, ModelInfo,
    ModelRegistry, OpenAiCompatProvider, OpenAiProvider, Provider,
    ProviderConfig, ProviderKind, Role, StreamEvent, StreamEventKind,
    ThinkingLevel, ToolCall, Usage,
};

// ─── Tools ────────────────────────────────────────────────────────
pub use pi_tool_types::{ToolError, ToolResult, ToolSpec};
pub use pi_tools::{Tool, ToolContext, ToolRegistry};

// ─── Sandbox ──────────────────────────────────────────────────────
pub use pi_sandbox::{
    LocalProcessProvider, SandboxError, SandboxExecution, SandboxProvider,
};
// MicroVmLauncher, MicroVmProvider, VmHandle, VmSpec, VmCeiling, CallLimits
// (RFD 0023 §Proposal §2) and RemoteTransport, RemoteProvider, RemoteSession,
// UploadStrategy (RFD 0026 §Proposal §2) join the public surface as those
// RFDs' implementations land. The trait shapes are already specified; only
// the impl artifacts need to materialise before re-exporting.

// ─── Agent runtime ────────────────────────────────────────────────
pub use pi_agent_core::{
    create_agent_session, AgentEvent, AgentEventKind, AgentSession,
    AgentSessionRuntime, Compactor, ContextFile, DefaultProviderFactory,
    EventSender, ProviderFactory, RuntimeConfig, SessionEntry,
    SessionEntryKind, SessionManager, SessionMeta, SessionTree, Settings,
    StreamInterceptor, ToolGate, ToolGateOutcome,
};

// ─── Convenience builder (moved from pi-coding-agent::sdk) ────────
// NOTE: BuildConfig stays in 0.x for back-compat. Per multi-personality
// review (UX + Maint), 1.0 ships with `RuntimeConfig::builder()` as the
// ONLY supported construction path; BuildConfig becomes a deprecated
// wrapper. Two builders for one config is a permanent papercut.
pub mod build;
pub use build::{BuildConfig, build_runtime_config};

// ─── Top-level error type (NEW per UX review) ─────────────────────
// Embedders catching errors today see per-crate types bubble up
// (`pi_ai::Error` from one site, `pi_sandbox::SandboxError` from
// another, etc.) — anyhow chains get ugly fast. SDK ships one
// thiserror-based facade with `#[from]` impls for each underlying
// crate's error type. SDK 1.0 promises `Error: std::error::Error +
// Send + Sync + 'static` and stable variant names.
pub mod error;
pub use error::{Error, Result};

// ─── Cost helper (NEW per UX review) ──────────────────────────────
// Every embedder writes the same per-model price table. Ship one.
// Best-effort numbers, refreshed each MINOR; embedders override via
// `CostRegistry::override_for(model_id, prices)`.
pub mod cost;
pub use cost::{CostRegistry, estimate_cost_usd};

// ─── Mock provider + sandbox (NEW per UX review, gated on `mocks`) ─
#[cfg(feature = "mocks")]
pub mod mocks;
#[cfg(feature = "mocks")]
pub use mocks::{MockProvider, MockSandboxProvider};

// ─── Quick-start convenience (NEW per Hardening §4.5 #8 + UX review) ─
// One call wires `AuthStorage::in_memory()` (NO env scan) + `ToolRegistry::with_readonly_extras()`
// + `InProcessExecutor::with_readonly_defaults()`. The returned runtime has NO credentials —
// embedders MUST call `runtime.auth_storage().set(provider, key)` (or pass a populated
// `AuthStorage` via the full builder) before the first `prompt()`, otherwise the LLM call
// fails at first turn with `Error::Provider(NoCredential)`. For first-touch demos and
// docs.rs examples. Production embedders use the full builder explicitly so the
// surface they wire is auditable.
pub fn quick_start(provider: &str, model: &str) -> Result<AgentSessionRuntime, Error>;
```

**Pre-Commit-A prerequisite:** `pi-ai`'s `lib.rs` must add `pub use provider::EventStream;` (today only `StreamEvent` and `StreamEventKind` are at the top level — see `crates/pi-ai/src/lib.rs:37`). The SDK re-export list above lists `EventStream` for embedders writing custom `Provider` impls; the prerequisite re-export is a one-line change in Commit A.

**Key rule:** `pi-sdk` re-exports only. No new types defined here except `BuildConfig` (the convenience builder). Underlying crates remain the source of truth; if a type moves between crates, only `pi-sdk/src/lib.rs` updates and embedders see no change.

### 2. What stays binary-side (NOT in the SDK)

Explicitly excluded from `pi-sdk`:

- **`pi-coding-agent::halo`** — RFD 0025 supervisor. Binary-specific.
- **`pi-coding-agent::evolve`** — RFD 0011/0013 AGENTS.md mutation daemon. Binary-specific.
- **`pi-coding-agent::modes`** (interactive, print, json, rpc) — CLI layer.
- **`pi-coding-agent::slash`** — slash command dispatch.
- **`pi-coding-agent::picker`, `trajectory`, `share`** — binary UX.
- **`pi-coding-agent::auto_approve`** — embedders ship their own `ToolGate` impl, not pi's policy file format.
- **`pi-tui`** — terminal rendering.
- **`pi-orchestrate`** — campaign runner, consumed by `pi --orchestrate` only.
- **`pi-stats`** — SQLite + axum dashboard. The `SessionEntry` types are in the SDK so embedders can write telemetry, but the storage / aggregation / server is binary-side.

A future feature flag (`pi-sdk = { features = ["stats"] }`) might re-export the stats types when an embedder explicitly wants them. v1.0 ships without.

### 2.5 Embedder-side surfaces (clarifications)

Several pi-rs subsystems sit at the boundary between SDK material and binary-side. Disambiguating each:

- **Subagents via the `task` tool (RFD 0005).** The `task` tool is **NOT in `pi-tools`** — it lives in `crates/pi-coding-agent/src/native/task/tool.rs` (struct `TaskTool`) and is registered by the binary's startup at `crates/pi-coding-agent/src/startup.rs:303` (`tools.register(Arc::new(crate::native::task::TaskTool::new()))`). It depends on `ParentHandle` runtime machinery (`with_runtime` wrapper) that is binary-only. **SDK 1.0 stance:** the `task` tool is binary-side; SDK embedders do NOT get subagent orchestration out of the box. Embedders who want subagent-style branching use `AgentSession::send` repeatedly with their own coordination layer (open multiple sessions, route a "summarise this" prompt to one and a "compose the answer" to another, etc.). If demand surfaces, a future RFD can extract `ParentHandle` + `TaskTool` into a `pi-task-sdk` companion crate; this is explicitly out of scope for SDK 1.0.

- **MCP servers.** Pi-rs does **not** ship MCP-server adapter code today (no MCP loader in any crate as of this RFD's draft date). Embedders who want MCP bridge it themselves by implementing the `Tool` trait per MCP server they need to call — `Tool` is already the right abstraction (one tool name per MCP function, JSON-schema input). **SDK 1.0 stance:** no MCP-specific surface in `pi-sdk`; bring-your-own-bridge. A future RFD can add a generic `ToolSource` trait + a default MCP-WebSocket adapter if multiple embedders ask for it.

- **Orchestrate runner (RFD 0021).** `pi-orchestrate` is a separate workspace crate, not part of `pi-sdk`. Embedders who want campaign orchestration depend on `pi-orchestrate` directly (it has its own version cadence). The orchestrate runner consumes `pi` binaries — it doesn't embed the SDK.

- **Halo loop (RFD 0025).** Entirely binary-side. Halo coordinates the orchestrate runner + evolve daemon + supervisor state machine; none of that belongs in `pi-sdk`. Embedders who want self-improvement loops build their own using the SDK primitives (`AgentSession`, `SessionEntry` telemetry, `ToolGate` for approval policy). A future RFD can extract reusable pieces of halo (the proposer prompt, the rollback heuristics, the cycle config schema) into a `pi-halo-sdk` companion crate if demand surfaces.

- **`MonitorPump` (RFD 0017 TTSR + monitor injection).** The `StreamInterceptor` trait is in the SDK and is stable. The specific `MonitorPump` implementation that drives `pi --monitor` is binary-side. Embedders implement their own `StreamInterceptor` for custom logging, time-travelling reminders, or streaming hooks.

- **Auto-approve (`AutoApproveGate`).** The `ToolGate` trait is in the SDK; embedders ship their own gate. Pi's policy file format (`~/.pi/agent/auto-approve.json`) and the `auto-policy` / `auto-judge` modes are binary-side. A future RFD can extract the policy parser if external embedders want compatibility, but it's not promised at SDK 1.0.

### 3. Stability commitment and versioning

**SDK 0.x (now → ~RFD 0023+0026 land):** explicitly experimental. README and crate docs both lead with:

> ⚠️ pi-sdk is in pre-1.0. Any 0.x → 0.x+1 release may break the public API. Pin a fixed version in your Cargo.toml.

**SDK 1.0 (after RFDs 0023 + 0026 land + at least one external embedder dogfoods):**

- **MAJOR (1.x → 2.0):** breaking change. Allowed when motivated; require an entry in the migration guide.
- **MINOR (1.x → 1.x+1):** additive only. New trait methods get default impls. New types introduced but never renamed. New variants on enums use `#[non_exhaustive]` or are gated on a feature flag.
- **PATCH:** bug fixes only.

**6-month back-compat window** for SDK 1.x (downgraded from 12 per maintainer review). Workspace at `0.1.0` shipping its first stable SDK is too young to commit to 12-month windows; we'll extend to 12 months at SDK 1.2 if two MINOR releases ship clean. Within the window, an embedder pinning `pi-sdk = "1"` should be able to upgrade to any 1.x without code changes.

**Security exception to back-compat.** Any High+ severity CVE in a stable trait or POD type may force a breaking change inside a MINOR release with 30-day pre-notification on GitHub Security Advisories. PATCH releases get backported to the previous MAJOR for 6 months past the next MAJOR's ship date. SDK reserves a RUSTSEC advisory namespace and ships `SECURITY.md` with a coordinated-disclosure address before Commit E (crates.io publish).

**MSRV bump policy.** Stable Rust − 2 minors is a *floor*, not a target. MSRV bumps within a MINOR for security-fix-grade rustc CVEs are NOT breaking. Otherwise: at most 2 MSRV bumps per year, MINOR releases only, with 30-day pre-announcement on GitHub Discussions.

**Deprecation policy.** Any 1.x deprecation lives for at least 4 MINOR releases (~6 months) before removal in the next MAJOR. Deprecation messages MUST name the replacement and link the migration guide entry. `#[deprecated(since = "1.x", note = "use Y; see migration guide")]` is mandatory.

**Compatibility matrix (CI-generated, NOT hand-maintained).** Each `pi-sdk` MINOR ships with a published table of which underlying-crate versions it pins:

```
pi-sdk 1.2  →  pi-tool-types 1.0, pi-ai 1.4, pi-tools-core 1.1, pi-sandbox 1.2, pi-agent-core 1.3
pi-sdk 1.3  →  pi-tool-types 1.0, pi-ai 1.5, pi-tools-core 1.1, pi-sandbox 1.3, pi-agent-core 1.4
```

The matrix is generated from a `compatibility.toml` by a CI step; the markdown table is regenerated, never hand-edited. Maintenance burden of a hand-edited matrix exceeds the maintainer's capacity (per long-term-maintainer review). A `pi-sdk-canary` test crate (one tiny embedder pinning the previous MINOR) is a required CI job per release, catching API drift the matrix can't see.

**Caret-pin underlying crates.** SDK pins underlying crates with `pi-ai = "1"` (caret), NOT `"=1.4"` (exact). This lets embedders who depend on `pi-ai` directly (e.g., to ship a custom `Provider`) get Cargo's normal version-unification. CI test `tests/dual_dep_unification.rs` builds against both `pi-sdk` alone and `pi-sdk + pi-ai-direct` to catch breakage.

Embedders pin `pi-sdk` only; the matrix is the SDK's responsibility, not theirs.

**Stable surface (committed at 1.0):**

- All trait shapes: `Provider`, `Tool`, `SandboxProvider`, `ProviderFactory`, `ToolGate`, `StreamInterceptor`. Sandbox launcher traits (`MicroVmLauncher`, `RemoteTransport`) ship behind `sandbox-microvm-unstable` / `sandbox-remote-unstable` features until SDK 1.2 (do NOT freeze them at 1.0 while RFDs 0023+0026 are still iterating).
- All POD types: `ToolResult`, `ToolSpec`, `ToolError`, `Message`, `ContentBlock`, `ToolCall`, `Usage`, `ModelInfo`, `ProviderConfig`, `AuthMethod`, `Settings`. **All marked `#[non_exhaustive]`** so MINOR-additive field growth is non-breaking.
- The runtime API: `RuntimeConfig` (builder API only; struct literals banned outside the workspace via `#[non_exhaustive]`, see §4). `AgentSession::send`/`prompt`/`compact`/`abort`, `AgentSessionRuntime::create_session`/`open_session`.
- Streaming events: `StreamEvent`, `StreamEventKind`, `AgentEvent`, `AgentEventKind`. **All marked `#[non_exhaustive]`** at the variant level so adding a new event kind is MINOR-additive.
- `SessionEntry` + `SessionEntryKind` (so external telemetry consumers don't break). **All marked `#[non_exhaustive]`** at the variant level.
- `pi_sdk::Error` (top-level, `thiserror`-based). Stable variant *names* (additive). Source error types may evolve.

**`#[non_exhaustive]` policy: blanket-applied** to every public struct AND every public enum variant in the SDK surface (per maintainer review). Without this, every field addition to `Settings`, `ToolSpec`, `Usage`, etc. is a MAJOR — and this hurts the maintainer constantly.

**`Extensions` escape hatch** (per maintainer review). `GenerateRequest`, `ContentBlock`, `ToolResult` all carry an `extensions: ProviderExtensions` opaque map (`Arc<dyn Any + Send + Sync>` keyed by provider) so providers can add capabilities (extended thinking, prompt caching, computer-use blocks) without trait churn. Each Anthropic / OpenAI quarterly model release adds a new capability; without the escape hatch we eat a 2.0 within 6 months.

**`SessionEntryKind` JSONL wire-format contract** (committed at SDK 1.0):

- The JSONL format is `WireSerializer::serialize(entry) + "\n"`. One entry per line. UTF-8. (`WireSerializer` is a thin wrapper around `serde_json::to_string` that applies the limits below; embedders configure tightening via `WireSerializer::with_limits(...)`.)
- The discriminator is `"kind"` (already the case via `#[serde(tag = "kind", rename_all = "snake_case")]`).
- **Per-field size cap.** `ContentBlock.text`, `ToolResult.model_output`, and any other model-controlled string field is capped at 1 MiB by default. Excess is hard-truncated with a marker `…[N bytes truncated by pi-sdk WireSerializer]`. Cap is configurable via `WireSerializer::with_limits`.
- **ANSI escape sequence stripping.** Any model-controlled string is run through an ANSI-escape filter before serialization. A model emitting `\x1b]0;rm -rf /\x07` cannot make operator terminals tailing the JSONL re-title or worse. Defense against log injection.
- **C1 control range escaping.** `..=` and bidirectional override characters (`U+202A..U+202E`, `U+2066..U+2069`) are explicitly `\uXXXX`-escaped. Defense against trojan-source style attacks via session logs.
- New variants on `SessionEntryKind` are MINOR-additive. **Reader-side tolerance is a convention, not enforced by serde:** `#[serde(rename_all = "snake_case")]` only fixes the discriminator string — it does not make older readers skip unknown variants. Readers MUST be written to tolerate `kind` values they don't recognise. Pi-stats `ingest.rs` uses silent skip-and-continue (`Err(_) => continue`); external consumers can mirror or wire in their own logger via a `deserialize_with` adapter.
- Field renames within an existing variant are MAJOR-breaking. Renaming `provider` → `provider_id`, for example, requires SDK 2.0.
- Optional fields added with `#[serde(default, skip_serializing_if = "Option::is_none")]` are MINOR-additive (existing rows deserialize cleanly because of `#[serde(default)]`; new rows surface the new field). Required fields cannot be added — those are MAJOR.
- New `SessionEntryKind::InterceptorInjection { reminder, source }` variant (per adversarial review) distinguishes synthetic-user messages produced by `StreamInterceptor::AbortAndInject` from real operator input. Auditors can tell forged context from real input.
- `SessionEntryKind::Compaction { replaced_ids }` is validated at append time: IDs must reference real entries, no cycles, no self-reference. Rejection is a write-side error.
- Embedders are expected to tolerate unknown fields (use `#[serde(default)]` on their reader structs or accept that future-pi rows may carry fields they ignore) and unknown `kind` values (skip-and-log pattern). This is a documented contract, not a serde feature.

**Unstable surface (will change in 0.x; may stabilise later):**

- `Compactor` and the LLM compactor's prompt template — likely adds new strategies.
- Auth storage internals — encryption format, file path conventions.
- `BuildConfig` defaults — convenience-only, embedders building their own `RuntimeConfig` directly are unaffected.

**Internal-only (never SDK):**

- `pi-stats` SQLite schema — embedders read JSONL events, not SQL.
- pi-coding-agent's `cli::Cli` struct.
- TUI rendering, slash registry, picker.
- `pi-orchestrate`'s state.jsonl wire format.

### 4. The `RuntimeConfig` field-growth problem

The compiled-agent reviewer flagged this and it's real: `RuntimeConfig` has grown three optional fields in three RFDs (`tool_gate`, `stream_interceptor`, `sandbox_provider`). Every addition is technically MINOR-compatible (the struct is non-`#[non_exhaustive]` and the new fields default to `None` via `BuildConfig`), but each one risks breaking embedders who construct `RuntimeConfig { ... }` literally.

**Decision for SDK 1.0:** mark `RuntimeConfig` as `#[non_exhaustive]`. Embedders use the builder pattern instead of struct literals. Adding a new field becomes a pure-additive MINOR.

**Builder shape** (committed at SDK 1.0; **`ConfigBuilder` does not exist on main yet — it lands in this RFD's Commit B**):

```rust
// crates/pi-agent-core/src/runtime.rs (TO BE IMPLEMENTED IN COMMIT B)

#[derive(Default)]
pub struct ConfigBuilder {
    inner: RuntimeConfigInner,   // private; mirrors RuntimeConfig fields
}

impl RuntimeConfig {
    pub fn builder() -> ConfigBuilder { ConfigBuilder::default() }
}

impl ConfigBuilder {
    // Required fields (build() panics or returns Err if any unset):
    pub fn session_manager(self, m: SessionManager) -> Self { ... }
    pub fn auth_storage(self, a: AuthStorage) -> Self { ... }
    pub fn model_registry(self, r: ModelRegistry) -> Self { ... }
    pub fn tools(self, t: ToolRegistry) -> Self { ... }
    pub fn settings(self, s: Settings) -> Self { ... }
    pub fn system_prompt<S: Into<String>>(self, p: S) -> Self { ... }
    pub fn cwd(self, p: PathBuf) -> Self { ... }

    // Optional plug-ins (None unless set):
    pub fn with_provider_factory(self, f: Arc<dyn ProviderFactory>) -> Self { ... }
    pub fn with_tool_gate(self, g: Arc<dyn ToolGate>, ask_is_approve: bool) -> Self { ... }
    pub fn with_stream_interceptor(self, i: Arc<dyn StreamInterceptor>) -> Self { ... }
    pub fn with_sandbox_provider(self, s: Arc<dyn SandboxProvider>) -> Self { ... }

    // Optional context:
    pub fn with_context_files(self, c: Vec<ContextFile>) -> Self { ... }

    // Terminal:
    pub fn build(self) -> Result<RuntimeConfig, ConfigError>;
    // For tests / quick-start: panics on missing required fields.
    pub fn build_unwrap(self) -> RuntimeConfig;
}
```

The `with_*` methods on the existing `RuntimeConfig` struct (`with_tool_gate`, etc.) on the `impl RuntimeConfig` block in `crates/pi-agent-core/src/runtime.rs` stay usable inside the workspace until 1.0; at 1.0 they are deprecated in favour of the builder. Callers can keep them by chaining off `RuntimeConfig::default()` style (which is what `BuildConfig`'s convenience builder does today).

**Builder-only requirement for embedders:** after SDK 1.0, `pi-sdk` consumers construct `RuntimeConfig` exclusively via `RuntimeConfig::builder()`. The `#[non_exhaustive]` annotation enforces this at the crate boundary — Rust's compiler refuses struct literals from outside the crate. Within `pi-rs`'s own workspace (the binary, tests, internal callers) struct literals remain legal and continue to be used where convenient.

**0.x → 1.0 migration preview for embedders:** today (`pi-sdk = "0.1"`) the example 01 builds via `build_runtime_config(BuildConfig { ... }).with_sandbox_provider(...)` — the fluent `with_*` methods chain off `RuntimeConfig` directly. At 1.0 the same code becomes `RuntimeConfig::builder().sandbox_provider(...).build()` and `BuildConfig` shrinks to a thin wrapper. Embedders should expect to replace `BuildConfig { ... }` literals + `RuntimeConfig { ... }` literals with the builder; everything else (Settings, ToolRegistry, AuthStorage, etc.) stays the same. Migration is a one-time edit when pinning to `pi-sdk = "1"`.

This requires a migration: approximately 25–30 sites in the workspace today do `RuntimeConfig { session_manager, ..., sandbox_provider }`. Exact count is captured during the **Commit B pre-flight audit** (run `rg --files-with-matches 'RuntimeConfig\\s*\\{' crates/`); the actual sites get rewritten to use the builder. Mostly mechanical, ~1 day of work.

### 4.5 Hardening contract (NEW per adversarial review)

The adversarial review of v0.6 found **real bugs in the current pi-rs runtime** that any embedder of pi-sdk would inherit. These are not RFD design issues — they are existing-code defects that ship today. v0.7 names them and commits the SDK to a hardening contract that pi-sdk 0.1 ships with these fixes in place.

#### Runtime invariants

1. **`tool.invoke()` must run inside `AssertUnwindSafe(...).catch_unwind()`.** A custom embedder tool that panics today kills the tokio worker thread; the runtime tracks no recovery. Fix: wrap the invocation, return `ToolError::Panicked`. Same for `provider.stream()`'s next-event task.

2. **Stream-event validation.** `runtime.rs` currently has `last_assistant.unwrap()` and an `(input + output) as u32` truncating cast on token counters. A stream that emits only `Usage { input_tokens: u64::MAX } + Finish` triggers the truncation, producing a context-window underflow that triggers a compaction storm. Fix: validate `ToolCallComplete.id` was previously announced; require `Finish { reason: ToolUse }` to be paired with at least one tool call; replace `unwrap()` with `Err(RuntimeError::EmptyTurn)`; clamp token counters with `saturating_add` into a session-scoped `u128` accumulator; reject `usage` events whose deltas exceed a per-session ceiling.

3. **Per-session call-depth + token-budget guards.** A custom `Tool::invoke` can re-enter `AgentSession::send` recursively. Nothing tracks depth today. Fix: add `RuntimeConfig` fields `max_recursion: usize` (default 8), `max_session_tokens: u64` (default 10_000_000), `max_tool_invocations_per_turn: usize` (default 64). Exceeding any returns `RuntimeError::DepthExceeded` / `BudgetExhausted`. Thread-local counter for depth; per-session for tokens.

4. **`ToolGate` carries session/turn context.** Today `approve(&self, name, &input)` has no session id, no turn index, no parent session. A naïve "approve once, remember the answer" gate is bypassable cross-session. Fix: change to `approve(&self, ctx: GateContext, name: &str, input: &Value) -> ToolGateOutcome` where `GateContext` carries `session_id`, `turn_index`, `parent_session: Option<SessionId>`, `recursion_depth`. Also: `gate_ask_is_approve = true` is TUI-only; document that headless modes MUST set it `false`.

5. **`ToolRegistry::register` rejects duplicate names.** Today `inner.insert(name, tool)` is silent last-write-wins — a malicious dependency `pub fn register_observability(reg)` can transparently shadow `bash` with an exfiltrating impl. Fix: change return type to `Result<(), DuplicateName>`; add `register_or_replace` as the explicit override.

6. **`bash` cwd jail check.** Today `bash::invoke` reads caller-supplied `cwd` from JSON, runs `resolve_path` (which honors `~` and joins relative paths), and shells out — no canonicalization vs `ctx.cwd`, no jail check. The model can `bash {cmd: "id", cwd: "../../../"}` and execute outside the embedder's intended directory. Fix: canonicalize the cwd, reject if it does not have `ctx.cwd` as a prefix.

7. **Per-tool input + output caps.** Today no per-tool input-size limit; bash `timeout_ms` is unbounded; `model_output` from sandbox path is returned untruncated. Fix: default 64 KiB per input field, hard-truncate; cap bash `timeout_ms` at 600 s; honor `ctx.max_output_bytes` on every output path.

8. **`AuthStorage` hardening.** Today `AuthStorage::from_env()` slurps 17 env vars unconditionally. Plain JSON write to disk with no permission discipline (default umask, typically 0644). No tenant scoping, no atomic-rename. Fix:
   - `from_env_explicit(&[(provider, env_key)])` for opt-in env scanning; `from_env()` deprecated + warning at construction;
   - `OpenOptions::new().mode(0o600).create_new(true)` + atomic temp + rename on write;
   - `AuthStorage::scoped(provider_filter)` for per-tenant restriction;
   - `AuthStorage::sealed()` panics on `set` after construction (defense for embedders that want immutable creds post-init).
   
   `BuildConfig::default()` and `pi_sdk::quick_start()` use `AuthStorage::in_memory()` — NO env scan in default constructors. Embedders explicitly call `AuthStorage::from_env_explicit()` if they want it.

9. **`StreamInterceptor` model-content side-channel.** Today no doc-comment about the trust boundary. A "safety filter" that forwards `event.text` to operator-trusted Slack/Sentry is a model-injection vector. Fix: `// SAFETY:` doc on the trait stating "the `event` argument may contain attacker-controlled (model-injected) text. Implementations forwarding `event.text` to operator-visible surfaces MUST mark the content as model-originated and MUST NOT render it as trusted prose." Ship `SafeLogStreamInterceptor` adapter that wraps any logger and prefixes/sanitizes.

10. **Distinguish operator messages from interceptor-injected messages.** Today TTSR-injected messages are indistinguishable from real user input in `SessionEntry`. Fix: new `SessionEntryKind::InterceptorInjection { reminder, source }` variant (or `synthetic_user: bool` flag on `User`). Per-turn cap on TTSR aborts (max 3) before promoting to `RuntimeError::InterceptorThrash`.

11. **JSONL serializer-side limits.** Today `serde_json::to_string(entry) + "\n"` is the contract; no field-size cap, no ANSI-escape stripping, no C1 control-range escaping. A model emitting `]0;rm -rf /` in a content block makes operator terminals tailing JSONL re-title or worse. Fix at SDK 1.0:
   - max field size 1 MiB per `ContentBlock.text`, hard-truncate with marker;
   - ANSI escape sequence stripping in any `text` field passing through serialization;
   - explicit `\uXXXX` escaping for the C1 control range `U+0080..U+009F` and bidi overrides;
   - `WireSerializer` config struct so embedders can tighten further.

12. **Default surface safety renames.** `LocalProcessProvider` → `InProcessExecutor` (or `UnsandboxedExecutor`); the current name implies isolation, the new name does not. `with_extras()` → `with_unsafe_extras()` (full set including bash) plus `with_readonly_extras()` (read/grep/find/ls only); `with_defaults()` becomes the readonly set. The struct/method name itself is the safety signal.

#### What this commits the SDK to

These are not "polish" — pi-sdk 0.1 cannot ship safely without them. They become hard constraints on Commit A1's scope (which is already pure refactor — the hardening lives in **new Commits H1-H6** added to the implementation schedule, see §Implementation schedule).

### 4.6 Conceptual model

A reader landing on the README's first page should be able to picture the moving parts in 30 seconds. Pi-sdk's mental model is **four collaborating actors**:

```
                    ┌────────────────────────────────────────────────────────┐
                    │                  Embedder process                      │
                    │  ┌────────────────────────────────────────────────┐    │
                    │  │            AgentSessionRuntime                 │    │
                    │  │  (owns RuntimeConfig + ProviderFactory +       │    │
                    │  │   ToolRegistry + ToolGate + StreamInterceptor) │    │
                    │  └─────────┬──────────────────────────────────────┘    │
                    │            │ create_session(...) → AgentSession        │
                    │            ▼                                           │
                    │  ┌────────────────────┐  ┌─────────────────────────┐   │
                    │  │   AgentSession     │  │   tokio::mpsc receiver  │   │
                    │  │  ──.send(msg)──▶   │──┼─▶ AgentEvent stream     │   │
                    │  │                    │  │   (TextDelta,           │   │
                    │  │                    │  │    ToolCall, …)         │   │
                    │  └─────────┬──────────┘  └─────────────────────────┘   │
                    │            │                                           │
                    │       ┌────┴────────┬─────────────────┬─────────────┐  │
                    │       ▼             ▼                 ▼             ▼  │
                    │  ┌────────┐  ┌──────────────┐  ┌──────────────┐ ┌────┐ │
                    │  │Provider│  │ToolRegistry  │  │SandboxProvider│ │Auth│ │
                    │  │  (LLM) │  │  (built-in   │  │  (microvm /   │ │Stor│ │
                    │  │        │  │   + custom)  │  │   remote /    │ │age │ │
                    │  │        │  │              │  │   in-process) │ │    │ │
                    │  └───┬────┘  └──────────────┘  └──────┬────────┘ └────┘ │
                    └──────┼─────────────────────────────────┼────────────────┘
                           │                                 │
                           ▼                                 ▼
                  ┌────────────────┐                ┌────────────────┐
                  │  Anthropic /   │                │  Sandbox VM /  │
                  │  OpenAI / etc. │                │  remote sbx /  │
                  │   API endpoint │                │  local procfs  │
                  └────────────────┘                └────────────────┘
```

- **AgentSessionRuntime** is the long-lived hub — built once, holds shared state.
- **AgentSession** is per-conversation — the embedder opens N of them and pipes prompts in.
- **AgentEvent** stream is the embedder's read side. Everything user-visible (text, tool calls, turn boundaries, errors) flows through it.
- **Provider** + **ToolRegistry** + **SandboxProvider** + **AuthStorage** are the four pluggable boundaries — embedders swap them to integrate their own LLM, tools, exec backend, secret store.

This diagram appears on page 1 of the README, before the embed-in-50-lines example.

### 5. Embed-in-50-lines example

The README's first compile-and-run example. The bar is: a Rust developer reads it, recognises every concept, and is confident they could adapt it for their use case in an afternoon.

```rust
//! crates/pi-sdk/examples/01_minimal.rs
//!
//! Embed a pi-rs agent in your own application. The example below
//! sends "list files in this directory and tell me what you see" to
//! an Anthropic-backed agent and prints the streamed response.
//!
//! Run with:
//!     ANTHROPIC_API_KEY=sk-... cargo run --example 01_minimal

use pi_sdk::{
    build_runtime_config, AgentEventKind, AgentSessionRuntime, AuthMethod,
    AuthStorage, BuildConfig, LocalProcessProvider, Settings, ToolRegistry,
};
use std::sync::Arc;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // 1. Wire up auth from env. Anthropic / OpenAI / Google auto-detected.
    let auth = AuthStorage::from_env();

    // 2. Build a runtime config with the default tool set + a real sandbox.
    let cfg = build_runtime_config(BuildConfig {
        auth: auth.clone(),
        // Convenience for the demo: `with_extras()` registers eight tools —
        // read/write/edit/bash (the defaults) plus grep/find/ls/web_search.
        // `with_defaults()` is the leaner file/process-only set (read/write/
        // edit/bash). Note: `monitor` and `task` are NOT in either helper —
        // they live binary-side in pi-coding-agent (see §2.5).
        // **Production agents should call `ToolRegistry::new()` and register
        // tools explicitly** so the tool surface is auditable. See Open
        // Question #1 for the canonical tool-registration patterns.
        tools: ToolRegistry::with_extras(),
        settings: Settings {
            provider: std::env::var("PI_PROVIDER")
                .unwrap_or_else(|_| "anthropic".into()),
            // Override via env var PI_MODEL for a different model.
            model: std::env::var("PI_MODEL")
                .unwrap_or_else(|_| "claude-haiku-4-5-20251001".into()),
            ..Settings::default()
        },
        ..BuildConfig::default()
    })
    .with_sandbox_provider(Arc::new(LocalProcessProvider::with_defaults()));

    // 3. Open a session.
    let runtime = AgentSessionRuntime::new(cfg);
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    let session = runtime.create_session(Some(tx))?;

    // 4. Send a prompt. Stream events back.
    tokio::spawn(async move {
        session
            .prompt("List files in the current directory and summarise.".into())
            .await
            .ok();
    });

    while let Some(event) = rx.recv().await {
        match event.kind {
            AgentEventKind::AssistantTextDelta { text } => print!("{text}"),
            AgentEventKind::TurnComplete => break,
            _ => {}
        }
    }

    Ok(())
}
```

50 lines (with comments). Compiles against `pi-sdk = "0.1"`. Runs end-to-end with a real provider.

> **Note on event-variant names:** The minimal example above uses `AssistantTextDelta` — the actual variant on `pi_agent_core::AgentEventKind` today (`crates/pi-agent-core/src/event.rs:18`). The full set is `SessionStarted`, `UserMessage`, `AssistantStart`, `AssistantTextDelta`, `AssistantThinkingDelta`, `AssistantToolCall { call }`, `ToolResult`, `AssistantMessage`, `Usage { usage }`, `TurnComplete`, `Error { message }`, `Aborted`, plus compaction + monitor events. The SDK 1.0 surface freezes these variant names and shapes; v0.7 (this RFD) does NOT propose renaming them. Embedders consuming the stream match on these exact identifiers.

### 5b. Realistic embed shape (per UX review)

The minimal example is for the README's first impression. **`examples/02_realistic_ci_agent.rs`** shows the shape every real embedder converges to: tool-event surfacing, cost tracking, structured errors, cancellation. It is the example linked from the "Production checklist" (§7).

```rust
//! crates/pi-sdk/examples/02_realistic_ci_agent.rs
//!
//! What an actual embedder writes once they get past Hello World:
//! - surface tool calls (operators want to see what the agent is doing);
//! - track cost per turn and abort when a budget cap is hit;
//! - turn pi_sdk::Error variants into the embedder's own error type;
//! - cancel on Ctrl-C without leaking the session.

use pi_sdk::{
    cost::{estimate_cost_usd, CostRegistry},
    AgentEventKind, AgentSessionRuntime, AuthStorage, Error, InProcessExecutor,
    RuntimeConfig, Settings, ToolRegistry,
};
use std::sync::Arc;
use tokio::sync::mpsc;

const BUDGET_USD: f64 = 0.50;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // 1. Explicit auth — no env scan in production. Embedder names the keys it trusts.
    //    `from_env_explicit` ships in Commit H5 (Hardening §4.5 #8); pre-H5 embedders
    //    must call `AuthStorage::set` manually.
    let auth = AuthStorage::from_env_explicit(&[
        ("anthropic", "MY_CI_ANTHROPIC_KEY"),
    ])?;

    // 2. Builder API. All seven required setters present (see §4 contract):
    //    session_manager, auth_storage, model_registry, tools, settings,
    //    system_prompt, cwd. Optional plug-ins use `with_*`.
    // ModelRegistry currently requires AuthStorage; AuthStorage is Clone.
    let cfg = RuntimeConfig::builder()
        .session_manager(pi_sdk::SessionManager::in_memory())
        .auth_storage(auth.clone())
        .model_registry(pi_sdk::ModelRegistry::new(auth.clone()))
        .tools(ToolRegistry::with_readonly_extras())   // no shell, no fs writes.
        .settings(Settings {
            provider: "anthropic".into(),
            model: "claude-haiku-4-5-20251001".into(),
            ..Settings::default()
        })
        .system_prompt("You are a CI inspector. Read the repo, do not modify it.")
        .cwd(std::env::current_dir()?)
        .with_sandbox_provider(Arc::new(InProcessExecutor::with_readonly_defaults()))
        .build()?;

    let runtime = AgentSessionRuntime::new(cfg);
    let registry = CostRegistry::default();   // best-effort price table; embedder can override.
    let model_id = "claude-haiku-4-5-20251001".to_string();

    let (tx, mut rx) = mpsc::unbounded_channel();
    let session = Arc::new(runtime.create_session(Some(tx))?);

    // 3. Cancellation: Ctrl-C aborts the in-flight turn cleanly.
    //    `AgentSession::abort` is async and idempotent.
    let abortable = session.clone();
    tokio::spawn(async move {
        let _ = tokio::signal::ctrl_c().await;
        eprintln!("\n[ctrl-c] aborting session");
        abortable.abort().await;
    });

    // 4. Send a CI-shaped task.
    let prompt = "Inspect the current directory and list any failing test files.".to_string();
    let prompt_session = session.clone();
    tokio::spawn(async move { let _ = prompt_session.prompt(prompt).await; });

    let mut turn_cost: f64 = 0.0;
    while let Some(event) = rx.recv().await {
        match event.kind {
            AgentEventKind::AssistantTextDelta { text } => print!("{text}"),
            AgentEventKind::AssistantToolCall { call } => {
                eprintln!("\n[tool] {}", call.name);
            }
            AgentEventKind::Usage { usage } => {
                // model_id is captured at session-init; not on the event itself.
                turn_cost += estimate_cost_usd(&usage, &model_id, &registry);
                if turn_cost > BUDGET_USD {
                    eprintln!("\n[budget] turn cost ${turn_cost:.4} > cap ${BUDGET_USD}; aborting");
                    session.abort().await;
                }
            }
            AgentEventKind::TurnComplete => break,
            AgentEventKind::Error { message } => {
                // `Error` in the event stream is a String today (see RFD §3 stable surface).
                // Embedders catching `pi_sdk::Error` from `session.prompt()` return value
                // get the structured variants — see the inner `match` pattern below.
                return Err(format!("agent error: {message}").into());
            }
            _ => {}
        }
    }

    eprintln!("\n[cost] turn total: ${turn_cost:.4}");

    // Structured-error pattern (illustrative; real code awaits the prompt JoinHandle).
    fn _classify(err: Error) -> &'static str {
        match err {
            Error::Provider(_)            => "upstream LLM failed",
            Error::Sandbox(_)             => "sandbox unavailable",
            Error::Tool(_)                => "tool invocation rejected",
            Error::BudgetExhausted { .. } => "session budget exhausted",
            Error::DepthExceeded { .. }   => "tool recursion depth exceeded",
            _                             => "other",
        }
    }
    Ok(())
}
```

This is the example most embedders will copy first. ~110 lines. Compiles against `pi-sdk = "0.1"` after Commits A through H7 land (the renames + readonly constructors come from H7; the builder ships in B; structured `Error` ships in C). Pre-H7 versions of this example use the legacy names noted in the v0.6 → v0.7 changelog.

Additional examples in the same directory:

- **02_custom_tool.rs** — implement `Tool` for a domain-specific tool, register it.
- **03_custom_provider.rs** — implement `Provider` for a hypothetical new LLM service; route through `ProviderFactory`.
- **04_custom_sandbox.rs** — implement `SandboxProvider` for a private execution backend.
- **05_event_streaming.rs** — show `StreamInterceptor` for time-travelling reminders (TTSR) or custom logging.

Each example is ~100 lines, self-contained, runnable.

### 6. Distribution

**Crates.io.** `pi-sdk` is the only crate that pi-rs publishes for SDK consumers. The underlying crates (`pi-ai`, `pi-tools`, etc.) MAY be published as well so embedders can override individual deps if they really need to, but the recommended path is `cargo add pi-sdk` and nothing else.

**Versioning on crates.io:**

- `pi-sdk = "0.1"` (resolves to latest 0.1.z) → pre-1.0 experimental.
- `pi-sdk = "1"` (resolves to latest 1.x) → 1.0+ stable.
- Strict pinning (`pi-sdk = "=1.2.0"`) supported but not required within a 1.x window.

**Documentation:** docs.rs auto-builds from each crates.io release. `pi-sdk`'s docs become the canonical embedder reference.

**Release cadence:** initially erratic (whenever pi-rs has a milestone). Post-1.0, target a regular MINOR every ~6 weeks aligned with a meaningful feature.

**Underlying-crate version policy:** `pi-sdk`'s `Cargo.toml` pins underlying crates with caret ranges (`pi-ai = "1"`, `pi-tools-core = "1"`, etc.) — NEVER exact pins (`"=1.4.0"`). Embedders who depend on `pi-ai` directly (to ship a custom `Provider`) need Cargo's normal version-unification to work. A required CI test (`tests/dual_dep_unification.rs`, Track 3 Commit G) builds `pi-sdk` once standalone and once alongside `pi-ai = "1"` direct, catching pinning-induced duplication.

**Supply-chain threat note.** Each `pi-sdk` MINOR ships `cargo-audit` clean against the published advisory database, plus `cargo-deny` configured to reject (a) duplicate transitive deps differing in MAJOR, (b) unmaintained crates flagged by RustSec, (c) GPL/AGPL deps (license incompatibility). Embedders running in compliance-sensitive environments are still expected to run their own `cargo audit` + `cargo deny` against their resolved lockfile — the SDK's clean status verifies *its own* deps, not the embedder's full transitive tree.

**Migration guide format.** Each MAJOR ships `MIGRATION.md` in the crate root with: (a) one diff-style snippet per renamed/removed symbol, (b) a "search-and-replace" table of the form `old_path::OldName  →  new_path::NewName`, (c) a "behavior change" callout per non-mechanical migration. The migration guide is a hard release blocker — no MAJOR ships without it.

## Implementation schedule

Twelve commits across three tracks. Per multi-personality review (v0.7), the original A-F was insufficient: it shipped a façade without the hardening that makes the surface safe-by-default and without the embedder ergonomics (mocks, error type, cost helper) that real adoption needs.

### Track 1 — Façade and core ergonomics (was A-F, expanded)

| # | Commit | Est. LoC |
|---|---|---|
| **A** | New crate `crates/pi-sdk/` with `Cargo.toml` (incl. **feature-flag matrix from §1**) + `src/lib.rs` re-exports + workspace registration. Move `BuildConfig` + `build_runtime_config` from `pi-coding-agent::sdk` into `pi-sdk::build`. Keep deprecated shim in `pi-coding-agent::sdk`. | 250 |
| **B** | `RuntimeConfig` builder + **blanket `#[non_exhaustive]` audit pass on every public struct + enum variant**. Migrate ~25-30 struct-literal sites. | 500 |
| **C** | **Top-level `pi_sdk::Error` enum** + `Result` type alias. `thiserror`-based, with `#[from]` for each underlying crate's error. Every public function returns `Result<_, pi_sdk::Error>`. | 300 |
| **D** | **`MockProvider` + `MockSandboxProvider`** in public surface, gated on `mocks` feature. Plus 5 `examples/` files showing realistic shapes (not "Hello World"): tool-event surfacing, cost tracking, cancellation, structured errors, custom tool. | 600 |
| **E** | **`pi-sdk::cost`** module: `CostRegistry` + `estimate_cost_usd(usage, model_info)` helper. Per-session and per-tool-call rollup helpers from `SessionEntry` stream. | 250 |
| **F** | `pi-sdk` README + `docs/` + **doc-tested via `#![doc = include_str!("../README.md")]`**. README structure: one-line pitch → 0.x warning → 50-line example → "what you get / don't get" → Production checklist (concrete contents, not TODO). Conceptual model ASCII diagram. | 600 (markdown) |

### Track 2 — Hardening (NEW per adversarial review; pre-1.0 hard gate)

| # | Commit | Est. LoC |
|---|---|---|
| **H1** | `tool.invoke()` wrapped in `catch_unwind`; `provider.stream()` next-event in same. Replace all `.unwrap()` paths in `runtime.rs` with explicit `RuntimeError::*` variants (audit pass). | 300 |
| **H2** | Stream-event validation invariants (validate `ToolCallComplete.id` was previously announced; `Finish { reason: ToolUse }` requires ≥1 tool call; saturating-add token counters; reject events with deltas > per-session ceiling). Plus per-session **call-depth + token-budget guards** (`max_recursion`, `max_session_tokens`, `max_tool_invocations_per_turn` on `RuntimeConfig`). | 400 |
| **H3** | `ToolGate::approve` signature change to take `GateContext { session_id, turn_index, parent_session, recursion_depth }`. `ToolRegistry::register` returns `Result<(), DuplicateName>`. `gate_ask_is_approve = true` documented as TUI-only with a debug-mode warning when used headlessly. | 300 |
| **H4** | `bash` tool **cwd jail check** (canonicalize, reject if not under `ctx.cwd`); per-tool input size cap (64 KiB default, hard-truncate); honor `ctx.max_output_bytes` on every output path including the sandbox return path; cap `bash.timeout_ms` at 600 s. | 250 |
| **H5** | `AuthStorage` hardening: `from_env_explicit(allowlist)`, `scoped(filter)`, `sealed()`. `OpenOptions::new().mode(0o600).create_new(true)` + atomic temp + rename on write. `from_env()` deprecated + warning. `BuildConfig::default()` and `pi_sdk::quick_start()` use `in_memory()` (no env scan). | 350 |
| **H6** | `WireSerializer` for JSONL (per-field 1 MiB cap + ANSI escape stripping + C1/bidi escaping). `SessionEntryKind::InterceptorInjection` variant. `Compaction.replaced_ids` validated at append. `StreamInterceptor` SAFETY doc + `SafeLogStreamInterceptor` adapter. | 400 |
| **H7** | **Default-surface safety renames** (Hardening §4.5 #12). `LocalProcessProvider` → `InProcessExecutor` in `crates/pi-sandbox/src/local.rs` with deprecation alias re-exporting the old name (removed at SDK 1.0). `with_extras()` → `with_unsafe_extras()` (full set, name itself is the warning) plus new `with_readonly_extras()` (read/grep/find/ls only, no bash, no fs writes) on `ToolRegistry`. `with_defaults()` becomes the readonly set. New constructor `InProcessExecutor::with_readonly_defaults()` returning a sandbox provider that refuses any `tool != read/ls/grep/find`. Plus `pi_sdk::quick_start(provider, model) -> Result<AgentSessionRuntime, Error>` convenience that wires `AuthStorage::in_memory()` + `ToolRegistry::with_readonly_extras()` + `InProcessExecutor::with_readonly_defaults()` for first-touch demos. | 350 |

### Track 3 — Distribution and CI

| # | Commit | Est. LoC |
|---|---|---|
| **G** | `pi-sdk-canary` test crate (pins prev MINOR), `compatibility.toml` + matrix-gen script, `tests/dual_dep_unification.rs`. Required CI per release. | 250 |
| **I** | `cargo-semver-checks` + `cargo-audit` + `cargo-deny` CI gates. SECURITY.md + RUSTSEC namespace + coordinated-disclosure address. | 100 + scripts |
| **J** | docs.rs publish + first 0.1.0 release to crates.io. License decision (MIT/Apache-2.0 dual; OQ#7) made before this commit. | scripts |
| **K** | Migration: pi-coding-agent switches to `pi_sdk::*`. Remove deprecated shim from A. | 150 |

**Total: ~4600 LoC + docs + scripts.** Track 1 ≈ 1900 (excl. F's 600 markdown), Track 2 ≈ 2350 (incl. H7 350), Track 3 ≈ 350 (G 250 + I 100 + J/K scripts + 150 K). Up from v0.6's 1600 estimate (which was unsafe-by-default and missed embedder ergonomics).

**First deliverable** = Commits A through J merged (i.e. SDK 0.1.0 published to crates.io with hardening contract in place + `pi-sdk-canary` green + supply-chain CI clean). Commit K (binary migration) is post-deliverable cleanup. The user's "loop until first deliverable" instruction targets J as the milestone.

The SDK becomes pre-1.0 stable with Commit J. The 1.0 freeze waits for:

1. RFD 0023 (microvm sandbox) lands — `MicroVmLauncher` and `MicroVmProvider` join the public surface.
2. RFD 0026 (remote sandbox) lands — `RemoteTransport` and `RemoteProvider` join the public surface.
3. At least one external embedder builds something non-trivial against pi-sdk 0.x and reports back. Without external dogfood, "stable surface" is just intent.
4. RFD 0028 (compiled agents) prototype proves the codegen target is right.

Estimated 1.0 timeline: 8–12 weeks after this RFD ships. v0.x can ship in 2 weeks.

## 7. Production checklist (embedder safety contract)

This is the README's "before you go to prod" page. Distilled from the threat model (§Threat model and trust boundaries) and the hardening contract (§4.5). Embedders who tick every item are safe-by-default; embedders who don't are knowingly opting out.

### Authentication & secrets

- [ ] **Do NOT call `AuthStorage::from_env()` in production.** Use `AuthStorage::from_env_explicit(&[("anthropic", "MY_TENANT_ANTHROPIC_KEY"), ...])` so the env-var allowlist is auditable. (Background: `from_env()` slurps 17 vars unconditionally — CWE-526 risk.)
- [ ] If running multi-tenant, use `AuthStorage::scoped(provider_filter)` to deny cross-tenant credential bleed.
- [ ] If credentials should not change after init, use `AuthStorage::sealed()`.
- [ ] On-disk auth files are written with `0o600` + atomic rename; verify your filesystem honors POSIX modes (NFS sometimes doesn't).

### Tool surface

- [ ] **Never use `ToolRegistry::with_unsafe_extras()` (post-H7; `with_extras()` pre-H7) in production.** It registers `bash`, which is a code-execution gateway. Use `ToolRegistry::new()` and register tools explicitly so the surface is auditable. The safe alternative for read-only inspection is `with_readonly_extras()`.
- [ ] If you need shell, register `bash` only with a `SandboxProvider` that actually isolates (microvm or remote — `LocalProcessProvider` runs in your process namespace).
- [ ] Custom `Tool` implementations must be panic-safe: the runtime catches panics (Hardening §4.5 #1) but a panicking tool returns `ToolError::Panicked` on every call until you fix it.
- [ ] Audit each custom tool's `invoke()` for: input validation, output truncation respecting `ctx.max_output_bytes`, no unscoped filesystem access outside `ctx.cwd`.

### Tool-gate policy

- [ ] Implement `ToolGate::approve` instead of using `gate_ask_is_approve = true` (which is TUI-only — debug-mode warning fires headlessly).
- [ ] Use `GateContext.session_id` + `parent_session` to scope approvals correctly (don't approve once and remember the answer cross-session).
- [ ] Set `RuntimeConfig::max_recursion`, `max_session_tokens`, `max_tool_invocations_per_turn` to values appropriate for your workload (defaults: 8 / 10M / 64).

### Sandbox

- [ ] In production, use a real sandbox: `MicroVmProvider` (Linux, RFD 0023) or `RemoteProvider` (cross-platform, RFD 0026). `LocalProcessProvider` (renamed `InProcessExecutor` per Hardening §4.5 #12) runs in your process — use only for local dev or trusted-content tasks.
- [ ] Set `VmCeiling` / `CallLimits` on the sandbox provider to bound per-call CPU/memory/walltime; don't rely on the agent's `bash.timeout_ms` alone (capped at 600 s by Hardening §4.5 #7).

### Streaming & observability

- [ ] Treat any string from `AgentEvent::TextDelta` and `ContentBlock.text` as **untrusted attacker-controlled text**. Do NOT render it as trusted prose to operators. Use `SafeLogStreamInterceptor` (ships in `pi-sdk` per Hardening §4.5 #9) when forwarding to Slack/Sentry/PagerDuty.
- [ ] If you tail `SessionEntry` JSONL, use `WireSerializer`'s default limits (1 MiB/field, ANSI strip, C1/bidi escape) — don't roll your own `serde_json::to_string` path.
- [ ] When auditing TTSR-injected messages, filter on `SessionEntryKind::InterceptorInjection` to separate synthetic from real operator input.

### Cost & budget

- [ ] Track `AgentEvent::Usage` and call `pi_sdk::cost::estimate_cost_usd(usage, model_id, &registry)` per turn.
- [ ] Set a per-session budget cap and call `session.abort()` when exceeded — see `examples/02_realistic_ci_agent.rs` for the pattern.
- [ ] Override `CostRegistry` defaults if your contract pricing differs from public list prices.

### Errors

- [ ] Match on `pi_sdk::Error` variants explicitly (don't rely on `Display` strings — they may change). Variant *names* are stable per §3; `Display` is not.
- [ ] Distinguish `BudgetExhausted` and `DepthExceeded` (operator-recoverable: bump caps and retry) from `Provider`/`Sandbox` (likely environmental; backoff + retry).

### Supply chain

- [ ] Run `cargo audit` and `cargo deny` against your resolved lockfile every release.
- [ ] Pin `pi-sdk = "1"` (caret) — accept MINOR bumps automatically, hold MAJOR for migration review.
- [ ] Each `pi-sdk` MINOR upgrade IS a supply-chain event. Read the changelog and `MIGRATION.md` even within a MINOR — security-CVE backports may force an unexpected breaking change inside MINOR (§3 escape hatch).

### Subscribe

- [ ] Subscribe to GitHub Security Advisories on the `pi-rs` repo.
- [ ] Subscribe to GitHub Discussions for MSRV-bump pre-announcements.

## Out of scope / deferred

- **Python / Node / WASM bindings.** Out of scope for this RFD. If they happen, they wrap `pi-sdk`. No constraints on `pi-sdk`'s shape from FFI requirements.
- **Pi-stats embedder hooks.** Embedders can read JSONL session entries. The SQLite ingest + dashboard remain pi-binary territory.
- **Deprecation of `pi-coding-agent::sdk`.** During 0.x both modules coexist (the binary uses the new path; old embedders see a deprecation warning). The shim is removed in Commit F.
- **Stable serde representations.** `SessionEntry` JSONL is the wire format embedders depend on; serde for `RuntimeConfig` etc. is convenience-only and may change.
- **MCP server adapters.** Out of scope here; `pi-coding-agent` already loads MCP servers, and that wiring is binary-side. A future RFD can add an MCP trait to `pi-sdk` if the demand surfaces.

## Open questions

1. **Should `pi-sdk` re-export all pi-tools tools, or require embedders to register them explicitly?** Today (pre-H7) `ToolRegistry::with_defaults()` registers four tools (read/write/edit/bash); `ToolRegistry::with_extras()` registers eight (defaults + grep/find/ls/web_search). The `monitor` and `task` tools are binary-side (registered by `pi-coding-agent` startup, not by `pi-tools`); SDK embedders never see them by default. Both helpers are re-exported. **Canonical guidance:** demo / quick-start examples use `with_unsafe_extras()` post-H7 (richest set, fastest path, name itself signals risk); production agents call `ToolRegistry::new()` and register tools explicitly so the tool surface is auditable; embedders who want a leaner safe surface call `with_readonly_extras()` (post-H7) which has no shell and no fs writes. **Post-H7 the helper renames flip** (`with_defaults()` becomes the readonly set; `with_extras()` becomes `with_unsafe_extras()`); see Hardening §4.5 #12 for the rename rationale. Document this in the README's "Production checklist."
2. **Feature flags or `--no-default-features`?** Likely yes for: `pi-sdk` default features = `[default-tools, default-providers, anthropic, openai]`; embedders who want a leaner build disable the providers they don't use. v0.1 ships without features (everything included); features land before 1.0.
3. **`async-trait` vs native async traits?** Current pi-rs uses `async-trait`. Native async traits stabilised in Rust 1.75 but have ergonomic issues with trait objects (no `dyn Provider`). v1.0 sticks with `async-trait`; revisit at v2.0 once the Rust ecosystem catches up.
4. **What's the SDK's MSRV?** Today's pi-rs MSRV is implicit. SDK 1.0 must declare one. Recommendation: track stable Rust − 2 minors (i.e., always at least 6 months old) so embedders aren't forced onto bleeding-edge toolchains.
5. **Should `pi-sdk` expose `ProviderFactory` defaults or require embedders to provide one?** `DefaultProviderFactory` is the natural choice; export it. Embedders override only when they want to inject mocks (test) or custom providers.
6. **Telemetry opt-in/out for embedded agents.** The runtime today writes JSONL session entries unconditionally if a `SessionManager` is configured. Embedders may not want disk I/O. Decision: `SessionManager::in_memory()` already exists and is the no-disk path. Document it as the "no telemetry" choice.
7. **License.** pi-rs is currently unlicensed (workspace `Cargo.toml` has no `license` field). Pre-1.0 SDK release requires picking one. Recommendation: MIT or Apache-2.0 (dual-license is the Rust ecosystem norm). Decide before Commit E.
8. **Crate name collision check.** `pi-sdk` on crates.io — verify availability before publishing. If taken, fallback name: `pirs-sdk` or `pi-rs-sdk`.
9. **HMAC-chained `entry_seq` on JSONL?** Adversarial review suggested cryptographic chaining of session entries so audit logs can detect tampering. Deferred from SDK 1.0 — adds a key-management dimension that doesn't fit a single-process SDK; revisit at SDK 1.2 with a `WireSerializer::with_audit_chain(key)` opt-in if a compliance-sensitive embedder asks. For now, embedders requiring tamper-evidence write JSONL into a WORM bucket or sign batches at log-shipping time.
10. **Native async traits when stable in trait objects.** Open Question #3 already covers `async-trait`; #10 is the forward pointer: when Rust ecosystem support for `dyn Trait` with native async lands (likely 2026/2027), revisit `Provider`, `Tool`, `SandboxProvider` shapes. Deferred from 1.0 because a half-migration breaks more than it helps.
11. **Pluggable telemetry sinks.** Today the SDK writes JSONL via `SessionManager`; embedders who want OpenTelemetry / Honeycomb / Datadog spans must transform JSONL themselves. Open question for SDK 1.2: should `pi-sdk` ship a `TelemetrySink` trait with adapters for OTel + tracing? Defer to gauge demand; Hot path is the JSONL stream, not adapter ergonomics.
12. **Embedder-supplied `tokio` runtime contract.** Today the SDK assumes the caller has a tokio runtime. Should the SDK document tokio as a hard dep, or expose a runtime-agnostic core (smol/async-std)? Pragma: 1.0 documents `tokio = "1"` as a hard dep; revisit if a non-tokio embedder lobbies. Switching async runtime mid-stream is a major undertaking.

## Testing strategy

### Unit tests

- All re-exports compile (`cargo check -p pi-sdk`).
- `BuildConfig::default()` produces a runnable config (in-memory session, no auth → no provider; the config builds, doesn't run).
- `RuntimeConfig::builder()` exhaustively sets every field; verify equivalence to the old struct-literal path.

### Integration tests

- Each example in `examples/` compiles under `cargo build --examples`.
- `01_minimal.rs` runs end-to-end against a `MockProvider` (don't hit a real LLM in CI; gate the real-provider variant on a `PI_SDK_E2E=1` env var that the maintainer runs locally before each release).
- `02-05_*.rs` examples each have a corresponding `tests/example_NN.rs` that exercises the example's code path against mocks.

### Doc tests

- Every public type in `lib.rs` re-exports gets a doc comment with one usage example. `cargo test --doc -p pi-sdk` runs them all.

### Stability tests (post-1.0)

- A `cargo-semver-checks` CI step rejects MINOR/PATCH releases that include breaking changes.
- A "compatibility canary" — a minimal embedder crate in `tests/canary/` that pins to the previous SDK MINOR. Each release verifies the canary still builds.

## Revision history

- **v0.7 (2026-05-02):** Multi-personality review pass synthesizing 4 reviewer personas (security / embedder-UX / long-term maintainer / adversarial). Substantive additions:
  - **Threat model section** explicitly enumerates what pi-sdk defends against, what it explicitly does NOT defend against (supply-chain, compromised LLM endpoint, attacker-controlled embedder env), and the shared-with-embedder boundary.
  - **Hardening contract (§4.5, NEW)** — 12 numbered runtime invariants encoding bugs the adversarial review found in the *current pi-rs runtime* (not RFD design issues). Each invariant maps to a hardening commit in Track 2 (H1-H6). Includes: `catch_unwind` around tool invocation, stream-event validation (replaces `last_assistant.unwrap()` panic path), per-session call-depth + token-budget guards, `ToolGate` carrying session/turn context, `ToolRegistry::register` rejecting duplicate names, `bash` cwd jail check, per-tool input/output caps, `AuthStorage` 0o600 + atomic rename + `from_env_explicit` opt-in, `StreamInterceptor` model-content trust-boundary doc, `SessionEntryKind::InterceptorInjection` variant, `WireSerializer` JSONL hardening, default-surface safety renames (`LocalProcessProvider` → `InProcessExecutor`, `with_extras()` → `with_unsafe_extras()` / `with_readonly_extras()`).
  - **Feature-flag matrix in §1** — moved from "before 1.0" to ship at 0.1. `default = ["provider-anthropic", "tools-readonly"]` is the safe-by-default baseline; embedders explicitly opt into `tools-process`, `provider-bedrock`, etc.
  - **Top-level `pi_sdk::Error`** (per UX review) — single `thiserror`-based facade with `#[from]` impls, replacing per-crate error types bubbling through `anyhow` chains.
  - **`pi_sdk::cost`** (per UX review) — `CostRegistry` + `estimate_cost_usd` so every embedder doesn't write the same per-model price table.
  - **`pi_sdk::mocks`** (gated, per UX review) — `MockProvider` + `MockSandboxProvider` in the public surface so embedders can write zero-LLM-cost CI tests.
  - **`#[non_exhaustive]` blanket policy** (per maintainer review) — applied to every public struct AND every public enum variant. Makes field/variant addition non-breaking.
  - **`ProviderExtensions` opaque escape hatch** on `GenerateRequest`/`ContentBlock`/`ToolResult` — providers add capabilities (extended thinking, prompt caching) without trait churn.
  - **6-month back-compat window** (down from 12 in v0.6) + security-CVE escape hatch within MINOR with 30-day pre-notification + MSRV bump policy (≤2/year, MINOR only) + deprecation policy (≥4 MINOR before removal).
  - **Sandbox launcher traits gated `*-unstable`** until SDK 1.2 — don't freeze `MicroVmLauncher` / `RemoteTransport` while RFDs 0023+0026 are still iterating.
  - **JSONL wire-format hardening** — `WireSerializer` (1 MiB/field default cap, ANSI escape strip, C1/bidi escaping). Documented as defense against log-injection / trojan-source attacks via session logs.
  - **Caret-pin underlying crates** (per maintainer review) — `pi-ai = "1"`, NOT `"=1.4"`. CI test `dual_dep_unification` catches breakage.
  - **§4.6 Conceptual model** ASCII diagram for README page 1.
  - **Realistic-shape example 02_realistic_ci_agent.rs** added to §5b — shows tool-event surfacing, cost tracking, structured errors, cancellation. `examples/01_minimal.rs` remains the brochure.
  - **§7 Production checklist** — explicit safety contract embedders tick before shipping.
  - **§6 Distribution** — caret-pin policy + supply-chain threat note + migration-guide-as-release-blocker.
  - **Implementation schedule** — restructured from 6 commits (A-F) to 12 commits across 3 tracks: Track 1 (A-F) façade + ergonomics, Track 2 (H1-H6) hardening, Track 3 (G, I, J, K) distribution + CI. Total ~4250 LoC, up from v0.6's 1600 (which was unsafe-by-default and missed embedder ergonomics). First deliverable explicitly = Commit J (0.1.0 published).
  - **Open questions** extended with items 9-12: HMAC entry_seq chaining (deferred to 1.2), native async traits (deferred), pluggable telemetry sinks (deferred), tokio runtime contract (1.0 documents tokio as hard dep).
  - **Post-multi-personality critic confirmation pass closed 8 deltas:** (1) `AgentEventKind` variants in examples 01 + 02 corrected to actual variant names (`AssistantTextDelta`, `AssistantToolCall { call }`, `Usage { usage }`, `Error { message }` — verified against `crates/pi-agent-core/src/event.rs:8-67`); (2) example 02 rewritten to use `AgentSession::abort().await` (async, idempotent) instead of fictional `abort_handle()` + sync `abort()`; (3) example 02 builder now sets all seven required fields per §4 contract (`session_manager`/`auth_storage`/`model_registry`/`tools`/`settings`/`system_prompt`/`cwd`); (4) **new Commit H7** added to Track 2 owning the §4.5 #12 default-surface renames (`LocalProcessProvider` → `InProcessExecutor`, `with_extras` → `with_unsafe_extras`/`with_readonly_extras`) and the new `with_readonly_defaults` constructor; (5) `pi_ai::EventStream` re-export added as a Pre-Commit-A prerequisite (one-line `pub use provider::EventStream` in `pi-ai/src/lib.rs:37`); (6) `pi_sdk::quick_start(provider, model)` defined in §1 lib.rs surface and bound to Commit H7; (7) §4 line citation softened to "the `impl RuntimeConfig` block" instead of stale line range; (8) implementation-schedule LoC total restated as ~4600 (was ~4250) with track-by-track breakdown.
- **v0.6 (2026-05-02):** Fifth `rfd-critic` pass closed three codebase-fact deltas: (1) §2.5 task-tool citation moved from `executor.rs:183` (which is re-registration for subagents) to `startup.rs:303` (the primary host-side registration); (2) §2.5 MCP claim corrected — pi-rs ships no MCP loader code (verified via grep across `crates/pi-coding-agent/`); embedders bridge via `Tool` impl, no MCP surface in `pi-sdk` 1.0; (3) §4 ConfigBuilder code block annotated "TO BE IMPLEMENTED IN COMMIT B" so readers know it's the proposed shape, not what's on main today.
- **v0.5 (2026-05-02):** Fourth `rfd-critic` pass closed three codebase-fact deltas v0.4 introduced: §2.5 task-tool location (now correctly says binary-side, not in `pi-tools`); example 01 + OQ#1 tool-list correction (`with_extras()` actually registers 8 tools — read/write/edit/bash/grep/find/ls/web_search; no `monitor` and no `task`); §1 dep clause softened to "`pi-tools` (or `pi-tools-core` post-RFD-0023-A2)". The fourth v0.4 critic claim — that `LocalProcessProvider::with_defaults()` doesn't exist — was incorrect; the constructor is at `crates/pi-sandbox/src/local.rs:37`, no fix needed.
- **v0.4 (2026-05-02):** Third `rfd-critic` pass closed remaining 4 deltas: (1) ConfigBuilder type now sketched in §4 with full method signatures (was: "builder API" claimed but never defined); (2) §3 ingest.rs citation corrected to "silent skip-and-continue with `Err(_) => continue`" — v0.3 said "logging + skipping" which is wrong (current ingest.rs has no log call on bad rows); (3) added explicit 0.x → 1.0 migration preview after the builder definition so embedders see what they'll need to change at 1.0; (4) Open Question #1 reworded to give canonical guidance per use case (`with_extras` for demo, `with_defaults` for lean prod, `ToolRegistry::new()` + explicit registration for auditable prod). Plus: softened Flue URL/handle claim — added "verify before publishing; project is at v0.3.x and may move" note.
- **v0.3 (2026-05-02):** Second `rfd-critic` confirmation pass on v0.2. Closed three deltas: (1) the migration count in §7 Commit B table now matches §4's "25–30 sites with pre-flight audit" — v0.2 left "23" in the table while §4 said "25–30", a contradiction; (2) §3 stable-surface entry for `RuntimeConfig` rewritten as "builder API" not "struct fields" — v0.2 said both that struct fields were stable AND that struct literals were banned via `#[non_exhaustive]`, contradiction; (3) JSONL unknown-variant fallback explanation rewritten — v0.2 implied `#[serde(rename = "...")]` enabled fallback (it doesn't; it only fixes the discriminator string). Reader-side tolerance is a documented convention, not a serde feature. Plus example 01 now annotates `with_extras()` as demo-convenience and points at Open Question #1's "production should register tools explicitly" guidance.
- **v0.2 (2026-05-02):** In-repo `rfd-critic` (gpt-5.4 xhigh) pass. Closed three critical deltas: (1) added `FinishReason` and `EventStream` to the `pi_ai` re-export list (omitted in v0.1 — embedders parsing streamed responses need them); (2) rewrote the MicroVm/Remote re-export comment in present tense citing RFD 0023 §2 / 0026 §2 (v0.1's "added when RFDs land" was unnecessarily conditional — the trait shapes are already specified); (3) softened the Flue framing — "design patterns extracted" not "Flue is built on pi-mono-TS." Tightened: bumped the `RuntimeConfig` migration count from "23 sites" to "25–30 sites with Commit B pre-flight audit"; clarified `#[non_exhaustive]` semantics (builder-only is required for embedders, struct literals stay legal inside the workspace); added explicit JSONL wire-format stability contract for `SessionEntryKind`. Added new §2.5 "Embedder-side surfaces" disambiguating where the `task` tool, MCP servers, orchestrate runner, halo loop, monitor pump, and auto-approve sit relative to the SDK boundary. Example 01 model ID now configurable via `PI_PROVIDER` / `PI_MODEL` env vars.
- **v0.1 (2026-05-02):** Initial draft. Inspired by Flue's design (single SDK package, agent=harness framing, programmable-first); does NOT integrate with Flue (separate research found structural mismatch). Foundation for RFD 0028 (compiled agents from TOML).

## References

- **RFD 0022** — SandboxProvider trait (the foundation this SDK exposes).
- **RFD 0023** — Local MicroVM Sandbox (adds `MicroVmLauncher` + `MicroVmProvider` to the SDK surface; SDK 1.0 waits on this).
- **RFD 0026** — Remote Sandbox Transports (adds `RemoteTransport` + `RemoteProvider`; SDK 1.0 waits on this).
- **RFD 0028** (planned, sister) — Compiled agents from TOML manifest. Consumes `pi-sdk` as its build dependency.
- **Flue** — https://flueframework.com/, https://github.com/withastro/flue. Inspiration for design patterns; not an integration target.
- **`pi-coding-agent::sdk`** (`crates/pi-coding-agent/src/sdk.rs`) — the seed module being promoted to a standalone crate.
- **Cargo's `rust-version`** — https://doc.rust-lang.org/cargo/reference/manifest.html#the-rust-version-field — the MSRV declaration we should mirror.
- **`cargo-semver-checks`** — https://github.com/obi1kenobi/cargo-semver-checks — the CI tool enforcing SDK stability post-1.0.
