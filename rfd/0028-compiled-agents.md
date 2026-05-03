# RFD 0028 — Compiled agents from TOML manifest (meta + split into A/B/C/D)

- **Status:** Draft (v0.1)
- **Author:** Giuseppe Massaro (drafted with claude-opus-4-7)
- **Created:** 2026-05-03
- **Implemented:** *(pending sub-RFDs 0028A–D)*

## Summary

`pi-build my-agent.toml` compiles a declarative TOML manifest into a
standalone Rust binary that embeds `pi-sdk` (RFD 0027). The binary
takes a prompt on stdin / `--prompt`, runs one or more agent turns
against the configured provider, and exits. Solves pi-rs's
"headless distribution" gap: today operators either (a) run the
`pi` interactive CLI (40+ flags, not designed for production) or
(b) hand-write a Rust embedder against `pi-sdk` (correct but
tedious). Compiled agents make (b) declarative.

This RFD is the **meta-decision** doc that splits the work into
four implementable sub-RFDs and locks the cross-cutting choices
they share. Each sub-RFD ships independently:

- **0028A** — Manifest schema (`pi.toml` + `agent.toml`).
- **0028B** — Codegen + runtime shape (what `pi-build` generates).
- **0028C** — Distribution (how the resulting binary is shipped).
- **0028D** — Halo integration (RFD 0025) — compiled agents as
  autonomous-loop cycle nodes.

## Background

### Today's headless story is anaemic

- **`pi-coding-agent` CLI** — 40+ flags (`--print`, `--no-tools`,
  `--provider`, `--thinking`, etc.). Designed for interactive use
  + admin verbs. Production operators use it via shell scripts
  that string-paste flags; surface is unstable enough that we
  refuse to commit to a 1.0 (RFD 0027 §Background).
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

| Sub-RFD | Owns | Blocks | Dep on |
|---|---|---|---|
| **0028A** | Manifest schema (TOML grammar, validation, versioning) | 0028B, 0028C, 0028D | RFD 0027 (SDK surface) |
| **0028B** | Codegen + runtime (`pi-build` verb, generated `main.rs` shape, exit-code contract, JSONL stdout protocol) | 0028D | 0028A, RFD 0027 |
| **0028C** | Distribution (cargo profile, cross-compile matrix, reproducibility, signing-deferred-to-RFD-003N) | — | 0028A, 0028B |
| **0028D** | Halo integration — compiled agents as halo cycle nodes (RFD 0025 §Composition) | — | 0028A, 0028B, RFD 0025 |

Implementation order:

```
A ──▶ B ──┬──▶ C
          └──▶ D
```

A and B are the load-bearing pair; C + D land in parallel after B.

### Cross-cutting choices (locked by this meta-RFD)

These constrain every sub-RFD. Sub-RFDs may not unilaterally
override them; changes require revising this meta-RFD first.

#### 1. Manifest format = TOML, not YAML

- TOML matches the rest of pi-rs (`.pi/halo.toml`,
  `compatibility.toml`, `Cargo.toml`). One serialisation library
  (`toml` crate), one mental model.
- YAML's allure is matching Anthropic Skills + LangChain
  manifests, but YAML's complexity (anchors, multi-doc, type
  coercion) is a security surface compiled agents don't need.
- Schema validation uses `serde` derive + `#[serde(deny_unknown_fields)]`
  so a typo'd key fails at parse time, not silently.

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

#### 4. Stdout wire format = pi-sdk's WireSerializer JSONL

- When invoked with `--jsonl`, the agent emits one JSON object
  per line on stdout, identical to pi-sdk's
  `SessionEntryKind` JSONL stream (RFD 0027 H6).
- Operators (halo, orchestrate, ad-hoc shell pipelines) parse
  the same wire format pi-sdk already stabilises. Zero
  duplicate format work.
- Default mode (no `--jsonl`) is plain UTF-8 text — assistant
  output only, no metadata.

#### 5. Exit codes = numeric stability contract

| Code | Meaning |
|---|---|
| 0 | Turn completed successfully. |
| 1 | Agent error (provider failure, tool error, budget exceeded). |
| 2 | Auth error (missing required env var; `MissingAuth`). |
| 3 | Tool-budget guard tripped (per-session token cap or per-turn invocation cap). |
| 64-78 | Reserved for sysexits.h codes (usage error, etc.). |
| 128+ | Signal exit (per POSIX). |

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

### Sketch — sub-RFD scopes

#### 0028A — Manifest schema

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

[tools.bash]                      # per-tool overrides (subset only)
timeout_ms = 300_000              # clamp lower than pi-sdk's H4 default of 600s

[runtime]
system_prompt = """
You are a flaky-test bisector. Identify the seed line.
"""
max_session_tokens          = 200_000   # H2 caps; reasonable defaults applied if absent.
max_tool_invocations_per_turn = 50
max_recursion               = 4
```

0028A's deliverable: this schema + a `pi-build validate
agent.toml` verb + serde types + round-trip test. ~600 LoC.

#### 0028B — Codegen + runtime

`pi-build my-agent.toml [--out target-dir]` walks the manifest
and emits a Cargo project. `main.rs` template (sketch, exact
output frozen by 0028B):

```rust
// CODE GENERATED by pi-build {version} from agent.toml hash {sha256}.
// DO NOT EDIT. Regenerate via `pi-build agent.toml`.
use pi_sdk::{
    AgentEventKind, AgentSessionRuntime, AuthMethod, AuthStorage,
    LocalProcessProvider, ModelRegistry, RuntimeConfig, SessionManager,
    Settings, ThinkingSetting, ToolRegistry,
};
use std::sync::Arc;

#[tokio::main]
async fn main() -> std::process::ExitCode {
    let auth = match AuthStorage::from_env_explicit([
        ("anthropic", "ANTHROPIC_API_KEY"),
    ]) {
        Ok(a) => a,
        Err(_) => return std::process::ExitCode::from(2),
    };
    let mut tools = ToolRegistry::new();
    tools.register(Arc::new(pi_sdk::read())).unwrap();
    tools.register(Arc::new(pi_sdk::grep())).unwrap();
    tools.register(Arc::new(pi_sdk::find())).unwrap();
    tools.register(Arc::new(pi_sdk::ls())).unwrap();
    tools.register(Arc::new(pi_sdk::bash())).unwrap();
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
        .with_sandbox_provider(Arc::new(LocalProcessProvider::with_defaults()))
        .with_max_session_tokens(200_000)
        .with_max_tool_invocations_per_turn(50)
        .with_max_recursion(4)
        .build()
        .expect("compile-time-validated config");
    // [...] CLI parse, prompt collection, run, exit-code map.
    std::process::ExitCode::SUCCESS
}
```

0028B's deliverable: the `pi-build` binary (lives in
`crates/pi-build/`), the codegen template (built into the
binary), the JSONL stdout protocol contract, and the
exit-code mapper. ~1200 LoC.

#### 0028C — Distribution

How the operator ships the resulting binary. Topics:

- **Cargo profile.** Default to `release` + `lto = "thin"` +
  `strip = true`. Adds 2-3 minutes to the build but produces a
  trimmer artifact.
- **Cross-compile matrix.** `pi-build --target aarch64-apple-darwin`
  forwards to `cargo build --target ...`. Operator must have
  the target installed (`rustup target add`); pi-build doesn't
  bundle toolchains.
- **Reproducibility.** Same pi-sdk version + same manifest hash
  + same target = bit-identical binary if `SOURCE_DATE_EPOCH`
  is set. `pi-build verify <binary>` re-runs codegen + diffs.
- **Out of scope for 0028C:** signing (Sigstore / cosign) →
  future RFD; container images → user wraps the binary in their
  own Dockerfile; package-manager distribution (apt, brew) →
  operator's choice.

0028C's deliverable: docs + `--target` flag + `verify` verb
+ reproducibility test. ~400 LoC.

#### 0028D — Halo integration

Halo (RFD 0025) is pi-rs's autonomous-loop supervisor. Today
each halo cycle invokes `pi-coding-agent` with a fixed flag
set. Compiled agents become first-class cycle nodes:

```toml
# halo.toml — supervisor config.
[[cycle]]
kind   = "compiled-agent"
binary = "./fix-flaky-tests"      # local path or $PATH lookup
prompt = "Audit yesterday's flaky CI failures and propose fixes."
on_exit = { 0 = "continue", 1 = "alert", 3 = "throttle" }
```

Halo:

1. Spawns the compiled-agent binary in a halo-owned worktree
   (per RFD 0025 §Halo-owned clone precondition).
2. Streams its `--jsonl` stdout into the halo cycle log.
3. Maps the agent's exit code to a halo policy (continue /
   alert / throttle) per `on_exit`.
4. Attributes the agent's spend (parsed from the JSONL `Usage`
   events) to halo's daily-budget ledger.

Compiled agents are inert (they don't loop themselves) — halo
provides the outer loop. This is the killer use case: operators
write a TOML, halo runs it forever.

0028D's deliverable: halo `compiled-agent` cycle kind + JSONL
parser + spend attribution + integration test. ~600 LoC.

### What we're NOT designing

- **Multi-agent graphs.** v1 = one agent per manifest. Operators
  compose graphs by chaining halo cycles or shell pipelines
  (`agentA | agentB`). Native graph syntax revisited at v2.
- **Long-running agent processes.** v1 = one prompt → one exit.
  No persistent server mode, no `--listen`. Halo + cron supply
  the "keep running" semantics.
- **Custom Rust tools at compile time.** Reserved
  `[[tool.kind = "rust"]]` syntax in 0028A but rejected by the
  v1 parser. v2 work.
- **Microvm sandbox integration.** Blocked on RFD 0021
  (contextfs / fs-mounting acceptance). Compiled agents in v1
  use `LocalProcessProvider` only; the manifest reserves
  `[runtime] sandbox = "microvm"` syntax but the v1 parser
  rejects it. Sub-RFD added when 0021 + RFD 0023 Commit G land.
- **MCP server adapters.** pi-sdk doesn't ship MCP yet (RFD 0027
  Open Question; binary-side concern). Compiled agents inherit
  the same boundary. Future RFD bridges if demand surfaces.

## Test plan

This meta-RFD's verification is delegated to the sub-RFDs; each
ships its own test-plan section. Cross-cutting tests that
verify the *split* itself works:

- **End-to-end "dice oracle"** — `examples/dice-oracle.toml` →
  `pi-build` → cargo build → `./dice-oracle "roll a d20"` returns
  text on stdout, exit code 0. Exercises 0028A + 0028B together.
- **Reproducibility integration test** — same manifest + pinned
  pi-sdk + `SOURCE_DATE_EPOCH=0` produces SHA-256-identical
  binary on two consecutive builds. Exercises 0028C.
- **Halo cycle test** — halo.toml configures a compiled agent
  with a deterministic MockProvider; halo runs N cycles; assert
  cycle log captures the agent's JSONL output. Exercises 0028D.
- **Manifest forward-compat** — a v1 parser MUST reject a
  manifest with `schema_version = 2` (don't silently accept).
  Exercises 0028A's versioning contract.

## Out of scope (this meta-RFD only)

- The detailed serde shape of `agent.toml` — defined in 0028A.
- The byte-exact codegen template — defined in 0028B.
- Specific cross-compile target list — defined in 0028C.
- Halo's `on_exit` policy table semantics — defined in 0028D.

## Open questions

1. **Sub-RFD numbering.** Current pi-rs convention: RFD 0023
   uses sub-commits (A1, A2, A3, A4) within one document, not
   separate RFD files. RFD 0028 splits across *separate* files
   (0028A.md, 0028B.md, ...). Rationale: each sub-RFD has its
   own status (READY / IMPLEMENTED), reviewer cycle, and
   citation surface. Confirm this is acceptable before drafting
   sub-RFDs — the alternative is one giant 0028 document with
   §A/§B/§C/§D headings (closer to RFD 0023's shape).

2. **`pi-build` lives where?** Three options:
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

3. **`agent.toml` location convention.** Cargo's `Cargo.toml` is
   the universal root marker. Should compiled agents adopt
   `pi-agent.toml` at repo root, or `<name>.toml` anywhere?
   Recommendation: per-file naming for v1 (`pi-build foo.toml`),
   reserve `pi-agent.toml` as the "discoverable root" convention
   for v2 (`pi-build` with no args looks for it).

4. **Provider-credential auto-discovery in compiled agents.**
   Pi-sdk made `AuthStorage::from_env()` a compile error; the
   only path is `from_env_explicit(allowlist)`. Should
   compiled agents support a `--auth-from-env-all` debug flag
   that opts into the broader scan (for local dev only)?
   Recommendation: NO. The flag exists in `pi-coding-agent`
   already; compiled agents are the production path. Local dev
   uses the `pi` binary.

5. **Should `pi-build` build the binary itself, or only emit the
   Cargo project?** Two ergonomic flavours:
   - `pi-build agent.toml` → emits `target-agent/` containing
     Cargo.toml + main.rs; operator runs `cargo build --release`.
   - `pi-build agent.toml --build` → emits the directory AND
     runs cargo. Convenient but couples our error reporting to
     cargo's.

   Recommendation: ship `--build` from v1 (operator convenience
   beats purity), but the core verb only emits the directory.

6. **Versioning the manifest schema.** `schema_version = 1` per
   the sketch above. When 0028A v2 lands (e.g., adds custom
   tools), what migration help do we ship? Recommendation:
   `pi-build migrate agent.toml` reads the file, identifies its
   `schema_version`, applies a series of migration passes
   defined in 0028A, writes back. Same pattern as Cargo's
   resolver-version migrations.

## References

- **RFD 0021** — pi-orchestrate-mode (current headless story;
  a runner, not a binary distribution).
- **RFD 0023** — Sandbox MicroVM (provides
  `MicroVmProvider`; compiled-agents v1 won't use it; v2
  blocked on RFD 0021 contextfs).
- **RFD 0025** — `pi --halo` autonomous loop (the consumer for
  0028D; halo cycles become the outer loop for compiled agents).
- **RFD 0027** — pi-rs SDK (the *required* dependency; compiled
  agents are the canonical SDK consumer per RFD 0027 §1).
- **Flue** — https://flueframework.com/ (`flue build` was the
  conceptual analogue cited in RFD 0027 v0.1; mechanism diverges
  significantly).

## Revision history

- **v0.1 (2026-05-03):** Initial draft. Establishes the split
  (A=manifest, B=codegen, C=dist, D=halo), the cross-cutting
  locked choices (TOML, Cargo project, env-only secrets,
  WireSerializer JSONL, exit-code contract, v1 builtin-only
  tools), and the dependency order. Sub-RFD drafts follow.
