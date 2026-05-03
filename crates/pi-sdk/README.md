# pi-sdk

> ⚠ **Pre-1.0.** Any 0.x → 0.x+1 release MAY break the public API. Pin a fixed version.

`pi-sdk` is the public Rust API for embedding the [pi-rs](https://github.com/n3mes1s/playground) coding-agent harness in another application. One dependency, one entry point — `cargo add pi-sdk` and write your agent.

```toml
[dependencies]
pi-sdk = "0.1"
```

## Conceptual model

Four collaborating actors:

```text
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
                    │  │  ──.prompt(msg)──▶ │──┼─▶ AgentEvent stream     │   │
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

- **`AgentSessionRuntime`** is the long-lived hub — built once, holds shared state.
- **`AgentSession`** is per-conversation — open N of them and pipe prompts in.
- **`AgentEvent`** stream is the read side. Everything user-visible (text, tool calls, turn boundaries, errors) flows through it.
- **Provider / ToolRegistry / SandboxProvider / AuthStorage** are the four pluggable boundaries.

## Quick start (safe by default)

`pi_sdk::quick_start` wires the safe defaults: `AuthStorage::in_memory()` (no env scan), readonly tools only (`read`/`grep`/`find`/`ls`), no shell.

```rust,no_run
use pi_sdk::{quick_start, AgentEventKind, AuthMethod};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let runtime = quick_start("anthropic", "claude-haiku-4-5-20251001")?;

    // Embedder MUST set credentials — the runtime starts with zero secrets.
    runtime.config().auth_storage.set(
        "anthropic",
        AuthMethod::ApiKey { value: std::env::var("ANTHROPIC_API_KEY")? },
    );

    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    let session = runtime.create_session(Some(tx))?;
    tokio::spawn(async move {
        let _ = session.prompt("List files in this directory.".into()).await;
    });

    while let Some(evt) = rx.recv().await {
        match evt.kind {
            AgentEventKind::AssistantTextDelta { text } => print!("{text}"),
            AgentEventKind::TurnComplete => break,
            _ => {}
        }
    }
    Ok(())
}
```

For shell tools, fs-mutation tools, or a custom plug-in surface, use the full `RuntimeConfig::builder()` path — see [`examples/02_realistic_ci_agent.rs`](examples/02_realistic_ci_agent.rs).

## What you get

- **One crate, one import.** `cargo add pi-sdk` exposes the entire embedder surface. The underlying split crates (`pi-ai`, `pi-tools-core`, `pi-sandbox`, `pi-agent-core`, `pi-tool-types`) are re-exports — embedders never depend on them directly.
- **Six providers via one `Provider` trait** — Anthropic, OpenAI (Chat + Responses), Google, AWS Bedrock, Azure OpenAI. Plus `OpenAiCompatProvider` for any compat endpoint.
- **Built-in tools** — `read`, `write`, `edit`, `grep`, `find`, `ls`, plus `bash` (gated behind `with_unsafe_extras`) and `web_search`.
- **Sandbox abstraction** — `LocalProcessProvider` for in-process execution today; `MicroVmProvider` (RFD 0023) and `RemoteProvider` (RFD 0026) join the surface in 1.x once those land.
- **Hardened runtime** — `catch_unwind` around tool invocations, stream-event validation, per-session token budget guards, ToolGate session-scoped context, bash cwd jail, `AuthStorage` 0o600 + atomic-rename, `WireSerializer` JSONL hardening (1 MiB per-field cap, ANSI escape stripping, C1/bidi escape).
- **Top-level `pi_sdk::Error`** — one thiserror facade with `#[from]` for every underlying crate's error type.
- **Cost helper** — `pi_sdk::cost::{CostRegistry, estimate_cost_usd}` ships best-effort price tables for major models; embedders override with `CostRegistry::override_for(...)`.
- **Mocks** — `pi_sdk::mocks::{MockProvider, MockSandboxProvider}` (gated on the `mocks` feature) for zero-LLM-cost CI tests.

## What you don't get

These belong in pi-rs's own binary, not in `pi-sdk`:

- TUI rendering, interactive REPL, slash commands, picker, trajectory viewer.
- Halo loop (RFD 0025 self-improvement supervisor).
- Evolve daemon (RFD 0011/0013 AGENTS.md mutation).
- Pi-stats SQLite dashboard (the JSONL types ARE in the SDK so embedders can write telemetry; the storage / aggregation / web UI is binary-side).
- Pi-orchestrate campaign runner.
- The `task` tool (subagent orchestration) — lives in pi-coding-agent. Embedders open multiple `AgentSession`s instead.
- MCP server adapters — bring-your-own-bridge by implementing the `Tool` trait.

## Production checklist

The README tells you the safe path; this checklist tells you what to verify before shipping.

### Authentication & secrets

- [ ] **Do NOT call `AuthStorage::from_env()` in production.** Use `AuthStorage::from_env_explicit(&[("anthropic", "MY_TENANT_ANTHROPIC_KEY"), ...])` so the env-var allowlist is auditable. (`from_env()` is `#[deprecated]` and slurps 17 env vars unconditionally — CWE-526 risk.)
- [ ] If running multi-tenant, use `AuthStorage::scoped(provider_filter)` to deny cross-tenant credential bleed.
- [ ] If credentials should not change after init, `AuthStorage::sealed()`.
- [ ] On-disk auth files are written with `0o600` + atomic rename; verify your filesystem honors POSIX modes (NFS sometimes doesn't).

### Tool surface

- [ ] **Never use `ToolRegistry::with_unsafe_extras()` (or its alias `with_extras()`) in production.** It registers `bash`, a code-execution gateway. Use `ToolRegistry::new()` and register tools explicitly so the surface is auditable. The safe alternative for read-only inspection is `with_readonly_extras()`.
- [ ] If you need shell, register `bash` only with a `SandboxProvider` that actually isolates (microvm or remote — `LocalProcessProvider` runs in your process namespace).
- [ ] Custom `Tool` impls must be panic-safe: the runtime catches panics (Hardening §4.5 #1) but a panicking tool returns `ToolError` on every call until you fix it.
- [ ] Audit each custom tool's `invoke()` for input validation, output truncation respecting `ctx.max_output_bytes`, no unscoped fs access outside `ctx.cwd`.

### Tool-gate policy

- [ ] Implement `ToolGate::approve` instead of using `gate_ask_is_approve = true` (TUI-only).
- [ ] Use `GateContext.session_id` + `parent_session` to scope approvals (don't approve once and remember the answer cross-session).
- [ ] Set `RuntimeConfig::max_recursion`, `max_session_tokens`, `max_tool_invocations_per_turn` for your workload (defaults: 8 / 10M / 64). `max_session_tokens = 0` means disabled.

### Sandbox

- [ ] In production, use a real sandbox: `MicroVmProvider` (Linux, RFD 0023) or `RemoteProvider` (cross-platform, RFD 0026). `LocalProcessProvider` runs in your process — local dev / trusted-content tasks only.
- [ ] Set `VmCeiling` / `CallLimits` on the sandbox provider to bound per-call CPU/memory/walltime; don't rely on `bash.timeout_ms` alone (capped at 600 s by Hardening §4.5 #7).

### Streaming & observability

- [ ] Treat any string from `AgentEvent::AssistantTextDelta` and `ContentBlock.text` as **untrusted attacker-controlled text**. Do NOT render it as trusted prose to operators.
- [ ] If you tail `SessionEntry` JSONL, the SDK's `WireSerializer` already applies the default limits (1 MiB/field, ANSI strip, C1/bidi escape) — don't roll your own `serde_json::to_string` path.
- [ ] When auditing TTSR-injected messages, filter on `SessionEntryKind::InterceptorInjection` to separate synthetic from real operator input.

### Cost & budget

- [ ] Track `AgentEvent::Usage` and call `pi_sdk::cost::estimate_cost_usd(&usage, model_id, &registry)` per turn.
- [ ] Set a per-session budget cap and call `session.abort()` when exceeded — see `examples/02_realistic_ci_agent.rs`.
- [ ] Override `CostRegistry` defaults if your contract pricing differs from public list prices.

### Errors

- [ ] Match on `pi_sdk::Error` variants explicitly (don't rely on `Display` strings — they may change). Variant *names* are stable per RFD §3.
- [ ] Distinguish `BudgetExhausted` and `DepthExceeded` (operator-recoverable: bump caps and retry) from `Provider`/`Sandbox` (likely environmental; backoff + retry).

### Supply chain

- [ ] Run `cargo audit` and `cargo deny` against your resolved lockfile every release.
- [ ] Pin `pi-sdk = "0.1"` (or `"1"` once 1.0 ships) — accept MINOR bumps automatically, hold MAJOR for migration review.
- [ ] Each `pi-sdk` MINOR upgrade IS a supply-chain event. Read the changelog and `MIGRATION.md` even within a MINOR — security-CVE backports may force an unexpected breaking change inside MINOR.

### Subscribe

- [ ] Subscribe to GitHub Security Advisories on the `pi-rs` repo.
- [ ] Subscribe to GitHub Discussions for MSRV-bump pre-announcements.

## Examples

The five examples in [`examples/`](examples/) cover the embedder shapes most likely to come up:

| Example | What it shows |
|---|---|
| [`01_minimal.rs`](examples/01_minimal.rs) | Smallest possible embed — `quick_start` + one prompt. |
| [`02_realistic_ci_agent.rs`](examples/02_realistic_ci_agent.rs) | Production shape: tool-event surfacing, cost tracking, structured errors, cancellation. |
| [`03_custom_tool.rs`](examples/03_custom_tool.rs) | Implement the `Tool` trait for a domain-specific tool, register it. |
| [`04_custom_provider.rs`](examples/04_custom_provider.rs) | Implement `ProviderFactory` to inject a mock or custom LLM provider. |
| [`05_custom_sandbox.rs`](examples/05_custom_sandbox.rs) | Implement `SandboxProvider` for a private execution backend. |

Each is self-contained and runnable (most under `--features mocks`).

## Stability commitment

Per RFD 0027 §3:

- **MAJOR** (1.x → 2.0): breaking change. Allowed when motivated; requires migration guide entry.
- **MINOR** (1.x → 1.x+1): additive only. New trait methods get default impls. New types don't get renamed. New variants on enums use `#[non_exhaustive]`.
- **PATCH**: bug fixes only.

**6-month back-compat window** for SDK 1.x. Pinning `pi-sdk = "1"` upgrades cleanly to any 1.x within 6 months of release.

**Security exception:** any High+ severity CVE in a stable trait may force a breaking change inside a MINOR with 30-day pre-notification on GitHub Security Advisories.

**MSRV policy:** at most 2 MSRV bumps per year, MINOR releases only, with 30-day pre-announcement. Security-fix-grade rustc CVEs are exempt.

**Deprecation policy:** any 1.x deprecation lives ≥4 MINOR releases (~6 months) before removal in the next MAJOR.

## Threat model

**What pi-sdk defends against:**

- Malformed LLM responses (panic-safe tool dispatch, stream validation, cumulative-Usage idempotency).
- Adversarial JSON tool input (per-tool size caps, schema validation).
- Panicking custom tools (`catch_unwind` boundary).
- Out-of-order or oversized stream events (cap + reject).
- Tool-name collision attacks (`register` returns `Err(DuplicateName)`).
- Bash cwd escape via `..` traversal (canonicalize-and-jail).
- ANSI / bidi / C1 escape sequences in JSONL (`WireSerializer`).
- Auth file world-readable on multi-user hosts (0o600 + atomic rename).
- Env-var slurp risk (deprecated `from_env()`, opt-in `from_env_explicit`).
- Pathological tool-call loops in a single turn (per-turn invocation cap, default 64).
- Adversarial provider that emits `Finish::ToolUse` with zero tool calls (rejected as `ToolUseFinishWithoutCalls`).
- Per-session token budget overruns (`max_session_tokens` cap, default 10M; `0` = disabled).

**What pi-sdk does NOT defend against, by design:**

- A malicious crate the embedder pulls in (supply-chain attack on `pi-ai`'s transitive deps — run `cargo audit`).
- A compromised LLM endpoint with valid TLS.
- An embedder running pi-sdk in a parent process whose environment is attacker-controlled (use `from_env_explicit`).
- A model whose output is verbatim-displayed to a human in a trusted UI surface (`StreamInterceptor` consumers carry the "model content is untrusted" responsibility).
- Cross-call-site state leaks via embedder-shared singletons.

**Shared trust with the embedder:** the embedder picks the `SandboxProvider`, `ToolGate`, and `AuthStorage` source. Pi-sdk ships safe defaults; the embedder is responsible for not opting out of them in production.

## Security disclosure

Coordinated disclosure: report security issues via [GitHub Security Advisories](https://github.com/n3mes1s/playground/security/advisories) on the pi-rs repo. A `SECURITY.md` with the disclosure address ships before the first crates.io publish (RFD 0027 Commit I); RUSTSEC advisory namespace reserved.

## License

Dual MIT / Apache-2.0.

## See also

- [`CHANGELOG.md`](CHANGELOG.md) — per-release change list.
- [`MIGRATION.md`](MIGRATION.md) — search-and-replace table for renamed/removed symbols across releases.
- [`COMPATIBILITY.md`](COMPATIBILITY.md) — pi-sdk MINOR ↔ underlying-crate version matrix.
- [RFD 0027](../../rfd/0027-pi-rs-sdk.md) — full design contract, threat model, hardening contract.
- [RFD 0023](../../rfd/0023-sandbox-microvm.md) — local microVM sandbox.
- [RFD 0026](../../rfd/0026-sandbox-remote.md) — remote sandbox transports.
- RFD 0028 (planned) — compiled agents from TOML manifest (future consumer of this SDK).
