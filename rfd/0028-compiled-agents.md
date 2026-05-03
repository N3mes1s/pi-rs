# RFD 0028 — Compiled agents from TOML manifest (meta + split into A/B/C/D)

- **Status:** Draft (v0.3)
- **Author:** Giuseppe Massaro (drafted with claude-opus-4-7, revised after rfd-critic v0.1 + v0.2 passes)
- **Created:** 2026-05-03
- **Implemented:** *(pending sub-commits Commit A–Commit D)*

## Summary

`pi-build my-agent.toml` compiles a declarative TOML manifest into a
standalone Rust binary that embeds `pi-sdk` (RFD 0027). The binary
takes a prompt on stdin / `--prompt`, runs one or more agent turns
against the configured provider, and exits. Solves pi-rs's
"headless distribution" gap: today operators either (a) run the
`pi` interactive CLI (40+ flags, not designed for production) or
(b) hand-write a Rust embedder against `pi-sdk` (correct but
tedious). Compiled agents make (b) declarative.

This RFD covers the entire compiled-agent design as **four
sub-commits in a single document** (matching RFD 0023's
A1/A2/B/C/D/E/F/G pattern). Each sub-commit ships independently
but they share Commit Cross-cutting choices and §Out of scope:

- **Commit A** — Manifest schema (`pi.toml` + `agent.toml`).
- **Commit B** — Codegen + runtime shape (what `pi-build` generates).
- **Commit C** — Distribution (how the resulting binary is shipped).
- **Commit D** — Halo integration (RFD 0025) — compiled agents as
  autonomous-loop cycle nodes.

(v0.1 proposed splitting into four separate RFD files. The
rfd-critic correctly noted that pi-rs's prior-art convention is
RFD 0023's single-file sub-commit pattern; deferring to that.)

## Background

### Today's headless story is anaemic

- **`pi-coding-agent` CLI** — 40+ flags (`--print`, `--no-tools`,
  `--provider`, `--thinking`, etc.). Designed for interactive use
  + admin verbs. Production operators use it via shell scripts
  that string-paste flags; surface is unstable enough that we
  refuse to commit to a 1.0 (RFD 0027 Commit Background).
- **`pi-orchestrate`** (RFD 0021) — runs N campaign rows in
  parallel against a single agent shape. Closest thing to a
  "headless runner" today, but it's a wrapper around the same
  CLI surface. An "orchestrate row" is `pi-coding-agent`
  invocation + flags, not a portable artifact.
- **Embedders writing Rust** — `pi-sdk` (RFD 0027) shipped this
  pre-publish. Correct path for power users. Not viable for
  operators who don't write Rust.

The gap: a way to ship "an agent" as a single immutable artifact
(binary), reproducible from a single declarative file (manifest),
that can be invoked from any shell or scheduler without a pi-rs
toolchain present at run time.

### Why compile-time, not interpreter-time

Two alternatives were considered + rejected:

1. **Interpreter** (`pi run my-agent.toml`). Loads the manifest,
   constructs the runtime in-process, runs. **Rejected** because:
   - Forces every operator to install pi-rs (the binary).
     Compile-time targets static + cross-compilable artifacts.
   - Same trust boundary as `pi-coding-agent` — the manifest can
     reference `bash` and the interpreter has to enforce
     allowlists. A compiled binary's allowlist is *encoded in
     the binary itself*; the operator inspects it via
     `cargo-audit` style tooling.
   - Doesn't solve the "stable surface" problem — the
     interpreter is on the same drift schedule as pi-rs.
2. **WASM agents.** Compile manifest into a WASM module, run in
   a host. **Rejected** for v1 — interesting, but the WASM
   ecosystem doesn't have stable async + tokio + reqwest
   support yet (2026 status). Revisit at SDK 2.0.

### Inspiration

`flue build` (RFD 0027 v0.1 framing) is the obvious analogue —
takes a YAML manifest and emits a Node.js bundle. Compiled-agents
borrows the *concept* (declarative → built artifact) but the
mechanism is Rust + cargo + pi-sdk, not Node + bundler.

### What this RFD doesn't do

- Doesn't replace `pi-coding-agent`. The interactive CLI stays —
  it's the development + admin surface (per RFD 0027 §2.5).
- Doesn't replace `pi-orchestrate`. Orchestrate becomes one
  consumer of compiled agents (a campaign row can be
  "invoke compiled-agent X with input Y").
- Doesn't bind to MCP, LangGraph, AutoGen, or any external agent
  framework. Compiled agents are pi-sdk consumers; cross-framework
  bridging is a future RFD (likely "RFD 003N — agent-protocol
  adapters").

## Proposal

### What a compiled agent is

A single self-contained Rust binary, built by Cargo, that:

1. Parses CLI args + reads stdin for the prompt(s).
2. Constructs an `AgentSessionRuntime` via
   `pi_sdk::RuntimeConfig::builder()` using the manifest values
   baked in at compile time.
3. Reads any required secrets from env at runtime via
   `AuthStorage::from_env_explicit(allowlist)`. Secrets are
   NEVER inlined in the binary.
4. Runs one or more agent turns. Streams text to stdout (default)
   or JSONL events to stdout (`--jsonl`).
5. Exits with status 0 on success, 1 on agent error, 2 on auth
   error, 3 on tool-budget error.

### The split

| Sub-commit | Owns | Blocks | Dep on |
|---|---|---|---|
| **Commit A** | Manifest schema (TOML grammar, validation, versioning) | Commit B, Commit C, Commit D | RFD 0027 (SDK surface) |
| **Commit B** | Codegen + runtime (`pi-build` verb, generated `main.rs` shape, exit-code contract, JSONL stdout protocol) | Commit D | Commit A, RFD 0027 |
| **Commit C** | Distribution (cargo profile + `--target` pass-through; reproducibility / `verify` / `migrate` deferred to v2) | — | Commit A, Commit B |
| **Commit D** | Halo integration — halo spawns the compiled-agent binary the way it spawns `pi --orchestrate` today (no new halo cycle-kind plug-in surface) | — | Commit A, Commit B, RFD 0025 |

Implementation order:

```
A ──▶ B ──┬──▶ C
          └──▶ D
```

A and B are the load-bearing pair; C + D land in parallel after B.

### Cross-cutting choices (locked by this meta-RFD)

These constrain every sub-commit. Sub-commits may not unilaterally
override them; changes require revising this meta-RFD first.

#### 1. Manifest format = TOML, not YAML

- TOML matches the rest of pi-rs (`.pi/halo.toml`,
  `compatibility.toml`, `Cargo.toml`). One serialisation library
  (`toml` crate), one mental model.
- YAML's allure is matching Anthropic Skills + LangChain
  manifests, but YAML's complexity (anchors, multi-doc, type
  coercion) is a security surface compiled agents don't need.
- Schema validation is **two-pass**: pass 1 reads only
  `schema_version` from a permissive shim that ignores unknown
  keys; pass 2 (only if version matches v1) does the strict
  parse with `#[serde(deny_unknown_fields)]`. A v2 manifest
  fails with `SchemaTooNew { found: 2, supported: 1 }`, NOT a
  confusing `unknown field 'foo'` error from a v2-introduced
  key. (rfd-critic v0.1 finding; one-pass + `deny_unknown_fields`
  conflates "schema too new" with "typo in v1 key".)

#### 2. Generated artifact = Cargo project, not raw `rustc`

- `pi-build agent.toml` emits a directory containing:
  - `Cargo.toml` — declares `pi-sdk = "0.1"` (caret-pin per
    RFD 0027 §6) + the user's tool deps (if any).
  - `src/main.rs` — generated; tokio main, deterministic.
  - `pi-build.lock` — pi-rs version + manifest hash that
    produced this output. Embedded for `pi-build verify`.
- `cargo build --release` from inside the directory produces
  the runnable binary. The user's toolchain owns the build —
  pi-build doesn't ship a hidden rustc.
- Rationale: every Rust developer knows cargo. No magic build
  step; the operator can `cargo audit` the generated tree.

#### 3. Secrets = env at runtime, never compile-time

- The manifest declares an *env-var allowlist* via
  `secrets = ["ANTHROPIC_API_KEY"]`. Codegen lowers this to
  `AuthStorage::from_env_explicit([("anthropic",
  "ANTHROPIC_API_KEY")])` per RFD 0027 §4.5 #8.
- Compile-time secrets in the manifest are an explicit
  parse-error (`secret-in-manifest` lint, hard-fail).
- Rationale: a compiled binary distributed via container image
  or `cargo install` should never embed credentials. CWE-798
  defense.

#### 4. Stdout wire format = `AgentEvent` JSONL

- When invoked with `--jsonl`, the agent emits one
  `serde_json::to_string(&AgentEvent)` per line on stdout.
- `AgentEvent` (and the nested `AgentEventKind`) already derive
  `Serialize`/`Deserialize` (verified `pi-agent-core/src/event.rs:6,68`)
  and are re-exported from `pi-sdk`, so consumers
  (halo, orchestrate, ad-hoc shell pipelines) deserialize via
  the same types pi-sdk publishes. Zero duplicate format work.
- Default mode (no `--jsonl`) is plain UTF-8 text — assistant
  output only, no metadata.

**Note on the relationship to pi-sdk's `WireSerializer`:**
`WireSerializer` serializes `SessionEntry` (the *on-disk
session-log* format used by `SessionManager` for session replay,
hardened in RFD 0027 H6 with a 1 MiB/field cap + ANSI strip +
C1/bidi escape). That is a **different** format from the
in-process `AgentEvent` channel — `SessionEntry` carries
tool-result outcomes + interceptor injections + replay metadata
that the channel doesn't. For streaming live events on stdout
the `AgentEvent` shape is the right choice; converting to
`SessionEntry` requires an `AgentEvent → SessionEntry` mapper
that does not exist in pi-sdk today and would be net-new
surface. (rfd-critic v0.2 finding N1; v0.2 incorrectly assumed
the two formats were unified.)

#### 5. Exit codes = numeric stability contract

| Code | Meaning |
|---|---|
| 0 | Turn completed successfully. |
| 1 | Agent error (provider failure, tool error, recursion-depth exceeded). |
| 2 | Auth error (missing required env var; `MissingAuth`). |
| 3 | Tool-budget guard tripped (per-session token cap or per-turn invocation cap; `BudgetExhausted` / `InvocationCapExceeded`). |
| 64 | `EX_USAGE` — missing required CLI arg or unknown flag. |
| 65-78 | Reserved for additional `sysexits.h` codes if needed. |
| 128+ | Signal exit (per POSIX: `128 + signum`). |

Halo + orchestrate inspect these in their cycle-driver loops.
Stable across 0028 minors — adding a new code is MAJOR.

#### 6. v1 MVP tools = built-in only

- `read`, `write`, `edit`, `bash`, `grep`, `find`, `ls`,
  `web_search` (the `pi-tools` registry).
- Custom Rust tools are deferred to v2. Manifest grammar
  reserves `[[tool]]` table syntax for them but parsing v1
  rejects any `[[tool]]` table with `kind != "builtin"`.
- Rationale: keeps the v1 build trivially fast and
  reproducible. No external crate fetches at compile time
  beyond pi-sdk's transitive depgraph.

#### 7. v1 providers = all six pi-sdk providers

The manifest's `[provider] name = "..."` accepts the same six
strings pi-sdk's `ProviderKind` accepts: `anthropic`, `openai`,
`openai-compat`, `google`, `bedrock`, `azure-openai`. Codegen
lowers via the existing `ModelRegistry::resolve()` path. No v1
restriction; if a provider works in pi-sdk, it works in a
compiled agent.

#### 8. stdout vs stderr separation

- **stdout** = agent output ONLY. Plain text in default mode;
  one JSON object per line in `--jsonl` mode. Halo +
  orchestrate parse stdout as a structured stream.
- **stderr** = diagnostics ONLY (tracing logs, panic
  backtraces, `rust_log` output, codegen warnings if any
  surface at runtime). NEVER any agent output, NEVER
  JSONL-formatted lines.
- Rationale: halo's JSONL parser (Commit D) is fragile if `tracing`
  ever logs to stdout. Keeping the two streams disjoint by
  contract is cheaper than parsing-with-resync.

#### 9. Tokio runtime flavour

Generated `main.rs` uses `#[tokio::main(flavor = "current_thread")]`,
not the multi-thread default. Compiled agents are
prompt-then-exit binaries; the multi-thread runtime adds
~10 ms startup latency per worker thread for no throughput
benefit on the typical single-prompt workload.

#### 10. Compiled agents do NOT walk `AGENTS.md`

The pi-coding-agent CLI walks `AGENTS.md` from the cwd up to
the repo root and merges it into the system prompt
(`pi_coding_agent::cmd::locate_agents_md`). Compiled agents
deliberately do NOT do this — the manifest is the **sole**
source of truth for the system prompt. Reproducibility
requires that the same binary on a different host produces
the same agent shape; ambient `AGENTS.md` defeats that.

If an embedder wants AGENTS.md-style overlay, they bake the
content into `[runtime] system_prompt` at manifest-author time.

### Sketch — sub-commit scopes

#### Commit A — Manifest schema

A complete `agent.toml` for v1 looks like:

```toml
# agent.toml — compiled-agent manifest, v1.
schema_version = 1                # MUST. Bumped on breaking changes.

[agent]
name        = "fix-flaky-tests"   # snake-case-or-hyphen, used as binary name.
description = "Auto-bisects flaky test runs."
version     = "0.1.0"             # SemVer; baked into the binary.

[provider]
name     = "anthropic"
model    = "claude-haiku-4-5-20251001"
thinking = "medium"               # off | low | medium | high | xhigh

[secrets]
required = ["ANTHROPIC_API_KEY"]  # env vars to allowlist in AuthStorage

[tools]
allowlist = ["read", "grep", "find", "ls", "bash"]
disallow_unsafe = false           # if true, refuse to register `bash`/`write`/`edit`
# v1 has NO per-tool config blocks. pi-tools-core today reads tool
# parameters (e.g., bash timeout_ms) from per-invocation tool input
# JSON, not from registration-time config; there's no plumbing for
# manifest-time per-tool overrides. Reserved for v2.

[runtime]
system_prompt = """
You are a flaky-test bisector. Identify the seed line.
"""
max_session_tokens          = 200_000   # H2 caps; reasonable defaults applied if absent.
max_tool_invocations_per_turn = 50
max_recursion               = 4
```

Commit A's deliverable: this schema + a `pi-build validate
agent.toml` verb + serde types + round-trip test. ~600 LoC.

#### Commit B — Codegen + runtime

`pi-build my-agent.toml [--out target-dir]` walks the manifest
and emits a Cargo project. `main.rs` template (sketch, exact
output frozen by Commit B):

```rust
// CODE GENERATED by pi-build {version} from agent.toml hash {sha256}.
// DO NOT EDIT. Regenerate via `pi-build agent.toml`.
use pi_sdk::{
    create_agent_session, AgentEventKind, AuthStorage, LocalProcessProvider,
    ModelRegistry, RuntimeConfig, SessionManager, Settings, ThinkingSetting,
    ToolRegistry,
};
use std::sync::Arc;

#[tokio::main(flavor = "current_thread")]
async fn main() -> std::process::ExitCode {
    let auth = match AuthStorage::from_env_explicit([
        ("anthropic", "ANTHROPIC_API_KEY"),
    ]) {
        Ok(a) => a,
        Err(_) => return std::process::ExitCode::from(2),
    };
    // Build ONE registry honouring the manifest allowlist, then
    // pass it to BOTH the runtime (`.tools(...)`) AND the sandbox
    // provider via `tools.clone()` (the registry is `Clone`; the
    // clone shares no mutable state — this is HashMap-of-Arcs
    // under the hood). `LocalProcessProvider::with_defaults()`
    // would silently instantiate a fresh `with_unsafe_extras()`
    // registry inside the sandbox and bypass the manifest
    // allowlist — codegen MUST use `LocalProcessProvider::new`
    // with the cloned allowlist. (rfd-critic v0.1 finding C2.)
    let mut tools = ToolRegistry::with_unsafe_extras();
    tools.keep_only(&[
        "read".into(), "grep".into(), "find".into(),
        "ls".into(),   "bash".into(),
    ]);
    let sandbox = Arc::new(LocalProcessProvider::new(tools.clone()));

    let (event_tx, mut event_rx) = tokio::sync::mpsc::unbounded_channel();
    let cfg = RuntimeConfig::builder()
        .session_manager(SessionManager::in_memory())
        .auth_storage(auth.clone())
        .model_registry(ModelRegistry::new(auth))
        .tools(tools)
        .settings(
            Settings::builder()
                .provider("anthropic")
                .model("claude-haiku-4-5-20251001")
                .thinking(ThinkingSetting::Medium)
                .build(),
        )
        .system_prompt("You are a flaky-test bisector...")
        .with_sandbox_provider(sandbox)
        .with_max_session_tokens(200_000)
        .with_max_tool_invocations_per_turn(50)
        .with_max_recursion(4)
        .build()
        .expect("compile-time-validated config");

    // Event pump: stdout is the JSONL wire surface Commit D consumes.
    // `AgentEvent` derives Serialize, so `serde_json` is the whole
    // serialiser. NOT `WireSerializer` — that operates on
    // `SessionEntry` (a different on-disk type; see Commit Cross-cutting
    // #4). Default mode forwards only `AssistantTextDelta` as
    // plain text.
    let jsonl = std::env::args().any(|a| a == "--jsonl");
    let pump = tokio::spawn(async move {
        while let Some(evt) = event_rx.recv().await {
            if jsonl {
                // Commit D's spend attribution reads Usage events from here.
                // unwrap is safe: AgentEvent's serde shape is total.
                println!("{}", serde_json::to_string(&evt).unwrap());
            } else if let AgentEventKind::AssistantTextDelta { text } = &evt.kind {
                print!("{text}");
            }
            if matches!(evt.kind, AgentEventKind::TurnComplete) { break; }
        }
    });

    // create_agent_session returns `(AgentSessionRuntime, AgentSession)`.
    // The runtime is held alive across `prompt(...).await` — dropping
    // it would close the provider/event channel. Bind both via tuple
    // pattern; `_runtime` keeps it alive until end-of-scope.
    let (_runtime, session) = match create_agent_session(cfg, Some(event_tx)) {
        Ok(rs) => rs,
        Err(_) => return std::process::ExitCode::from(1),
    };
    let prompt = read_prompt_from_args_or_stdin();   // CLI helper, generated.
    let exit = match session.prompt(prompt).await {
        Ok(_) => 0,
        Err(e) => map_runtime_error_to_exit(e),       // 1/2/3 per Commit Cross-cutting #5.
    };
    let _ = pump.await;
    std::process::ExitCode::from(exit)
}
```

Commit B's deliverable: the `pi-build` binary (lives in
`crates/pi-build/`), the codegen template (built into the
binary), the JSONL stdout protocol contract, the exit-code
mapper, and the `read_prompt_from_args_or_stdin` + event-pump
helpers. ~1200 LoC.

Commit B's hard codegen invariants (each gets a regression test):

1. **Same allowlist, both registries.** The `ToolRegistry` value
   passed to `.tools(...)` and the one inside the
   `LocalProcessProvider` MUST contain the same set of tool
   names. Test: build both, then
   `assert_eq!(left.names(), right.names())`. Failing this
   silently restores all 8 unsafe tools through the sandbox path.
2. **AgentEvent JSONL shape is stable.** Serializing a
   representative `AgentEvent` (`AssistantTextDelta`,
   `AssistantToolCall`, `Usage`, `TurnComplete`) and
   round-tripping through `serde_json::from_str` MUST equal the
   original. Locks the Commit Cross-cutting #4 wire-format claim.
3. **Stdout discipline.** Tracing, panics, and warnings go to
   stderr. Stdout in `--jsonl` mode emits ONLY
   `serde_json::to_string(&evt)` output — no bare `println!`
   for diagnostics, no `tracing::info!` to stdout subscriber.
4. **Tokio runtime flavour.** `#[tokio::main(flavor = "current_thread")]`
   is non-negotiable per Commit Cross-cutting #9. Generated `main.rs`
   contains the literal attribute exactly.

Commit B explicitly does NOT touch pi-sdk's surface. If a future
embedder wants `WireSerializer`-grade hardening on the streamed
event JSONL (1 MiB caps, ANSI strip, etc.), that's a separate
follow-up against pi-sdk introducing an
`AgentEvent → SessionEntry` adapter — not codegen scope.

#### Commit C — Distribution

How the operator ships the resulting binary. v1 keeps the
surface deliberately narrow (rfd-critic v0.1 flagged the
v0.1 scope as overengineered for a v1 with one consumer).

- **Cargo profile.** Default to `release` + `lto = "thin"` +
  `strip = true`. Adds 2-3 minutes to the build but produces a
  trimmer artifact.
- **Cross-compile pass-through.** `pi-build --target
  aarch64-apple-darwin` forwards to `cargo build --target ...`.
  Operator must have the target installed (`rustup target add`);
  pi-build doesn't bundle toolchains.

Deferred to **v2** (each separately motivated when an
operator asks):

- Bit-identical reproducibility (`SOURCE_DATE_EPOCH` plumbing,
  documented invariants across cargo MINORs).
- `pi-build verify <binary>` verb (re-runs codegen + diffs).
- Schema migration tooling (`pi-build migrate`) — premature for a
  schema with exactly one version.

Out of scope (whole 0028 series): signing (Sigstore / cosign)
→ future RFD; container images → user wraps the binary in
their own Dockerfile; package-manager distribution (apt, brew)
→ operator's choice.

Commit C's v1 deliverable: docs + `--target` flag pass-through +
`--release`/`--debug` toggle. ~150 LoC.

#### Commit D — Halo integration

Halo (RFD 0025) is pi-rs's autonomous-loop supervisor. Today
halo invokes `pi --orchestrate` as a subprocess (verified
RFD 0025 Commit Composition with pi-orchestrate, lines 247-258).

Commit D adds a **second** subprocess shape halo knows how to spawn —
a compiled-agent binary — using the same subprocess machinery,
NOT a new "cycle-kind plug-in" surface. (rfd-critic v0.1 noted
that halo today has no cycle-kind dispatch; inventing one
would balloon the LoC estimate and re-architect halo.)

```toml
# halo.toml — supervisor config.
[[cycle]]
binary = "./fix-flaky-tests"      # path resolved relative to halo cwd, or $PATH
args   = ["--jsonl"]              # appended after the binary path
prompt = "Audit yesterday's flaky CI failures and propose fixes."
on_exit = { 0 = "continue", 1 = "alert", 3 = "throttle" }
```

Halo:

1. Spawns the binary in a halo-owned worktree (per RFD 0025
   §Halo-owned clone precondition) — same subprocess plumbing
   that today spawns `pi --orchestrate`.
2. Pipes the prompt to stdin (or appends as the final CLI arg
   per Commit B's `read_prompt_from_args_or_stdin` helper).
3. Streams the binary's stdout `--jsonl` lines into the halo
   cycle log.
4. Maps the agent's exit code to a halo policy (continue /
   alert / throttle) per `on_exit`.
5. Attributes the agent's spend (parsed from `Usage`-kind JSONL
   lines per Commit B's wire format) to halo's daily-budget ledger.

Compiled agents are inert (they don't loop themselves) — halo
provides the outer loop. This is the killer use case: operators
write a TOML, halo runs it forever.

Commit D's deliverable: halo subprocess-cycle support for arbitrary
binaries (not just `pi --orchestrate`) + JSONL stdout parser
+ spend attribution + integration test. ~600 LoC.

Commit D explicitly does NOT add a new halo cycle-kind plug-in trait —
that's a halo refactor + would need its own RFD.

### What we're NOT designing

- **Multi-agent graphs.** v1 = one agent per manifest. Operators
  compose graphs by chaining halo cycles or shell pipelines
  (`agentA | agentB`). Native graph syntax revisited at v2.
- **Long-running agent processes.** v1 = one prompt → one exit.
  No persistent server mode, no `--listen`. Halo + cron supply
  the "keep running" semantics.
- **Custom Rust tools at compile time.** Reserved
  `[[tool.kind = "rust"]]` syntax in Commit A but rejected by the
  v1 parser. v2 work.
- **Microvm sandbox integration.** Blocked on **RFD 0023
  Commit G** (`MicroVmProvider` wire-up + `pi sandbox doctor`
  CLI) plus the rootfs build (RFD 0023 Commit B). Compiled
  agents in v1 use `LocalProcessProvider` only; the manifest
  reserves `[runtime] sandbox = "microvm"` syntax but the v1
  parser rejects it. Additional gate the user is tracking but
  which is not yet an RFD: a "contextfs" library API for
  microvm fs-mounting (RFD-to-be-numbered). When that lands,
  add a sub-commit §E here. (rfd-critic v0.1 caught the v0.1
  draft's mis-citation of "RFD 0021 contextfs" — RFD 0021 is
  pi-orchestrate, not contextfs.)
- **MCP server adapters.** pi-sdk doesn't ship MCP yet (RFD 0027
  Open Question; binary-side concern). Compiled agents inherit
  the same boundary. Future RFD bridges if demand surfaces.

## Test plan

This meta-RFD's verification is delegated to the sub-commits; each
ships its own test-plan section. Cross-cutting tests that
verify the *split* itself works:

- **End-to-end "dice oracle"** — `examples/dice-oracle.toml` →
  `pi-build` → cargo build → `./dice-oracle "roll a d20"` returns
  text on stdout, exit code 0. Exercises Commit A + Commit B together.
- *(Reproducibility integration test deferred to v2 alongside
  the `verify` verb — see Commit C.)*
- **Halo cycle test** — halo.toml configures a compiled agent
  with a deterministic MockProvider; halo runs N cycles; assert
  cycle log captures the agent's JSONL output. Exercises Commit D.
- **Manifest forward-compat** — a v1 parser MUST reject a
  manifest with `schema_version = 2` (don't silently accept).
  Exercises Commit A's versioning contract.

## Out of scope (this meta-RFD only)

- The detailed serde shape of `agent.toml` — defined in Commit A.
- The byte-exact codegen template — defined in Commit B.
- Specific cross-compile target list — defined in Commit C.
- Halo's `on_exit` policy table semantics — defined in Commit D.

## Open questions

1. **`pi-build` lives where?** Three options:
   - **`crates/pi-build/`** as a new workspace member (parallel
     to `pi-coding-agent`). Recommended.
   - Add a `pi build` subcommand to the existing `pi` binary.
     Couples compiled-agent ergonomics to pi-coding-agent's
     release cadence.
   - Standalone `cargo install pi-build` published from a
     separate crate. Cleanest separation but extra publish.

   Recommendation: workspace member, `cargo install --path
   crates/pi-build` for v1; promote to its own crates.io publish
   alongside pi-sdk 0.2.

2. **`agent.toml` location convention.** Cargo's `Cargo.toml` is
   the universal root marker. Should compiled agents adopt
   `pi-agent.toml` at repo root, or `<name>.toml` anywhere?
   Recommendation: per-file naming for v1 (`pi-build foo.toml`),
   reserve `pi-agent.toml` as the "discoverable root" convention
   for v2 (`pi-build` with no args looks for it).

3. **Provider-credential auto-discovery in compiled agents.**
   Pi-sdk made `AuthStorage::from_env()` a compile error; the
   only path is `from_env_explicit(allowlist)`. Should
   compiled agents support a `--auth-from-env-all` debug flag
   that opts into the broader scan (for local dev only)?
   Recommendation: NO. The flag exists in `pi-coding-agent`
   already; compiled agents are the production path. Local dev
   uses the `pi` binary.

4. **Should `pi-build` build the binary itself, or only emit the
   Cargo project?** Two ergonomic flavours:
   - `pi-build agent.toml` → emits `target-agent/` containing
     Cargo.toml + main.rs; operator runs `cargo build --release`.
   - `pi-build agent.toml --build` → emits the directory AND
     runs cargo. Convenient but couples our error reporting to
     cargo's.

   Recommendation: ship `--build` from v1 (operator convenience
   beats purity), but the core verb only emits the directory.

## References

- **RFD 0021** — pi-orchestrate-mode (current headless story;
  a runner, not a binary distribution).
- **RFD 0023** — Sandbox MicroVM (provides
  `MicroVmProvider`; compiled-agents v1 won't use it; the
  `[runtime] sandbox = "microvm"` syntax unlocks once RFD 0023
  Commit G ships + the not-yet-RFD'd "contextfs" fs-mount
  library API lands).
- **RFD 0025** — `pi --halo` autonomous loop (the consumer for
  Commit D; halo cycles become the outer loop for compiled agents).
- **RFD 0027** — pi-rs SDK (the *required* dependency; compiled
  agents are the canonical SDK consumer per RFD 0027 §1).
- **Flue** — https://flueframework.com/ (`flue build` was the
  conceptual analogue cited in RFD 0027 v0.1; mechanism diverges
  significantly).

## Revision history

- **v0.3 (2026-05-03):** rfd-critic v0.2 pass returned
  `NEEDS_REVISION` — closed v0.1's 4 critical (3 cleanly, 1
  partially) but introduced 2 new criticals while rewriting the
  Commit B template. v0.3 closes both:
  - **N1 (`WireSerializer::serialize_event` doesn't exist):**
    Verified `WireSerializer::serialize(entry: &SessionEntry)` is
    the only serializer method, AND that the channel emits
    `AgentEvent`, not `SessionEntry`. They are distinct types
    with distinct serde shapes. Reframed §Cross-cutting #4 as
    "stdout JSONL = `serde_json::to_string(&AgentEvent)`" and
    added a note explaining that `WireSerializer` is the on-disk
    `SessionEntry` format (RFD 0027 H6 hardened) — different
    from in-process events. Commit B template now imports no
    `WireSerializer`; pump body uses `serde_json::to_string(&evt)`.
    Codegen invariant #2 rewritten as an `AgentEvent` JSONL
    round-trip test. The "1 MiB cap + ANSI strip" claim removed
    from invariants — that hardening lives on `SessionEntry`,
    not on the streamed event surface.
  - **N2 (`create_agent_session` returns a tuple):** Verified
    the real signature is `Result<(AgentSessionRuntime,
    AgentSession)>`. Template now binds via `let (_runtime,
    session) = ...;` with a comment explaining `_runtime` must
    stay alive through `prompt(...).await` so the provider /
    event channel doesn't close.
  - **N3 (`§A`/`§B` collides loosely with `Commit A`/`Commit B`
    style RFD 0023 uses):** Renamed all `§A`/`§B`/`§C`/`§D` →
    `Commit A`/`Commit B`/`Commit C`/`Commit D` for vocabulary
    parity. Now identical structure to RFD 0023.
  - **N4 (`Settings::builder().provider(...).model(...).thinking(...)`
    unverified):** Verified all three exist as fluent setters at
    `pi-agent-core/src/settings.rs:555,561,567`.
  - **Critic delta #3 (codegen invariant #1 prose mismatch):**
    Tightened the wording to "MUST contain the same set of tool
    names" and specified the regression test as
    `assert_eq!(left.names(), right.names())` rather than
    pointer-identity.

- **v0.2 (2026-05-03):** rfd-critic v0.1 pass returned
  `NEEDS_REVISION` with 4 critical + 5 underspec'd + 2
  overengineered + 4 missing items. Closed:
  - **C1 (broken codegen template):** rewrote Commit B `main.rs` to
    use `ToolRegistry::with_unsafe_extras().keep_only([...])`
    instead of fictional `pi_sdk::read()` / `pi_sdk::bash()`
    bare functions. Pi-sdk doesn't re-export tool constructors;
    `keep_only` is the canonical pattern from `examples/01_minimal.rs`.
  - **C2 (sandbox bypass):** Commit B template now passes the SAME
    `ToolRegistry` instance to both `.tools(...)` AND
    `LocalProcessProvider::new(tools.clone())`. Calling
    `LocalProcessProvider::with_defaults()` would silently
    instantiate a fresh `with_unsafe_extras()` registry inside
    the sandbox, bypassing the manifest's `tools.allowlist`.
    Promoted to a Commit B hard codegen invariant with regression test.
  - **C3 (broken citation "RFD 0021 contextfs"):** RFD 0021 is
    pi-orchestrate-mode, not contextfs (verified via grep). Fixed
    both occurrences to cite RFD 0023 Commit G + Commit B as the
    real microvm gate. Noted that "contextfs" is a separate
    not-yet-RFD'd concern the user is tracking.
  - **C4 (deny_unknown_fields conflates schema_version):** Commit A's
    parser is now two-pass — pass 1 reads `schema_version` from
    a permissive shim; pass 2 enforces strict parse only if
    version matches. v2 manifests fail with `SchemaTooNew`, not
    `unknown field`.
  - **Underspec'd: split-files convention.** Dropped Open Q #1;
    converted from "split into 4 separate RFD files" to "single
    document with Commit A/Commit B/Commit C/Commit D sub-commits" matching RFD 0023's
    pattern. Renamed all `0028A`/`0028B`/`0028C`/`0028D` →
    `Commit A`/`Commit B`/`Commit C`/`Commit D` throughout.
  - **Underspec'd: Commit D invented halo cycle-kind plug-in.** Halo
    today has no cycle-kind dispatch trait; reframed Commit D as
    "halo spawns the binary the same way it spawns
    `pi --orchestrate` today" (verified RFD 0025 Commit Composition).
  - **Underspec'd: per-tool config plumbing.** Dropped
    `[tools.bash] timeout_ms` from Commit A sketch — pi-tools-core
    today reads tool params from per-invocation JSON, not from
    registration-time config. Reserved for v2.
  - **Underspec'd: JSONL event pump missing from Commit B template.**
    Added the `event_rx` pump to the codegen template — without
    it, `--jsonl` mode emits nothing and Commit D's spend attribution
    breaks.
  - **Overengineered: Commit C reproducibility / verify / migrate.**
    Deferred all three to v2; Commit C v1 ships only profile + `--target`
    pass-through. LoC estimate dropped from 400 → 150.
  - **Overengineered: Open Q #6 `pi-build migrate`.** Dropped —
    migration tooling for a one-version schema is premature.
  - **Missing: provider-list constraint, AGENTS.md policy,
    stdout/stderr separation, tokio runtime flavour.** Added as
    Commit Cross-cutting #7-#10. Each is a hard contract for Commit B codegen.
  - **Citation accuracy fixes:** `LocalProcessProvider::new(tools)`
    is the actual constructor (with_defaults instantiates a fresh
    registry); halo invokes `pi --orchestrate`, not
    `pi-coding-agent`; bash `BASH_MAX_TIMEOUT_MS` is the max
    clamp, default is 120_000 ms (acknowledged but not cited in
    v0.2 since Commit A's `tools.bash.timeout_ms` was dropped).
- **v0.1 (2026-05-03):** Initial draft. Establishes the split
  (A=manifest, B=codegen, C=dist, D=halo), the cross-cutting
  locked choices (TOML, Cargo project, env-only secrets,
  WireSerializer JSONL, exit-code contract, v1 builtin-only
  tools), and the dependency order.
