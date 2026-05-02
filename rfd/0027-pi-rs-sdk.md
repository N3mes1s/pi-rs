# RFD 0027 — Pi-rs as a Self-Contained Rust SDK

- **Status:** Discussion (v0.6)
- **Author:** pi-rs maintainers
- **Created:** 2026-05-02
- **Implemented:** (pending)

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
2. **Stability commitment** at 1.0: trait surfaces (`Provider`, `Tool`, `SandboxProvider`, `ProviderFactory`, `ToolGate`, `StreamInterceptor`) and POD types (`ToolResult`, `ToolSpec`, `Message`, `ContentBlock`, `Usage`, etc.) are MAJOR-versioned. MINOR adds; PATCH fixes.
3. **Clear boundary** between SDK material and binary-side material. Halo, evolve, TUI, slash commands, REPL modes stay in `pi-coding-agent` and are NOT in the SDK.
4. **Embed-in-50-lines example** that compiles and runs. The first thing in the SDK README.
5. **No SDK 1.0 until RFD 0023 (microvm sandbox) and RFD 0026 (remote sandbox) settle.** Both touch `SandboxProvider`. Freezing the SDK on a moving target wastes embedders' breaking-change budget.

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

**Cargo.toml dependencies:** ONLY workspace-aliased pi-rs crates that contribute to the public surface — `pi-tool-types`, `pi-ai`, `pi-tools` (or `pi-tools-core` once RFD 0023 Commit A2 splits it; until then `pi-tools` is the single crate), `pi-sandbox`, `pi-agent-core`. No `pi-tui`, no `pi-coding-agent`, no `pi-orchestrate`, no `pi-stats`. The dep on `pi-tools` updates to `pi-tools-core` mechanically when A2 lands; embedders see no change because both crates re-export `Tool` / `ToolRegistry` / `ToolContext` identically.

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
pub mod build;
pub use build::{BuildConfig, build_runtime_config};
```

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

**12-month back-compat window** for SDK 1.x. Within that window, an embedder pinning `pi-sdk = "1"` should be able to upgrade to any 1.x without code changes.

**Compatibility matrix.** Each `pi-sdk` MINOR maintains a published table of which underlying-crate versions it pins:

```
pi-sdk 1.2  →  pi-tool-types 1.0, pi-ai 1.4, pi-tools 1.1, pi-sandbox 1.2, pi-agent-core 1.3
pi-sdk 1.3  →  pi-tool-types 1.0, pi-ai 1.5, pi-tools 1.1, pi-sandbox 1.3, pi-agent-core 1.4
```

Embedders pin `pi-sdk` only; the matrix is the SDK's responsibility, not theirs.

**Stable surface (committed at 1.0):**

- All trait shapes: `Provider`, `Tool`, `SandboxProvider`, `ProviderFactory`, `ToolGate`, `StreamInterceptor`, `MicroVmLauncher` (when 0023 lands), `RemoteTransport` (when 0026 lands).
- All POD types: `ToolResult`, `ToolSpec`, `ToolError`, `Message`, `ContentBlock`, `ToolCall`, `Usage`, `ModelInfo`, `ProviderConfig`, `AuthMethod`, `Settings`.
- The runtime API: `RuntimeConfig` (builder API — `RuntimeConfig::builder().with_X(...).build()`; struct literals banned outside the workspace via `#[non_exhaustive]`, see §4), `AgentSession::send`/`prompt`/`compact`/`abort`, `AgentSessionRuntime::create_session`/`open_session`.
- Streaming events: `StreamEvent`, `StreamEventKind`, `AgentEvent`, `AgentEventKind`.
- `SessionEntry` + `SessionEntryKind` (so external telemetry consumers don't break).

**`SessionEntryKind` JSONL wire-format contract** (committed at SDK 1.0):

- The JSONL format is `serde_json::to_string(&entry) + "\n"`. One entry per line. UTF-8.
- The discriminator is `"kind"` (already the case via `#[serde(tag = "kind", rename_all = "snake_case")]`).
- New variants on `SessionEntryKind` are MINOR-additive. **Reader-side tolerance is a convention, not enforced by serde:** `#[serde(rename_all = "snake_case")]` only fixes the discriminator string — it does not make older readers skip unknown variants. Readers MUST be written to tolerate `kind` values they don't recognise (deserialize attempts that fail are caught and the row is skipped). Pi-stats `ingest.rs` already uses this pattern: a silent skip-and-continue with `Err(_) => continue` when serde returns an unknown-variant error (no logging in v1.0 — invisible failures, but the alternative would be log floods on legacy session files). External consumers can mirror the silent-skip approach or wire in their own logger via a `deserialize_with` adapter that tolerates unknowns.
- Field renames within an existing variant are MAJOR-breaking. Renaming `provider` → `provider_id`, for example, requires SDK 2.0.
- Optional fields added with `#[serde(default, skip_serializing_if = "Option::is_none")]` are MINOR-additive (existing rows deserialize cleanly because of `#[serde(default)]`; new rows surface the new field). Required fields cannot be added — those are MAJOR.
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

The `with_*` methods on the existing `RuntimeConfig` struct (`with_tool_gate`, etc.) at `crates/pi-agent-core/src/runtime.rs:138-163` stay usable inside the workspace until 1.0; at 1.0 they are deprecated in favour of the builder. Callers can keep them by chaining off `RuntimeConfig::default()` style (which is what `BuildConfig`'s convenience builder does today).

**Builder-only requirement for embedders:** after SDK 1.0, `pi-sdk` consumers construct `RuntimeConfig` exclusively via `RuntimeConfig::builder()`. The `#[non_exhaustive]` annotation enforces this at the crate boundary — Rust's compiler refuses struct literals from outside the crate. Within `pi-rs`'s own workspace (the binary, tests, internal callers) struct literals remain legal and continue to be used where convenient.

**0.x → 1.0 migration preview for embedders:** today (`pi-sdk = "0.1"`) the example 01 builds via `build_runtime_config(BuildConfig { ... }).with_sandbox_provider(...)` — the fluent `with_*` methods chain off `RuntimeConfig` directly. At 1.0 the same code becomes `RuntimeConfig::builder().sandbox_provider(...).build()` and `BuildConfig` shrinks to a thin wrapper. Embedders should expect to replace `BuildConfig { ... }` literals + `RuntimeConfig { ... }` literals with the builder; everything else (Settings, ToolRegistry, AuthStorage, etc.) stays the same. Migration is a one-time edit when pinning to `pi-sdk = "1"`.

This requires a migration: approximately 25–30 sites in the workspace today do `RuntimeConfig { session_manager, ..., sandbox_provider }`. Exact count is captured during the **Commit B pre-flight audit** (run `rg --files-with-matches 'RuntimeConfig\\s*\\{' crates/`); the actual sites get rewritten to use the builder. Mostly mechanical, ~1 day of work.

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
            AgentEventKind::TextDelta { text } => print!("{text}"),
            AgentEventKind::TurnComplete => break,
            _ => {}
        }
    }

    Ok(())
}
```

50 lines (with comments). Compiles against `pi-sdk = "0.1"`. Runs end-to-end with a real provider.

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

## Implementation schedule

Six commits, each independently reviewable. None of them block on RFD 0023 / 0026 landing — `pi-sdk 0.x` ships first, gets feedback, refactors, then waits for 0023/0026 to settle before declaring 1.0.

| # | Commit | Est. LoC |
|---|---|---|
| **A** | New crate `crates/pi-sdk/` with `Cargo.toml` + `src/lib.rs` re-exports + workspace registration. Move `BuildConfig` + `build_runtime_config` from `pi-coding-agent::sdk` into `pi-sdk::build`. Keep a deprecated `pub use pi_sdk::*;` shim in `pi-coding-agent::sdk` for back-compat during the transition. | 200 |
| **B** | `RuntimeConfig` builder pattern + `#[non_exhaustive]` annotation. Migrate the existing struct-literal sites in the workspace (~25–30; exact count from pre-flight audit, see §4). | 400 |
| **C** | Five `examples/` files that compile + run. | 500 |
| **D** | `pi-sdk` README — 50-line embed example, plugin-pattern docs, version policy, compatibility matrix. | 400 (markdown) |
| **E** | docs.rs publish — first 0.1.0 release of `pi-sdk` to crates.io. SDK is now pip-equivalent: `cargo add pi-sdk`. | scripts |
| **F** | Migration: pi-coding-agent's binary switches from `use pi_coding_agent::sdk::*` to `use pi_sdk::*`. Removes the deprecated shim from Commit A. | 100 |

**Total: ~1600 LoC + docs.**

The SDK becomes pre-1.0 stable with Commit E. The 1.0 freeze waits for:

1. RFD 0023 (microvm sandbox) lands — `MicroVmLauncher` and `MicroVmProvider` join the public surface.
2. RFD 0026 (remote sandbox) lands — `RemoteTransport` and `RemoteProvider` join the public surface.
3. At least one external embedder builds something non-trivial against pi-sdk 0.x and reports back. Without external dogfood, "stable surface" is just intent.
4. RFD 0028 (compiled agents) prototype proves the codegen target is right.

Estimated 1.0 timeline: 8–12 weeks after this RFD ships. v0.x can ship in 2 weeks.

## Out of scope / deferred

- **Python / Node / WASM bindings.** Out of scope for this RFD. If they happen, they wrap `pi-sdk`. No constraints on `pi-sdk`'s shape from FFI requirements.
- **Pi-stats embedder hooks.** Embedders can read JSONL session entries. The SQLite ingest + dashboard remain pi-binary territory.
- **Deprecation of `pi-coding-agent::sdk`.** During 0.x both modules coexist (the binary uses the new path; old embedders see a deprecation warning). The shim is removed in Commit F.
- **Stable serde representations.** `SessionEntry` JSONL is the wire format embedders depend on; serde for `RuntimeConfig` etc. is convenience-only and may change.
- **MCP server adapters.** Out of scope here; `pi-coding-agent` already loads MCP servers, and that wiring is binary-side. A future RFD can add an MCP trait to `pi-sdk` if the demand surfaces.

## Open questions

1. **Should `pi-sdk` re-export all pi-tools tools, or require embedders to register them explicitly?** Today `ToolRegistry::with_defaults()` registers four tools (read/write/edit/bash); `ToolRegistry::with_extras()` registers eight (defaults + grep/find/ls/web_search). The `monitor` and `task` tools are binary-side (registered by `pi-coding-agent` startup, not by `pi-tools`); SDK embedders never see them by default. Both helpers are re-exported. **Canonical guidance:** demo / quick-start examples use `with_extras()` (richest set, fastest path); production agents call `ToolRegistry::new()` and register tools explicitly so the tool surface is auditable; embedders who want a leaner default tool surface call `with_defaults()`. Document this in the README's "Production checklist."
2. **Feature flags or `--no-default-features`?** Likely yes for: `pi-sdk` default features = `[default-tools, default-providers, anthropic, openai]`; embedders who want a leaner build disable the providers they don't use. v0.1 ships without features (everything included); features land before 1.0.
3. **`async-trait` vs native async traits?** Current pi-rs uses `async-trait`. Native async traits stabilised in Rust 1.75 but have ergonomic issues with trait objects (no `dyn Provider`). v1.0 sticks with `async-trait`; revisit at v2.0 once the Rust ecosystem catches up.
4. **What's the SDK's MSRV?** Today's pi-rs MSRV is implicit. SDK 1.0 must declare one. Recommendation: track stable Rust − 2 minors (i.e., always at least 6 months old) so embedders aren't forced onto bleeding-edge toolchains.
5. **Should `pi-sdk` expose `ProviderFactory` defaults or require embedders to provide one?** `DefaultProviderFactory` is the natural choice; export it. Embedders override only when they want to inject mocks (test) or custom providers.
6. **Telemetry opt-in/out for embedded agents.** The runtime today writes JSONL session entries unconditionally if a `SessionManager` is configured. Embedders may not want disk I/O. Decision: `SessionManager::in_memory()` already exists and is the no-disk path. Document it as the "no telemetry" choice.
7. **License.** pi-rs is currently unlicensed (workspace `Cargo.toml` has no `license` field). Pre-1.0 SDK release requires picking one. Recommendation: MIT or Apache-2.0 (dual-license is the Rust ecosystem norm). Decide before Commit E.
8. **Crate name collision check.** `pi-sdk` on crates.io — verify availability before publishing. If taken, fallback name: `pirs-sdk` or `pi-rs-sdk`.

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
