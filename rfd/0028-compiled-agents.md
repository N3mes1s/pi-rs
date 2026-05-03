# RFD 0028 — Compiled agents from TOML manifest (meta + split into A/B/C/D)

- **Status:** Draft (v0.12; meta READY in v0.4, Commit A READY in v0.5, Commit B READY in v0.8, Commit C READY in v0.9, Commit D v0.11 spec pending critic)
- **Author:** Giuseppe Massaro (drafted with claude-opus-4-7, revised after rfd-critic v0.1, v0.2, v0.3, Commit A v1, Commit A v0.5, Commit B v1, Commit B v0.7 passes)
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
but they share §Cross-cutting choices and §Out of scope:

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

### Sub-commits

#### Commit A — Manifest schema

##### A.1 — Surface example

A complete `agent.toml` for v1, exercising every field:

```toml
# agent.toml — compiled-agent manifest, v1.
schema_version = 1                # REQUIRED. Bumped on breaking changes.

[agent]
name        = "fix-flaky-tests"   # snake-or-hyphen [a-z0-9-_]+, also the binary name.
description = "Auto-bisects flaky test runs."
version     = "0.1.0"             # SemVer; baked into the binary's `--version`.

[provider]
name     = "anthropic"            # one of: anthropic | openai | openai-compat | google | bedrock | azure-openai
model    = "claude-haiku-4-5-20251001"
thinking = "medium"               # off | low | medium | high | xhigh

[secrets]
required = ["ANTHROPIC_API_KEY"]  # env vars to allowlist in AuthStorage

[tools]
allowlist        = ["read", "grep", "find", "ls", "bash"]
disallow_unsafe  = false          # if true, REJECTS the manifest at parse time if
                                  # allowlist contains `bash`/`write`/`edit`.

[runtime]
system_prompt = """
You are a flaky-test bisector. Identify the seed line.
"""
max_session_tokens             = 200_000  # default 10_000_000 (pi-sdk H2 default).
max_tool_invocations_per_turn  = 50       # default 64 (pi-sdk H2 default).
max_recursion                  = 4        # default 8 (pi-sdk H2 default).
```

##### A.2 — Crate layout

A new workspace member `crates/pi-build/` containing:

```
crates/pi-build/
├── Cargo.toml
└── src/
    ├── lib.rs               # public API (parse + validate); used by Commit B's codegen.
    ├── manifest.rs          # serde structs (this section).
    ├── error.rs             # ManifestError enum.
    ├── parse.rs             # two-pass parser.
    └── bin/
        └── pi-build.rs      # CLI entry (`pi-build validate <toml>`, etc.).
```

Commit A delivers `manifest.rs` + `error.rs` + `parse.rs` + the
`pi-build validate <toml>` verb. Codegen (Commit B) consumes
`pi_build::manifest::Manifest` directly.

##### A.3 — Serde types (canonical surface)

```rust
// crates/pi-build/src/manifest.rs

use serde::{Deserialize, Serialize};

/// Top-level manifest, v1.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]   // v1 strict — see A.5 for the two-pass parse.
pub struct Manifest {
    pub schema_version: u32,    // MUST equal 1 for v1; checked in pass 2.
    pub agent:    AgentMeta,
    pub provider: ProviderConfig,
    #[serde(default)]
    pub secrets:  SecretsConfig,
    #[serde(default)]
    pub tools:    ToolsConfig,
    pub runtime:  RuntimeConfig,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AgentMeta {
    pub name:        String,    // pattern: ^[a-z][a-z0-9_-]{0,63}$
    pub description: String,    // 1-1024 chars; non-empty.
    pub version:     String,    // SemVer; parsed via `semver::Version::parse`.
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProviderConfig {
    pub name:  ProviderName,
    pub model: String,          // 1-256 chars; provider-side validity not checked.
    #[serde(default)]
    pub thinking: ThinkingLevel,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ProviderName {
    Anthropic, Openai, OpenaiCompat, Google, Bedrock, AzureOpenai,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ThinkingLevel {
    #[default] Off, Low, Medium, High, Xhigh,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SecretsConfig {
    #[serde(default)]
    pub required: Vec<String>,  // each MUST match `^[A-Z][A-Z0-9_]*$`.
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ToolsConfig {
    #[serde(default = "default_tool_allowlist")]
    pub allowlist:       Vec<String>,
    #[serde(default)]
    pub disallow_unsafe: bool,
}

fn default_tool_allowlist() -> Vec<String> {
    vec!["read".into(), "grep".into(), "find".into(), "ls".into()]
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RuntimeConfig {
    pub system_prompt: String,    // 1-65_536 chars; required.
    #[serde(default = "default_max_session_tokens")]
    pub max_session_tokens: u64,
    // u64 (not usize) on the wire so manifests are platform-portable;
    // Commit B lowers via `usize::try_from(n).expect(...)` against the
    // pi-sdk builder methods which take `usize` (verified
    // pi-agent-core/src/runtime.rs:386,394).
    #[serde(default = "default_max_tool_invocations_per_turn")]
    pub max_tool_invocations_per_turn: u64,
    #[serde(default = "default_max_recursion")]
    pub max_recursion: u64,
}

fn default_max_session_tokens()             -> u64 { 10_000_000 }
fn default_max_tool_invocations_per_turn()  -> u64 { 64 }
fn default_max_recursion()                  -> u64 { 8 }
```

The defaults match pi-sdk's `RuntimeConfig::default()`
(`pi-agent-core/src/runtime.rs:445-447` — `max_session_tokens =
10_000_000`, `max_tool_invocations_per_turn = 64`,
`max_recursion = 8`), so an omitted block produces the same caps
as a hand-built embedder.

**Wire vs runtime types:** `max_tool_invocations_per_turn` and
`max_recursion` are `u64` on the manifest wire but `usize` in
pi-sdk's runtime fields. Commit B's codegen lowers via
`usize::try_from(n)?` and surfaces an `OutOfRange` error if the
manifest specified a value beyond the host platform's `usize::MAX`
(would only matter on a hypothetical 16-bit target). `u64` here
is deliberate — keeps the schema platform-portable.

**ProviderName wire-form parity:** the `kebab-case` rename emits
`"anthropic" | "openai" | "openai-compat" | "google" | "bedrock"
| "azure-openai"` — identical to the strings pi-sdk's
`Settings.provider: String` expects (verified
`pi-ai/src/auth.rs:102`, `pi-ai/src/registry.rs:711`). Commit B's
codegen passes them through as `serde_json::to_string(&p.name)`-
equivalent string slices with no remapping.

##### A.4 — Validation rules (post-parse, semantic)

After serde succeeds, `parse.rs` runs `validate(&mut Manifest) ->
Result<(), ManifestError>` (mutable so it can apply the silent
`tools.allowlist` dedup before returning to Commit B):

| Rule | Failure |
|---|---|
| `schema_version == 1` | `SchemaTooNew { found, supported: 1 }` if `> 1`; `SchemaTooOld { found }` if `< 1` (i.e., `0`). |
| `agent.name` matches `^[a-z][a-z0-9_-]{0,63}$` (case-sensitive, lowercase only) | `InvalidAgentName(name)`. |
| `agent.description.len()` in `1..=1024` (UTF-8 bytes, not chars) | `InvalidDescription { len }`. |
| `agent.version` parses as `semver::Version` | `InvalidVersion(name, e)`. |
| `provider.name` is a valid `ProviderName` enum variant | enforced at serde layer; produces `Parse(unknown variant '...')`. |
| `provider.thinking` is a valid `ThinkingLevel` variant | enforced at serde layer (closed enum); no semantic rule. |
| `provider.model.len()` in `1..=256` (UTF-8 bytes) | `InvalidModelLen { len }`. |
| every `secrets.required[i]` matches `^[A-Z][A-Z0-9_]*$` (case-sensitive, uppercase only) | `InvalidEnvVarName(name)`. |
| every `tools.allowlist[i]` ∈ `{read, write, edit, bash, grep, find, ls, web_search}` (case-sensitive, lowercase only — `"Read"` produces `UnknownTool("Read")`, no normalization) | `UnknownTool(name)`. |
| if `tools.disallow_unsafe`, `allowlist ∩ {bash, write, edit} == ∅` | `UnsafeToolWithDisallow(name)`. |
| `tools.allowlist` is non-empty after silent dedup | `EmptyAllowlist`. |
| `runtime.system_prompt.len()` in `1..=65_536` (UTF-8 bytes) | `InvalidSystemPromptLen { len }`. |
| `runtime.max_session_tokens` ≥ 1_000 | `MaxSessionTokensTooLow { found }`. |
| `runtime.max_tool_invocations_per_turn` ≥ 1 | `MaxInvocationsTooLow`. |
| `runtime.max_recursion` in `1..=16` | `MaxRecursionOutOfRange { found }`. |
| `usize::try_from(max_tool_invocations_per_turn).is_ok()` | `OutOfRangeForUsize { field, found }`. |
| `usize::try_from(max_recursion).is_ok()` | `OutOfRangeForUsize { field, found }`. |

Validation is total — every error variant carries the bad input
so the CLI prints the offending value, not just the rule name.

**Dedup behavior:** `tools.allowlist` is silently de-duplicated
in `validate(&Manifest)` before the `EmptyAllowlist` check —
duplicates are NOT a parse error, just a no-op. The `Manifest`
returned to Commit B carries the de-duplicated `Vec<String>`.

**Enum-validation note:** `ProviderName` and `ThinkingLevel` are
closed enums with `deny_unknown_fields`-equivalent semantics at
the serde layer (an unknown variant produces a serde
`unknown variant 'foo', expected one of ...` error which the
parser surfaces as `ManifestError::Parse`). No separate
validation rule needed.

##### A.5 — Two-pass parser (per §Cross-cutting #1)

`parse.rs` does NOT call `toml::from_str::<Manifest>` directly.
Instead:

```rust
// crates/pi-build/src/parse.rs

#[derive(Deserialize)]
struct VersionShim {
    schema_version: u32,
    // No #[serde(deny_unknown_fields)] — accept any v2+ shape.
}

pub fn parse(raw: &str) -> Result<Manifest, ManifestError> {
    // PASS 1: detect schema_version with a permissive shim. Ignores
    // unknown keys so a v2 manifest fails with `SchemaTooNew`, not
    // `unknown field 'foo'`.
    let v: VersionShim = toml::from_str(raw)
        .map_err(ManifestError::VersionDetect)?;
    match v.schema_version {
        1 => {}
        0 => return Err(ManifestError::SchemaTooOld { found: 0 }),
        n => return Err(ManifestError::SchemaTooNew { found: n, supported: 1 }),
    }
    // PASS 2: strict v1 parse with deny_unknown_fields.
    let m: Manifest = toml::from_str(raw).map_err(ManifestError::Parse)?;
    validate(&m)?;
    Ok(m)
}
```

Tests (Commit A regression):

- `schema_version = 2` + a v2-introduced key fails with
  `ManifestError::SchemaTooNew { found: 2, supported: 1 }` —
  NOT `ManifestError::Parse(unknown field 'foo')`.
- `schema_version = 0` fails with
  `ManifestError::SchemaTooOld { found: 0 }`.
- `schema_version = -1` (or any non-`u32`-representable integer)
  fails at the toml layer surfaced as
  `ManifestError::VersionDetect(_)`.
- A file containing only invalid UTF-8 bytes fails as
  `ManifestError::VersionDetect(_)` (toml parse fails before
  the shim deserializes).

##### A.6 — Error type

```rust
// crates/pi-build/src/error.rs

use thiserror::Error;

#[derive(Debug, Error)]
pub enum ManifestError {
    #[error("manifest schema_version {found} is newer than this pi-build supports (max {supported}); upgrade pi-build")]
    SchemaTooNew { found: u32, supported: u32 },

    #[error("manifest schema_version {found} is older than v1 (no v0 schema exists)")]
    SchemaTooOld { found: u32 },

    #[error("could not detect schema_version: {0}")]
    VersionDetect(toml::de::Error),

    #[error("manifest parse error: {0}")]
    Parse(toml::de::Error),

    #[error("invalid agent.name {0:?}: must match ^[a-z][a-z0-9_-]{{0,63}}$")]
    InvalidAgentName(String),

    #[error("invalid agent.description length {len} (must be 1..=1024)")]
    InvalidDescription { len: usize },

    #[error("invalid agent.version {0:?}: {1}")]
    InvalidVersion(String, semver::Error),

    #[error("invalid provider.model length {len} (must be 1..=256)")]
    InvalidModelLen { len: usize },

    #[error("invalid env-var name {0:?} in secrets.required: must match ^[A-Z][A-Z0-9_]*$")]
    InvalidEnvVarName(String),

    #[error("unknown tool {0:?} in tools.allowlist (v1 supports: read, write, edit, bash, grep, find, ls, web_search)")]
    UnknownTool(String),

    #[error("tool {0:?} is unsafe but tools.disallow_unsafe = true")]
    UnsafeToolWithDisallow(String),

    #[error("tools.allowlist is empty after dedup")]
    EmptyAllowlist,

    #[error("invalid runtime.system_prompt length {len} (must be 1..=65_536)")]
    InvalidSystemPromptLen { len: usize },

    #[error("runtime.max_session_tokens {found} below floor 1_000")]
    MaxSessionTokensTooLow { found: u64 },

    #[error("runtime.max_tool_invocations_per_turn must be ≥ 1")]
    MaxInvocationsTooLow,

    #[error("runtime.max_recursion {found} out of range 1..=16")]
    MaxRecursionOutOfRange { found: u64 },

    #[error("runtime.{field} = {found} exceeds usize::MAX on this host")]
    OutOfRangeForUsize { field: &'static str, found: u64 },
}
```

##### A.7 — `pi-build validate` CLI verb

```text
pi-build validate <path/to/agent.toml>

  Reads the manifest, runs the two-pass parser + semantic
  validation, and prints either:

    OK: <name> <version> (<provider>/<model>) — <N> tools allowlisted

  on success, or the ManifestError's Display on failure (one line,
  with the offending value). Exit 0 on success, 65 (`EX_DATAERR`)
  on validation failure.
```

This verb is the unit-test surface for the whole manifest layer.
CI runs `pi-build validate examples/*.toml` over a fixture set
covering every valid + every error variant.

##### A.8 — Test plan

- **Round-trip:** `toml::to_string(&Manifest)` then `parse` returns
  an equal `Manifest` (PartialEq+Eq derived on every type per A.3).
- **Schema-version-too-new:** `schema_version = 2 \n agent.name =
  "x"` (with all v1 keys present) fails with `SchemaTooNew`,
  not `Parse(unknown field)`.
- **Schema-version-too-old:** `schema_version = 0` fails with
  `SchemaTooOld { found: 0 }`.
- **Defaults applied:** a manifest omitting `[secrets]`, `[tools]`,
  and `runtime.max_*` fields parses cleanly with defaults
  (`required = []`, `allowlist = ["read","grep","find","ls"]`,
  `max_session_tokens = 10_000_000`,
  `max_tool_invocations_per_turn = 64`, `max_recursion = 8`).
- **disallow_unsafe rejects bash:** `tools.allowlist = ["bash"]
  + tools.disallow_unsafe = true` fails with
  `UnsafeToolWithDisallow("bash")`.
- **Tool-name case sensitivity:** `tools.allowlist = ["Read"]`
  fails with `UnknownTool("Read")` (no normalization).
- **Allowlist dedup:** `tools.allowlist = ["read", "grep", "read"]`
  parses to a `Vec` of `["read", "grep"]` after `validate()`;
  no error.
- **Length boundaries — accept:** description = exactly 1024
  bytes, system_prompt = exactly 65_536 bytes, model = exactly
  256 bytes all parse + validate cleanly.
- **Length boundaries — reject:** each of the above + 1 byte
  fails with the matching `Invalid*Len` variant carrying the
  exact length.
- **`max_recursion` boundaries:** 1, 8, 16 accept; 0 fails with
  `MaxRecursionOutOfRange { found: 0 }`; 17 fails with
  `MaxRecursionOutOfRange { found: 17 }`.
- **`OutOfRangeForUsize`:** a manifest with
  `max_tool_invocations_per_turn = 18_446_744_073_709_551_615`
  (`u64::MAX`) on a 32-bit target (or any host where `u64 >
  usize::MAX`) fails with `OutOfRangeForUsize`. CI runs the
  test conditionally on `cfg(target_pointer_width = "32")` so
  it's a no-op on 64-bit hosts but compiles everywhere.
- **Empty / garbage:**
  - empty file → `VersionDetect(_)` (toml's "missing field
    `schema_version`").
  - file with only `schema_version = 1` (and no other required
    blocks) → `Parse(_)` ("missing field `agent`").
  - file containing invalid UTF-8 bytes → `VersionDetect(_)`.
- **Per-error fixture file:** one `.toml` per `ManifestError`
  variant under `crates/pi-build/tests/fixtures/invalid/`; the
  test sweeps and asserts the exact variant via
  `matches!(err, ManifestError::Foo { .. })`.

##### A.9 — Out of scope (explicitly noted for future commits)

- **Per-tool config blocks** (e.g., `[tools.bash] timeout_ms = 30_000`)
  — pi-tools-core today reads tool params from per-invocation
  JSON, not from registration time; manifest-time overrides need
  pi-tools API changes. Reserved syntax: `[tools.<name>]` table
  rejected in v1 by serde's `deny_unknown_fields` on `ToolsConfig`
  (`ManifestError::Parse(unknown field 'bash')`; the toml-rs
  error span points at the `[tools.bash]` line, but the field
  name in the message is `bash` not `tools.bash`).
- **Custom Rust tools** (`[[tool]] kind = "rust" path = "..."`)
  — v2.
- **Sandbox provider selection** (`[runtime] sandbox = "microvm"`)
  — gated on RFD 0023 Commit G + the not-yet-RFD'd contextfs
  fs-mount library API.
- **MCP server adapters** (`[[mcp]] command = "..."`) — pi-sdk
  doesn't ship MCP yet; future RFD.
- **Multi-agent manifests** (`[[agent]] [[agent]]`) — v2.

Commit A's deliverable: `crates/pi-build/{Cargo.toml, src/lib.rs,
src/manifest.rs, src/error.rs, src/parse.rs, src/bin/pi-build.rs}`
+ `tests/fixtures/{valid,invalid}/*.toml` + the regression tests
listed in A.8. ~600 LoC total (manifest 180 + error 80 + parse 100
+ CLI 80 + tests 160).

#### Commit B — Codegen + runtime

##### B.1 — `pi-build` CLI shape

Commit A ships `pi-build validate <toml>` (the manifest unit-test
surface). Commit B adds the codegen verb:

```text
pi-build <agent.toml> [OPTIONS]

  Generate a Cargo project from <agent.toml>. Default sub-command
  when invoked with a path arg.

  --out <DIR>      Output directory. Default: <agent_name>-build/.
                   Refuses to overwrite a non-empty directory unless
                   --force is passed.
  --force          Overwrite the output directory if it exists.
  --build          After codegen, run `cargo build --release` in the
                   output directory. Forwards stdout/stderr.
  --target <T>     Cross-compile target (forwarded to cargo). Implies
                   --build. Operator MUST have the target installed
                   via `rustup target add <T>`.
  --release | --debug
                   Cargo profile. Default --release. Implies --build.
  -h, --help

  Exits:
    0   codegen + (optional) build succeeded.
   64   bad CLI usage (EX_USAGE).
   65   manifest parse / validation failed (EX_DATAERR; same as
        `pi-build validate`).
   73   I/O error writing the output dir (EX_CANTCREAT).
   75   cargo build failed (EX_TEMPFAIL).
```

Other Commit A verbs unchanged:

```text
pi-build validate <agent.toml>     # A.7
pi-build verify   <binary>         # v2 (Commit C deferral)
pi-build migrate  <agent.toml>     # v2 (Commit C deferral)
```

##### B.2 — Codegen flow

```
        ┌──────────────────────────────────────────┐
        │ 1. parse(manifest_toml) -> &mut Manifest │  (Commit A)
        │ 2. compute manifest_sha256               │
        │ 3. render(Manifest, sha256) -> CargoTree │  (Commit B)
        │    - Cargo.toml                          │
        │    - src/main.rs                         │
        │ 4. write CargoTree to --out              │
        │ 5. if --build: spawn cargo               │
        └──────────────────────────────────────────┘
```

`render` is pure: same `(Manifest, sha256, pi-build version)` →
byte-identical output. No randomness, no timestamps, no
environment leakage. This is the determinism invariant Commit C
v2 (`pi-build verify`) will hash-compare.

**Across pi-build versions:** the comment header carries the
`pi_build_version` literal, so upgrading from `pi-build 0.1.1`
to `0.1.2` may produce different bytes for the same manifest
(comment changes; the substantive code may also change if the
template was edited). Operators who need cross-version
reproducibility either pin pi-build at the same version or use
`pi-build verify` (Commit C v2) which hashes only the
post-comment region OR records the producing version explicitly
in `pi-build.lock`.

##### B.3 — Byte-exact `main.rs` template

The codegen substitutes manifest fields into a fixed template.
Substitution markers are `{{field}}`; everything else is literal.
The `MEDIUM` example below is what `pi-build` produces from the
A.1 manifest:

```rust
// CODE GENERATED by pi-build {{pi_build_version}} from agent.toml
// hash sha256:{{manifest_sha256}}.
// DO NOT EDIT. Regenerate via `pi-build agent.toml`.
//
// Agent:    {{agent_name}} v{{agent_version}}
// Provider: {{provider_name}}/{{provider_model}} (thinking={{thinking}})

use pi_sdk::{
    create_agent_session, AgentEventKind, AuthStorage, LocalProcessProvider,
    ModelRegistry, RuntimeConfig, SessionManager, Settings, ThinkingSetting,
    ToolRegistry,
};
use std::sync::Arc;

#[tokio::main(flavor = "current_thread")]
async fn main() -> std::process::ExitCode {
    // Auth: per-manifest env-var allowlist (B.6). The {{auth_call}}
    // marker expands to either:
    //   from_env_explicit([("anthropic", "ANTHROPIC_API_KEY"), ...])
    //   for non-empty `secrets.required`, OR
    //   from_env_explicit(std::iter::empty::<(&str, &str)>())
    //   for the empty case (the `[]` literal can't infer the
    //   `<I, P, E>` generics on `from_env_explicit`).
    let auth = match AuthStorage::{{auth_call}} {
        Ok(a) => a,
        Err(_) => return std::process::ExitCode::from(2),
    };

    // Tools: SAME registry instance to .tools() AND sandbox (B.5).
    let mut tools = ToolRegistry::with_unsafe_extras();
    tools.keep_only(&[
        {{tool_allowlist}}                           // e.g., "read".into(), "grep".into(),
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
                .provider({{provider_name_lit}})    // B.8: "anthropic"
                .model({{provider_model_lit}})       // "claude-haiku-4-5-20251001"
                .thinking({{thinking_lit}})          // B.9: ThinkingSetting::Medium
                .build(),
        )
        .system_prompt({{system_prompt_lit}})        // raw multi-line string literal
        .with_sandbox_provider(sandbox)
        .with_max_session_tokens({{max_session_tokens}}u64)
        .with_max_tool_invocations_per_turn({{max_tool_invocations_per_turn}}usize)
        .with_max_recursion({{max_recursion}}usize)
        .build()
        .expect("compile-time-validated config");

    // Event pump (§Cross-cutting #4): AgentEvent JSONL via serde_json.
    let jsonl = std::env::args().any(|a| a == "--jsonl");
    let pump = tokio::spawn(async move {
        while let Some(evt) = event_rx.recv().await {
            if jsonl {
                println!("{}", serde_json::to_string(&evt).unwrap());
            } else if let AgentEventKind::AssistantTextDelta { text } = &evt.kind {
                print!("{text}");
            }
            if matches!(evt.kind, AgentEventKind::TurnComplete) { break; }
        }
    });

    // Tuple binding per rfd-critic v0.3: AgentSession owns its own
    // Arc<RuntimeConfig>; _runtime is stylistic, not load-bearing.
    let (_runtime, session) = match create_agent_session(cfg, Some(event_tx)) {
        Ok(rs) => rs,
        Err(_) => return std::process::ExitCode::from(1),
    };
    let prompt = read_prompt_from_args_or_stdin();
    let exit = match session.prompt(prompt).await {
        Ok(_) => 0,
        Err(e) => map_runtime_error_to_exit(e),
    };
    let _ = pump.await;
    std::process::ExitCode::from(exit)
}

// Generated helper. Reads the prompt from (in order):
//   1. argv[1..] joined with spaces, if any present.
//   2. stdin (until EOF), if stdin is not a tty.
//   3. otherwise: print usage to stderr, exit 64.
fn read_prompt_from_args_or_stdin() -> String { /* ~30 LoC, fixed */ }

// Generated helper. Maps RuntimeError variants to exit codes
// per §Cross-cutting #5.
fn map_runtime_error_to_exit(e: pi_sdk::RuntimeError) -> u8 {
    use pi_sdk::RuntimeError::*;
    match e {
        BudgetExhausted { .. } | InvocationCapExceeded { .. } => 3,
        DepthExceeded { .. }                                  => 1,
        // ... full match per pi-sdk's RuntimeError enum; ~25 LoC.
        _                                                      => 1,
    }
}
```

##### B.4 — Generated `Cargo.toml` template

```toml
# CODE GENERATED by pi-build {{pi_build_version}} from agent.toml
# hash sha256:{{manifest_sha256}}.
[package]
name    = "{{agent_name}}"
version = "{{agent_version}}"
edition = "2021"

[dependencies]
pi-sdk      = "{{pi_sdk_caret_pin}}"     # e.g., "0.1" — caret-pin per RFD 0027 §6
tokio       = { version = "1", features = ["macros", "rt", "sync"] }   # `sync` for mpsc::unbounded_channel
serde_json  = "1"

[profile.release]
lto             = "thin"   # §C: default profile
strip           = true
codegen-units   = 1
```

The `pi_sdk_caret_pin` is bound to the pi-build binary's own
build-time pi-sdk version (read from `env!("CARGO_PKG_VERSION_*")`
at pi-build compile time). Changing pi-sdk's version in the
generated agent without regenerating via the matching pi-build
is unsupported.

##### B.5 — Tool allowlist lowering

Manifest `tools.allowlist` (already deduped + validated by
Commit A) lowers to:

```rust
tools.keep_only(&[
    {{tool_allowlist}}     // each entry: `"<name>".into(),`
]);
```

Sort order is the dedup-canonical order Commit A returns:
**insertion order of first occurrence**. Commit A's
`validate(&mut Manifest)` lowers via this std-only primitive
(no `itertools` dep):

```rust
let mut seen = std::collections::BTreeSet::<String>::new();
m.tools.allowlist.retain(|x| seen.insert(x.clone()));
```

Test: same input manifest → byte-identical `keep_only` arg
list across runs and across hosts (`Vec::retain` preserves
first-occurrence position; the membership set's iteration
order is irrelevant — `HashSet` would also work for
correctness, `BTreeSet` is just a std-only no-extra-dep
default).

If `tools.allowlist == ["read", "write", "edit", "bash", "grep",
"find", "ls", "web_search"]` (the full set), the codegen still
emits the full `keep_only` call rather than skipping it — the
explicit list is the audit surface, even when redundant.

##### B.6 — Auth allowlist lowering

**Lowering rule:** each `secrets.required[i]` pairs with
`provider.name`'s kebab string. Codegen has TWO branches:

```rust
// Non-empty case — array literal:
match secrets.required.len() {
    0 => emit("from_env_explicit(std::iter::empty::<(&str, &str)>())"),
    _ => {
        let pairs = secrets.required.iter()
            .map(|env| format!("(\"{}\", \"{}\")", provider.name.as_kebab(), env))
            .collect::<Vec<_>>().join(", ");
        emit(&format!("from_env_explicit([{pairs}])"));
    }
}
```

The empty case MUST use `std::iter::empty::<(&str, &str)>()` —
the bare `from_env_explicit([])` form fails to infer the
`<I, P, E>` generics on the function (verified
`pi-ai/src/auth.rs:144`) and emits E0282/E0283 at the agent's
own `cargo build`. Tested by Commit B's invariant 8.

The table below is the **canonical pairing reference** operators
use to author the manifest (it tells you WHICH env var to put
in `secrets.required` for a given provider). It is NOT the
codegen lowering map — codegen pairs whatever the operator
listed with whatever the operator chose for `provider.name`.
That's why a manifest with mismatched pairs (`provider.name =
"anthropic"` but `secrets.required = ["FOO"]`) emits a stderr
warning rather than a hard error: `from_env_explicit` accepts
arbitrary keys; only the operator knows whether the pairing is
intentional.

v1's canonical pairing table — the same one pi-sdk's `ENV_KEYS`
constant uses (`pi-ai/src/auth.rs:79-97`):

```text
anthropic     -> ANTHROPIC_API_KEY
openai        -> OPENAI_API_KEY
openai-compat -> (no canonical env var; manifest must list explicitly)
google        -> GOOGLE_API_KEY
bedrock       -> AWS_BEDROCK_TOKEN
azure-openai  -> AZURE_OPENAI_API_KEY
```

`secrets.required` is the **operator-allowed** set (CWE-526
defense — codegen never invents env-var names not in the
manifest). The codegen pairs each entry with the `provider.name`
that matches: `("anthropic", "ANTHROPIC_API_KEY")`. If
`secrets.required` contains an env var the codegen can't pair
with the configured provider (e.g., manifest declares
`provider = "anthropic"` but `secrets.required = ["FOO"]`),
codegen emits a parse-time warning to stderr ("`FOO` allowlisted
but not used by configured provider; consider removing") and
emits the pair with `provider = "anthropic"` anyway — pi-sdk's
`AuthStorage::from_env_explicit` accepts arbitrary keys.

##### B.7 — Settings::builder lowering

Each manifest field maps directly to a `SettingsBuilder` setter
verified to exist at `pi-agent-core/src/settings.rs`:

| Manifest field | Builder call | Setter src |
|---|---|---|
| `provider.name` | `.provider(...)` | `:555` |
| `provider.model` | `.model(...)` | `:561` |
| `provider.thinking` | `.thinking(...)` | `:567` |

##### B.8 — ProviderName → string lowering

`ProviderName` serializes to its kebab-case wire form (per
A.3's "wire-form parity" note). Codegen emits the literal
string:

```rust
.provider("anthropic")        // ProviderName::Anthropic
.provider("openai-compat")    // ProviderName::OpenaiCompat
.provider("azure-openai")     // ProviderName::AzureOpenai
```

##### B.9 — ThinkingLevel → ThinkingSetting lowering

```text
manifest ThinkingLevel    pi-sdk ThinkingSetting
─────────────────────────────────────────────────
off                       ThinkingSetting::Off
low                       ThinkingSetting::Low
medium                    ThinkingSetting::Medium
high                      ThinkingSetting::High
xhigh                     ThinkingSetting::XHigh
```

Codegen emits `ThinkingSetting::Medium` literal (NOT a
`From<ThinkingLevel> for ThinkingSetting` impl — keeps the
generated code self-contained, no per-agent crate boilerplate).

##### B.10 — `max_*` u64 → usize lowering

```rust
.with_max_session_tokens({{max_session_tokens}}u64)
.with_max_tool_invocations_per_turn({{max_tool_invocations_per_turn}}usize)
.with_max_recursion({{max_recursion}}usize)
```

Commit A's `validate(&mut Manifest)` already ran
`usize::try_from(n)?` for `max_tool_invocations_per_turn` and
`max_recursion`, returning `OutOfRangeForUsize` on a 32-bit
host with values exceeding `u32::MAX`. So by the time codegen
runs, the values fit `usize`; the literal is emitted with the
`usize` suffix to make the call site unambiguous to rustc.

##### B.11 — `read_prompt_from_args_or_stdin` helper

```rust
fn read_prompt_from_args_or_stdin() -> String {
    use std::io::{IsTerminal, Read};
    let args: Vec<String> = std::env::args().skip(1)
        .filter(|a| !a.starts_with("--"))   // drop flags like --jsonl
        .collect();
    if !args.is_empty() {
        return args.join(" ");
    }
    let stdin = std::io::stdin();
    if !stdin.is_terminal() {
        let mut buf = String::new();
        if stdin.lock().read_to_string(&mut buf).is_ok() && !buf.trim().is_empty() {
            return buf.trim().to_owned();
        }
    }
    eprintln!("usage: {{agent_name}} [--jsonl] <prompt> | echo <prompt> | {{agent_name}}");
    std::process::exit(64);   // EX_USAGE per §Cross-cutting #5
}
```

Stdout discipline: the usage banner goes to **stderr** per
§Cross-cutting #8. Stdout is reserved for agent output.

**`--` arg handling (v1):** every arg matching `^--` is dropped
unconditionally — including the literal POSIX `--`
end-of-options separator. A prompt that starts with `--` MUST
be passed via stdin: `echo '--my-prompt' | agent`. Honoring the
POSIX `--` separator (so that `agent -- --my-prompt` would
treat `--my-prompt` as the prompt) is reserved for v2.

##### B.12 — `map_runtime_error_to_exit` helper

The full enum match against `pi_sdk::RuntimeError` (per
`pi-agent-core/src/runtime.rs:1856-1912`):

```rust
fn map_runtime_error_to_exit(e: pi_sdk::RuntimeError) -> u8 {
    use pi_sdk::RuntimeError::*;
    match e {
        // §Cross-cutting #5 row "tool-budget" → exit 3.
        BudgetExhausted { .. }     => 3,
        InvocationCapExceeded { .. } => 3,
        // §Cross-cutting #5 row "agent error" → exit 1.
        DepthExceeded { .. }       => 1,
        EmptyTurn                  => 1,
        ToolPanicked { .. }        => 1,
        ToolUseFinishWithoutCalls  => 1,
        // Everything else (provider + io + serde failures) → exit 1.
        _                          => 1,
    }
}
```

Auth errors don't reach this helper — they're caught at
`AuthStorage::from_env_explicit` time and exit 2 directly
(see B.3 main).

##### B.13 — Hard codegen invariants (regression-tested)

1. **Same allowlist, both registries.** The `ToolRegistry` value
   passed to `.tools(...)` and the one inside the
   `LocalProcessProvider` MUST contain the same set of tool
   names. Test: `assert_eq!(left.names(), right.names())`.
   Failing this silently restores all 8 unsafe tools through the
   sandbox path.
2. **AgentEvent JSONL shape is stable.** Round-trip a
   representative `AgentEvent` (`AssistantTextDelta`,
   `AssistantToolCall`, `Usage`, `TurnComplete`) through
   `serde_json::to_string` then `from_str`; assert equality.
3. **Stdout discipline.** Tracing, panics, warnings → stderr.
   Stdout in `--jsonl` mode emits ONLY the serde_json output.
4. **Tokio runtime flavour.** `#[tokio::main(flavor =
   "current_thread")]` is in the generated `main.rs` literally.
5. **Codegen determinism.** Same manifest hash + same pi-build
   version → byte-identical `(Cargo.toml, src/main.rs)`. Test
   runs codegen twice and `assert_eq!` the bytes.
6. **No secrets in generated source.** Parse the rendered
   `main.rs` via `syn::parse_file` and walk the AST: every
   string literal containing `_API_KEY|_TOKEN` MUST appear
   EITHER (a) as a child of a `from_env_explicit([...])` call
   expression, OR (b) as the literal argument to
   `Settings::builder().system_prompt(...) | .model(...) |
   .provider(...)`. (Pure regex would false-positive on every
   `system_prompt` that legitimately documents an env var by
   name; the (b) clause is the necessary exemption — operators
   commonly write prompts like "Set ANTHROPIC_API_KEY before
   running this tool.") Test asserts via syn walk; pi-build
   takes a dev-dep on `syn` for tests only.
7. **Stdout vs stderr separation.** Integration test runs
   the generated binary with `--jsonl` against a MockProvider
   that emits known events; asserts stdout contains only valid
   JSONL and stderr contains zero JSONL-shaped lines.
8. **No-secret manifest produces no env reads.** A manifest
   with `secrets.required = []` codegens
   `AuthStorage::from_env_explicit(std::iter::empty::<(&str, &str)>())`
   — a typed empty iterator, NOT the literal `[]` (which would
   fail to infer the `P`/`E` generic params on
   `from_env_explicit::<I, P, E>` and emit E0282/E0283; verified
   `pi-ai/src/auth.rs:144`). Snapshot test asserts the exact
   rendered substring `from_env_explicit(std::iter::empty::<(&str, &str)>())`
   appears in `main.rs` and that `cargo check` on the generated
   project succeeds. Catches the "operator forgot to allowlist
   anything but the agent still magically authenticates"
   failure mode AND the codegen-emits-non-compiling-Rust
   regression.

##### B.14 — Test plan

- **Unit, codegen template:** snapshot test on the generated
  `main.rs` for the A.1 manifest. Stored under
  `crates/pi-build/tests/snapshots/dice-oracle.main.rs`.
- **Unit, codegen Cargo.toml:** snapshot test on the generated
  Cargo.toml for the A.1 manifest.
- **Determinism (B.13 #5):** run `pi-build` twice in tmpdirs;
  diff outputs; assert empty.
- **Build smoke:** `pi-build examples/dice-oracle.toml --build`
  produces a binary; `./target-agent/dice-oracle "roll a d20"`
  exits 2 (no `ANTHROPIC_API_KEY` set in CI). With the env var
  set + a MockProvider hooked, exits 0 + emits text.
- **`--jsonl` smoke:** same, with `--jsonl`; assert stdout is
  parseable JSONL, exactly one `Usage` event, exactly one
  `TurnComplete`.
- **Cross-target build (Commit C smoke):** `pi-build agent.toml
  --target x86_64-unknown-linux-musl --build` succeeds (CI
  installs the target with `rustup target add`).
- **Per-invariant test:** one `tests/invariant_*.rs` file per
  B.13 invariant.

##### B.15 — Out of scope (Commit B)

- **`pi-build verify <binary>`** — re-run codegen + diff (Commit C v2 per §C deferral).
- **Reproducibility across pi-build versions** — same manifest
  with pi-build 0.1 and 0.2 may produce different bytes;
  pi-build pin lives in the generated `Cargo.toml`'s pi-sdk caret.
- **In-process `pi-sdk` library API for codegen** — Commit B's
  `pi-build` is a binary, not a library. Embedders who want
  to invoke codegen programmatically file a follow-up.
- **Code-signing the generated binary** — operator's choice;
  reuses standard cargo + downstream signing tooling.

**Maintainer note:** if pi-sdk's `Settings::builder()` gains a
new `&str`-taking setter that legitimately holds operator prose
(e.g., `.guidance(...)`, `.persona(...)`), extend B.13 invariant
6 clause (b) to list it. Currently the exempt setters are
`system_prompt`, `model`, `provider`.

Commit B's deliverable: `crates/pi-build/src/{lib.rs (codegen),
codegen.rs (template engine), bin/pi-build.rs (CLI)}` + the
`tests/snapshots/` + `tests/invariant_*.rs` fleet. ~1200 LoC
(codegen 500 + template engine 300 + CLI 200 + tests 200).

#### Commit C — Distribution

##### C.1 — Scope statement

Commit C ships the **minimum distribution surface** that lets an
operator turn a generated Cargo project into a runnable binary
on a non-build host. Everything beyond that — reproducibility
verification, schema migration tooling, signing, container
recipes — defers to v2 or a follow-up RFD per the rfd-critic
v0.1 finding that the v0.1 v1 scope was overengineered for a
single-consumer (halo, on the same host) v1.

##### C.2 — `pi-build` build-related flags

Commit B's `pi-build <agent.toml>` already accepts the
build-related flags listed in B.1. Commit C's deliverable is
the *implementation* of `--build` / `--target` / `--release` /
`--debug`, not new flag surface.

```text
pi-build <agent.toml> [--out DIR] [--build] [--target T]
                      [--release | --debug] [--force]
```

| Flag | Behavior |
|---|---|
| (none) | Generate the Cargo project; do not build. Operator runs `cargo build` from the output dir. |
| `--build` | After codegen, spawn `cargo build` in the output dir; forward stdout/stderr; exit with cargo's status (mapped to 75 EX_TEMPFAIL on non-zero). |
| `--target T` | Forward `--target T` to cargo. Implies `--build`. Operator MUST have the target installed via `rustup target add T`; pi-build does NOT manage toolchains. |
| `--release` (default) | Forward `--release`. `lto = "thin"` + `strip = true` from B.4's Cargo.toml template. ~2-3 minute build cost; produces a trimmer artifact. |
| `--debug` | Forward `--profile dev`. Skip the LTO + strip overhead; useful for iterating on a manifest. |

Implementation detail: pi-build spawns cargo via
`tokio::process::Command::spawn` (Commit B already takes a
tokio dep), inheriting the operator's `PATH`, `CARGO_HOME`,
`RUSTUP_HOME`. Pi-build does NOT inject codegen flags or
`RUSTFLAGS` or `-C` overrides; profile tuning lives in the
generated `Cargo.toml` (B.4) where the operator can audit
+ override it. The ONLY flag pi-build adds beyond what the
operator passed is `--manifest-path <out>/Cargo.toml`, which
points cargo at the generated tree.

##### C.3 — Operator artifacts

After `pi-build agent.toml --build`, the output directory
contains:

```
<out>/
├── Cargo.toml             # B.4 template
├── Cargo.lock             # generated by cargo on first build
├── src/main.rs            # B.3 template
├── pi-build.lock          # NEW in C — pi-build version + manifest sha256
└── target/                # cargo's default target dir
    └── release/
        └── <agent_name>   # the runnable binary
```

The runnable artifact is `target/release/<agent_name>` (or
`target/<target_triple>/release/<agent_name>` if `--target`
was used). The operator copies this single binary to the
deployment host; nothing else from the output directory is
required at runtime.

**`pi-build.lock`** records the producing pi-build version +
the manifest's sha256 hash:

```toml
# Generated by pi-build. Do NOT edit.
pi_build_version = "0.1.0"
manifest_sha256  = "abc123...def"
generated_at_unix = 1714752000      # informational only; not used for verify
```

Reserved for Commit C v2's `pi-build verify <binary>` verb
(re-runs codegen against the recorded `manifest_sha256` and
diffs against the binary's embedded source). v1 just writes
the file; nothing reads it yet.

`pi_sdk_version_pin` is intentionally NOT a separate field —
the caret-pin in the generated `Cargo.toml` is a deterministic
function of `(manifest_sha256, pi_build_version)` per B.4, so
Commit C v2's `verify` reconstructs it rather than trusting
the lockfile. Two-source truth is a stale-data magnet.

##### C.3.1 — `--out` semantics

| State of `--out` directory at codegen time | Behavior |
|---|---|
| Doesn't exist | pi-build creates it (incl. parents); writes the artifact tree. |
| Exists, empty | Writes the artifact tree directly. |
| Exists, non-empty, no `--force` | pi-build refuses with exit 73 (`EX_CANTCREAT`) and stderr `<out>: directory not empty; pass --force to overwrite`. |
| Exists, non-empty, `--force` | **Wipe-then-write**: pi-build `std::fs::remove_dir_all(<out>)` followed by re-creation + write. Atomic from the operator's perspective: either the new tree replaces the old completely, or the old is preserved (the `remove_dir_all` failure path leaves the old tree intact). |

The wipe-then-write semantics matter because cargo's
`target/` directory accumulates artifacts. An "overwrite
in place" model would leave stale `target/` entries from a
prior generation, defeating the determinism story (a
follow-up `cargo build` would link against stale objects).

##### C.4 — Cross-compile matrix (informational)

pi-build has zero opinion on which targets the operator builds
for; it forwards `--target` verbatim. Targets pi-rs's CI
exercises (so they're known to work end-to-end with pi-sdk's
transitive depgraph):

| Target | Notes |
|---|---|
| `x86_64-unknown-linux-musl` | static-linked Linux; the canonical "drop in a container" choice. |
| `x86_64-unknown-linux-gnu` | dynamic-linked Linux; matches the host pi-rs builds on. |
| `aarch64-unknown-linux-musl` | ARM Linux (e.g., Graviton). |
| `aarch64-apple-darwin` | macOS arm64 (Apple Silicon). |
| `x86_64-apple-darwin` | macOS x86_64. |
| `x86_64-pc-windows-msvc` | Windows; CI smoke only — no halo support yet. |

Operators targeting other tier-1/tier-2 Rust targets should
work but pi-rs CI doesn't exercise them; file an issue if
something breaks.

##### C.5 — Profile choice rationale

Default `--release` with the B.4 Cargo.toml's `lto = "thin"`
+ `strip = true` + `codegen-units = 1`:

- `lto = "thin"` — LLVM thin LTO: ~10-30% smaller binary, ~5%
  faster runtime, ~50% longer link step than no LTO. Choosing
  `"thin"` over `"fat"` keeps incremental rebuilds under
  ~3 minutes on a typical laptop. The cold first build (with
  the full pi-sdk + tokio + serde transitive graph) is closer
  to 4-6 minutes; cargo's incremental cache amortises
  subsequent builds.
- `strip = true` — drops debug symbols. Saves 5-30 MB
  depending on dependency graph depth. Operators wanting
  symbols for crash analysis use `--debug` or a custom
  `cargo build`.
- `codegen-units = 1` — single codegen unit. Slows
  compilation but produces tighter inlining and smaller
  binaries. Trades 30-60 seconds of build time for a few MB.

These are all generic Rust release-tuning choices; nothing
pi-rs-specific. An operator who wants a different profile
edits the generated `Cargo.toml` (it's their tree after the
codegen step).

##### C.6 — Out of scope (Commit C v1 — deferred to v2 or future RFDs)

Deferred to **v2** (each separately motivated when an operator
asks):

- **Bit-identical reproducibility.** `SOURCE_DATE_EPOCH`
  plumbing + documented invariants across cargo MINORs.
  Requires both pi-build determinism (Commit B B.13 #5
  already asserts this for codegen) AND cargo determinism
  (which depends on the cargo version and is non-trivial).
- **`pi-build verify <binary>`.** Reads `pi-build.lock`,
  re-runs codegen, diffs against the binary's embedded
  source. Requires (a) pi-build to embed source as a
  symbol in the binary OR (b) the operator to retain the
  generated tree. Spec'd as a follow-up.
- **Schema migration tooling (`pi-build migrate`).** Premature
  for a schema with exactly one version.

Out of scope (whole 0028 series, future RFDs):

- **Signing** (Sigstore / cosign). Operator's tooling.
- **Container images** — operator wraps the binary in their
  own Dockerfile.
- **Package-manager distribution** (apt, brew, AUR).
  Operator's choice.
- **Toolchain bundling.** pi-build does NOT ship a hidden
  rustc; the operator's `cargo` + `rustup` are the build
  authority.

##### C.7 — Test plan

- **`--build` smoke:** `pi-build dice-oracle.toml --build`
  on the host triple produces a runnable binary; assert
  `target/release/dice-oracle` exists and is +x.
- **`--target` smoke:** `pi-build dice-oracle.toml --target
  x86_64-unknown-linux-musl --build` succeeds. CI installs
  the target via `rustup target add x86_64-unknown-linux-musl`
  in the workflow setup step.
- **`--debug` smoke:** asserts the produced binary exists at
  `target/debug/dice-oracle` (not `target/release/`).
- **Cargo failure → exit 75:** invalid manifest that passes
  pi-build validation but fails cargo (e.g., contrived case
  where pi-sdk pin doesn't resolve) → pi-build exits 75
  (`EX_TEMPFAIL`).
- **`pi-build.lock` shape:** snapshot test against a fixture
  asserts the lock file has `pi_build_version` + `manifest_sha256`
  + `generated_at_unix` keys; semver of pi_build_version
  parses; sha256 is 64 hex chars.
- **No extraneous flags:** spawn `cargo` via a wrapping mock
  binary that records the argv it was called with; assert
  pi-build invokes it with EXACTLY the flags the operator
  asked for plus `--manifest-path <out>/Cargo.toml`. No
  injected `RUSTFLAGS`, no `-C` overrides.
- **`--target` not installed:** `pi-build agent.toml --target
  nonexistent-triple-unknown --build` exits 75 (`EX_TEMPFAIL`);
  stderr surfaces cargo's `error[E0463]` / `could not find
  target` line verbatim. Pi-build does NOT pre-flight via
  `rustup target list --installed` (would break the
  "no toolchain management" promise of C.2).
- **cargo not on PATH:** invoke pi-build with
  `PATH=/var/empty --build`; tokio's `Command::spawn` returns
  `io::ErrorKind::NotFound` synchronously. Pi-build maps to
  exit 75 with stderr `pi-build: cargo not found on PATH`.
  Spawn-time error is NOT propagated as a panic or generic
  exit 1.
- **`--out` semantics matrix** (per C.3.1): one test per row
  of the table, asserting the documented exit code +
  filesystem outcome for each of the four states.

Commit C's v1 deliverable: build-related flag implementations
in `pi-build` (cargo subprocess + flag forwarding) + the
`pi-build.lock` writer + the test plan above. ~150-200 LoC
(spawn 60 + lock writer 40 + tests 80).

#### Commit D — Halo integration

##### D.1 — Background

Halo (RFD 0025) is pi-rs's autonomous-loop supervisor. Today
each halo cycle invokes `pi --orchestrate` as a subprocess
(verified RFD 0025 §Composition with pi-orchestrate, lines
247-258); halo provides the outer loop, pi-orchestrate runs
one campaign of agent decisions per cycle.

Commit D adds a **second** subprocess shape halo knows how to
spawn — a compiled-agent binary — using the same subprocess
machinery, NOT a new "cycle-kind plug-in" surface. (rfd-critic
v0.1 finding: halo today has no cycle-kind dispatch trait;
inventing one would balloon the LoC estimate and re-architect
halo.) Commit D is a *generalisation* of halo's existing spawn
path: the hardcoded `pi --orchestrate` invocation becomes
"any binary in the operator's halo.toml."

The killer use case: operator writes a `fix-flaky-tests.toml`
manifest (Commit A), `pi-build`s it once (Commits B+C), commits
the binary to a `bin/` dir, points halo at it. Halo runs it
forever, attributes spend to its daily budget, alerts on
non-zero exits, throttles on budget breach.

##### D.2 — `halo.toml` schema additions

Today's `halo.toml` (RFD 0025 §Config: `<repo>/.pi/halo.toml`,
line 530 onward) has a single implicit cycle shape: orchestrate.
Commit D adds the `[[cycle]]` array-of-tables for the operator
to declare one or more compiled-agent cycles:

```toml
# .pi/halo.toml — supervisor config (Commit D additions only).

# Existing fields (RFD 0025) — unchanged: clone path, daily
# spend budget, target_branch, etc.

# NEW: array of compiled-agent cycle declarations. If empty or
# absent, halo's behavior is unchanged from today (pre-Commit-D).
# If non-empty, halo runs the listed cycles in declaration
# order, one per supervisor tick, then loops.
[[cycle]]
name    = "fix-flaky-tests"        # display name for cycle log + alerts.
binary  = "./bin/fix-flaky-tests"  # path resolution rules:
                                   #   - starts with "/" → absolute path.
                                   #   - contains "/"    → relative to .pi/halo.toml's parent dir.
                                   #   - no "/"          → resolved via $PATH.
args    = ["--jsonl"]              # appended after the binary; v1 ALWAYS
                                   # forces `--jsonl` for spend attribution
                                   # to work (D.5). Operator's `args` are
                                   # appended; if they ALSO list `--jsonl`
                                   # the dup is harmless (Commit B's
                                   # arg parser is `any(|a| a == "--jsonl")`).
prompt  = "Audit yesterday's flaky CI failures and propose fixes."
                                   # Piped to the binary's stdin (Commit B's
                                   # read_prompt_from_args_or_stdin honours
                                   # stdin when not a tty).
on_exit = { 0 = "continue", 1 = "alert", 2 = "alert",
            3 = "throttle", 64 = "alert", 65 = "alert",
            73 = "alert", 75 = "throttle" }
                                   # Required keys: 0. Other exit codes
                                   # default to "alert" if unspecified.
                                   # Values: "continue" | "alert" | "throttle"
                                   # (typed `enum ExitPolicy` in code; serde
                                   # rename_all = "snake_case"; future variants
                                   # added without a string-parsing migration).
timeout_secs = 1800                # Wall-clock cap. Halo SIGTERMs the child
                                   # process group at the cap; exit
                                   # synthesised as code 124 mapped per the
                                   # table (default "alert"). Default
                                   # `timeout_secs = 3600` (1 hour) — matches
                                   # halo's safety-by-default ethos. Set
                                   # `timeout_secs = 0` for explicit no-cap
                                   # (not recommended).

throttle_streak_max     = 5        # Pause halo entirely after N consecutive
                                   # throttle outcomes. v1 default 5.
throttle_base_delay_secs = 60      # Initial backoff delay; halo waits
                                   # 2^streak * base_delay before next cycle.
throttle_cap_secs       = 3600     # Maximum backoff (1 hour).

# OPTIONAL nested table for additional env vars to set on the
# child beyond what halo inherits from its own process env (D.3).
# Halo inherits its own env by default — so ANTHROPIC_API_KEY etc.
# propagate without any per-cycle config. `env_extra` is for
# cycle-specific extras. TOML-table shape so each var reads like
# any other TOML key (matches GitHub Actions, docker-compose,
# systemd `Environment=` — flat KEY=VALUE shape was rejected
# pre-publish for being unidiomatic).
[cycle.env_extra]
CYCLE_NAME    = "fix-flaky-tests"
GIT_PAGER     = "cat"
```

Halo's existing `[[cycle]] kind = "orchestrate"` (implicit
today) becomes one of two shapes; orchestrate cycles continue
to work without any operator-side change. The new shape is
distinguished by the presence of `binary`.

##### D.3 — Subprocess plumbing

Halo's existing orchestrate-spawn code at
`crates/pi-coding-agent/src/halo/cycle.rs:655-687`
(`step_orchestrate`) is the model. Today it spawns
`Command::new(current_exe()).args(["--orchestrate", ...])`,
calls `cmd.process_group(0)` (line 673) so the child gets its
own process group, then stores the child's PID in
`CycleCtx.orchestrate_pid_shared` (`Arc<AtomicI32>`, line 49)
so a halo SIGINT handler can `killpg(child_pgid, SIGINT)` to
take down the whole process tree.

Commit D refactors this into a generic
`spawn_cycle_subprocess(cmd: &CycleSubprocessCommand) ->
CycleSubprocessOutcome` function shared by both shapes
(orchestrate and compiled-agent):

```rust
// crates/pi-coding-agent/src/halo/subprocess.rs (NEW — placed
// at the halo module root rather than under cycle/ because the
// existing crates/pi-coding-agent/src/halo/cycle.rs already
// defines a sibling `pub enum CycleOutcome { Done, Aborted }`
// at line 25; nesting under cycle/ would require a
// cycle.rs → cycle/mod.rs rename of the entire 1.4 KLoC file.)

pub struct CycleSubprocessCommand<'a> {
    pub name:    &'a str,
    pub binary:  &'a Path,
    pub args:    &'a [String],
    pub prompt:  &'a str,             // piped to stdin
    pub cwd:     &'a Path,            // halo-owned clone (RFD 0025 §259)
    pub env_extra: &'a BTreeMap<String, String>, // ADDITIONAL vars beyond
                                                 // halo's inherited env (D.3 below).
                                                 // BTreeMap for deterministic
                                                 // iteration order in cycle log.
    pub timeout: Option<Duration>,
    pub pid_shared: Arc<AtomicI32>,   // SIGINT propagation; same
                                      // contract as the existing
                                      // CycleCtx.orchestrate_pid_shared.
}

pub struct CycleSubprocessOutcome {
    pub exit_code:    i32,
    pub events:       Vec<AgentEvent>, // parsed from stdout JSONL
    pub stderr_tail:  String,          // last 16 KiB of stderr
    pub wall_time:    Duration,
    pub spend_usd:    f64,             // sum of Usage events × pricing (D.5)
}
```

Halo invokes via `tokio::process::Command::new(cmd.binary)`,
attaches `Stdio::piped()` for both stdin and stdout (write the
prompt + read JSONL), `Stdio::piped()` for stderr (capture the
tail for the cycle log), and **MUST call `cmd.process_group(0)`
before spawn + store the child PID into `pid_shared`** —
preserving the SIGINT-via-killpg contract halo's existing
signal handler depends on. Failing to do this would make
compiled-agent cycles un-killable from `pi --halo-stop`,
silently regressing halo's signal handling.

**Env passthrough:** halo inherits its own process env into the
child by default — `tokio::process::Command::new` does this
unless `.env_clear()` is called (existing `step_orchestrate`
at `cycle.rs:664-671` does NOT call `env_clear`; halo's env IS
the secrets surface). The compiled agent reads
`secrets.required` from this inherited env via
`AuthStorage::from_env_explicit`. The `env_extra` field is for
ADDITIONAL vars (e.g., cycle-name tags) the operator wants to
inject; it is NOT the secrets channel.

Operator workflow: set `ANTHROPIC_API_KEY` (etc.) in halo's
own environment (via systemd unit, `.envrc`, etc.) — every
spawned cycle inherits it.

##### D.4 — JSONL stdout parser

Reads stdout line-by-line; each line is `serde_json::from_str::<AgentEvent>`.
Per §Cross-cutting #4 + B.13 invariant 2, the wire format is
guaranteed-stable AgentEvent JSON.

```rust
fn parse_event_line(line: &str) -> Option<AgentEvent> {
    if line.trim().is_empty() { return None; }
    match serde_json::from_str::<AgentEvent>(line) {
        Ok(evt) => Some(evt),
        Err(e) => {
            // Bad JSONL line → log to halo cycle log, KEEP READING.
            // A single malformed line MUST NOT abort the cycle.
            tracing::warn!(line, error = ?e, "compiled-agent JSONL parse failed");
            None
        }
    }
}
```

The "skip bad line, keep reading" stance handles the case where
a future Commit B revision adds a new `AgentEventKind` variant
that this halo doesn't know — `serde_json` deserialise fails,
halo logs + drops, cycle continues. Cross-version parser
fragility is the only path to a "halo throws on a benign
forward-compat" failure mode.

##### D.5 — Spend attribution

Halo today maintains a daily-budget ledger at
`~/.pi/halo/<repo>/usage.jsonl` (per RFD 0025 §549-553). Each
orchestrate cycle's spend is wall-clock-bounded estimated.

Compiled-agent cycles get **precise** spend attribution from the
`AgentEventKind::Usage { usage }` events emitted by the agent
binary on stdout (Commit B's pump emits Usage on every turn).
Halo extracts `model_id` from the first `SessionStarted` event
(`pi-agent-core/src/event.rs:9-14` — the variant carries
`{ id, cwd, model: String, provider }` and Commit B's
`create_agent_session` emits it before any Usage event):

```rust
fn cycle_spend(events: &[AgentEvent], pricing: &CostRegistry)
    -> Result<f64, CycleSpendError>
{
    use pi_sdk::AgentEventKind;
    use pi_sdk::cost::estimate_cost_usd;
    let model_id = events.iter()
        .find_map(|e| match &e.kind {
            AgentEventKind::SessionStarted { model, .. } => Some(model.as_str()),
            _ => None,
        })
        .ok_or(CycleSpendError::NoSessionStarted)?;
    let total: f64 = events.iter()
        .filter_map(|e| match &e.kind {
            AgentEventKind::Usage { usage } => Some(usage),
            _ => None,
        })
        .map(|usage| estimate_cost_usd(usage, model_id, pricing))
        .sum();
    Ok(total)
}
```

`pi_sdk::cost::estimate_cost_usd` (RFD 0027 §1 lib.rs surface;
implementation in Track 1 Commit E — pi-sdk::cost module) is
the canonical price-table lookup. The
`SessionStarted`-first ordering is a Commit B invariant (the
event pump can't emit `Usage` before the session opens — pi-sdk
emits `SessionStarted` synchronously during `create_agent_session`).
Receiving a `Usage` before `SessionStarted` IS a hard cycle
abort (`CycleSpendError::NoSessionStarted` → halo treats the
cycle as failed, alert policy applies).

The summed `spend_usd` lands in `usage.jsonl` as a
**precise** ledger row (the "best-effort estimated" caveat
that orchestrate rows carry per RFD 0025 §552-554 does NOT
apply; compiled-agent rows are computed from the agent's
own Usage events).

##### D.6 — Exit-code policy mapping

The `on_exit` table in `halo.toml` `[[cycle]]` declares one of
three policies per exit code:

| Policy | Halo behavior |
|---|---|
| `"continue"` | Halo logs the cycle outcome, advances to the next cycle. The standard happy-path. |
| `"alert"` | Halo logs + emits an alert (per RFD 0025's existing alert plumbing — `~/.pi/halo/<repo>/alerts.jsonl`), continues to the next cycle. Operator review expected. |
| `"throttle"` | Halo logs + delays the next cycle by `min(2^streak * throttle_base_delay_secs, throttle_cap_secs)` per the D.2 schema fields. After `throttle_streak_max` consecutive throttles, halo pauses entirely. Used for "this is degraded but not broken" — e.g., budget exhaustion or transient build failure. |

Required: a row for exit `0`. Unspecified codes default to
`"alert"` — the safe-by-default choice. If the operator wants
"unknown exit codes are fine, just log and continue," they
explicitly write `"*" = "continue"` (catch-all wildcard,
optional).

**In-code shape:** the `on_exit` table deserializes via a typed
`#[serde(rename_all = "snake_case")] pub enum ExitPolicy {
Continue, Alert, Throttle }`. Adding new variants in v2 (e.g.,
`Pause`, `Restart`, `Backoff(Duration)`) is a serde-additive
change with NO string-parsing migration — the v1 manifest with
the three current variants continues to deserialize cleanly.

##### D.7 — Out-of-scope (Commit D v1)

- **Cycle-kind plug-in dispatch trait.** Halo currently has no
  dynamic cycle-kind registry; Commit D piggybacks on the
  static "if `[[cycle]]` has `binary`, spawn that path"
  conditional. A real plug-in trait (so third-party crates
  could register custom cycle kinds) is a halo refactor RFD,
  not Commit D.
- **Multi-cycle parallelism.** v1 runs cycles serially in
  declaration order. Parallel cycles need an orchestration-
  level capability halo doesn't have (cf. RFD 0021's
  `[orchestrate].parallel` deferral).
- **In-process compiled agents.** Halo always spawns a
  subprocess; loading the agent's `main` as a library and
  invoking it in-process would be faster but requires
  pi-build to emit a `lib.rs` shape — not in scope for
  Commit B v1 either.
- **Forward-compat parser auto-upgrade.** If a compiled
  agent emits an `AgentEventKind` variant this halo doesn't
  know, halo skip-and-logs (D.4). It does NOT attempt to
  hot-update its own pi-sdk version to learn the new variant.
- **Live cycle cancellation by the operator.** v1 honours
  `timeout_secs` (halo SIGTERMs the agent) but does NOT
  expose `pi --halo-cancel <cycle-name>` mid-cycle. Defer to
  a halo follow-up RFD.

##### D.8 — Test plan

- **End-to-end with MockProvider:** halo.toml with one
  compiled-agent `[[cycle]]` pointing at a binary built from
  the dice-oracle.toml fixture (Commit A) compiled with
  `--features mocks` (Commit B's manifest can opt into
  features via Cargo.toml — but for v1, this just means
  the test harness builds the agent under `--features mocks`
  externally, NOT that halo.toml has a `features` field).
  Halo runs 3 cycles; assert cycle log has 3 rows, each with
  an `events: [...]` array containing exactly one `Usage`,
  one `TurnComplete`.
- **Spend attribution precision:** MockProvider returns a
  fixed `Usage { input_tokens: 1000, output_tokens: 500 }`;
  CostRegistry has a known $1/MTok input + $5/MTok output
  rate; assert `cycle_spend = 1000/1e6 * 1 + 500/1e6 * 5 =
  $0.0035` (within float epsilon).
- **on_exit policy plumbing:** binary that exits 1 → halo
  emits an alert row in `alerts.jsonl`. Binary that exits 3
  → halo delays the next cycle by ≥ `base_delay` seconds.
  Binary that exits 0 → no alert, no delay.
- **Bad JSONL line skip:** harness binary that emits one
  valid JSONL line, one literal "garbage", one more valid
  JSONL line. Halo MUST log a warning, parse 2 events, NOT
  abort the cycle.
- **`timeout_secs` enforcement:** harness binary that sleeps
  forever on stdin read. `timeout_secs = 5` → halo SIGTERMs
  after ~5s, synthesised exit code 124, mapped to "alert"
  (default for unspecified codes).
- **`binary` path resolution — three cases:**
  - `binary = "/usr/local/bin/agent"` (absolute) — used verbatim.
  - `binary = "./bin/agent"` (relative, contains `/`) —
    resolved relative to the halo.toml's parent dir
    (NOT halo's cwd, NOT `$PATH`).
  - `binary = "agent"` (no `/`) — resolved via `$PATH`.
  Test all three.
- **stderr tail capture:** harness emits 100 KiB to stderr;
  halo retains the last 16 KiB only in `CycleSubprocessOutcome.stderr_tail`.

##### D.9 — Deliverable

- `crates/pi-coding-agent/src/halo/subprocess.rs` — NEW;
  the `spawn_cycle_subprocess` function +
  `CycleSubprocessCommand` / `CycleSubprocessOutcome` types
  (named to avoid collision with the existing `pub enum
  CycleOutcome { Done, Aborted }` at
  `halo/cycle.rs:25`). Includes the `process_group(0)` +
  `pid_shared` SIGINT propagation contract. ~280 LoC.
- `crates/pi-coding-agent/src/halo/jsonl.rs` — NEW;
  AgentEvent JSONL parser + `cycle_spend` (D.5). ~140 LoC.
- `crates/pi-coding-agent/src/halo/config.rs` — extend
  the existing config with `[[cycle]]` schema (D.2): name,
  binary, args, prompt, on_exit (typed `ExitPolicy` enum),
  timeout_secs, env_extra, throttle_*. ~120 LoC.
- `crates/pi-coding-agent/src/halo/cycle.rs` — refactor
  `step_orchestrate` to use the new `spawn_cycle_subprocess`
  primitive (collapsing duplicate spawn plumbing); add the
  parallel `step_compiled_agent` path. ~150 LoC delta. Plus
  a touch-up to `CycleCtx` to thread through the new
  `CycleSubprocessOutcome` shape.
- `crates/pi-coding-agent/src/halo/run.rs` — extend the
  cycle-driver loop to dispatch by cycle shape (the new
  `[[cycle]] binary = ...` rows take the new path). ~80 LoC
  delta.
- Tests: `crates/pi-coding-agent/tests/halo_compiled_agent.rs`
  (D.8 fixtures + assertions, including the 3 binary-path
  cases). ~280 LoC.

Total: **~1050 LoC** (v0.4 sketch said ~600; v0.10 said
~800; v0.11 bumps to ~1050 once the type rename, the
process_group + shared-atomic plumbing duplication, the
env_extra surface, and the cycle.rs refactor are all
priced in per rfd-critic v0.10 finding).

Commit D explicitly does NOT add a new halo cycle-kind plug-in
trait — that's a halo refactor + would need its own RFD.

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

- **v0.12 (2026-05-03):** Commit D v0.11 critic returned
  `NEEDS_REVISION` with 1 critical (citation regression) +
  2 small fixes. v0.12 closes:
  - **Citation regression:** v0.11 swapped "RFD 0027 Commit E"
    for "RFD 0027 §4 Cost & budget helpers" — but §4 is "The
    `RuntimeConfig` field-growth problem", not the cost
    module. Cost helpers are in RFD 0027 §1 lib.rs surface
    + Track 1 Commit E. v0.12 cites both correctly:
    "RFD 0027 §1 lib.rs surface; implementation in Track 1
    Commit E — pi-sdk::cost module."
  - **Orphan rename miss:** D.8 line 1753 still said
    `CycleOutcome.stderr_tail` after the v0.11 type rename;
    fixed to `CycleSubprocessOutcome.stderr_tail`.
  - **`env_extra` TOML form:** v0.11 used flat string-list
    `["FOO=bar"]`; critic flagged as unidiomatic vs.
    GitHub Actions / docker-compose / systemd `Environment=`
    conventions which are all `KEY=VALUE` outliers.
    v0.12 switches to a TOML-table form `[cycle.env_extra]
    KEY = "value"` matching every other TOML key the operator
    writes. Rust type changes from `&[(String, String)]` to
    `&BTreeMap<String, String>` (BTreeMap for deterministic
    cycle-log iteration order).

- **v0.11 (2026-05-03):** Commit D v0.10 critic returned
  `NEEDS_REVISION` with 3 critical + 5 underspec + 2 citation
  errors. v0.11 closes:
  - **Critical: D.3 wrong file cited.** Halo's actual spawn
    site is `crates/pi-coding-agent/src/halo/cycle.rs:655-687`
    (`step_orchestrate`), not `pi-orchestrate/src/runner.rs`.
    Updated D.3 to cite the right file + describe the
    existing `process_group(0)` + `pid_shared` SIGINT
    propagation contract; flagged as a HARD requirement
    (failing to preserve it would make compiled-agent cycles
    un-killable from `pi --halo-stop`).
  - **Critical: D.3/D.9 type-name collision.** Existing
    `pub enum CycleOutcome { Done, Aborted }` at
    `halo/cycle.rs:25` collides with the proposed new
    struct. Renamed to `CycleSubprocessCommand` /
    `CycleSubprocessOutcome`; new module path
    `halo/subprocess.rs` (root-level, not under `cycle/`,
    to avoid the cycle.rs → cycle/mod.rs refactor of a
    1.4 KLoC file).
  - **Critical: D.3 env passthrough hand-waved.** The
    compiled agent reads `secrets.required` from its OWN
    process env via `AuthStorage::from_env_explicit`. Halo
    MUST pass that env. v0.11 spec'd: halo inherits its own
    env into the child by default (matching today's
    `step_orchestrate` at `cycle.rs:664-671`); halo's env is
    THE secrets surface; new D.2 `env_extra` field is for
    additional vars (cycle-name tags etc.), NOT secrets.
  - **Underspec: D.5 model_id sourcing.** Replaced the
    conditional `model_hint` hand-wave with the verified
    `SessionStarted` extraction (`pi-agent-core/src/event.rs:9-14`
    carries `model: String`). `cycle_spend` signature drops
    the `model_id` parameter; receiving `Usage` before
    `SessionStarted` is now `CycleSpendError::NoSessionStarted`.
  - **Underspec: D.6 magic numbers.** Promoted `5` /
    `1 hour` / `base_delay` to D.2 schema fields with v1
    defaults (`throttle_streak_max = 5`,
    `throttle_base_delay_secs = 60`,
    `throttle_cap_secs = 3600`).
  - **Underspec: D.6 `on_exit` shape.** Spec'd that the
    in-code shape is a typed `enum ExitPolicy { Continue,
    Alert, Throttle }` with `serde rename_all = "snake_case"`
    so v2 variants land additively without a string-parsing
    migration.
  - **Underspec: D.2 `binary` path resolution.** Added the
    third case (absolute path "starts with /"); D.8 test
    plan now covers all three.
  - **Underspec: D.2 `timeout_secs` default.** Changed from
    "no cap" to `3600` (1 hour) per halo's safety-by-default
    ethos; `0` is the explicit no-cap.
  - **Citation: "RFD 0027 Commit E"** for `pi_sdk::cost` —
    wrong (Commit E is the crates.io publish). Changed to
    "RFD 0027 §4 Cost & budget helpers."
  - **D.9 LoC bump:** ~800 → ~1050 once the type rename,
    process_group plumbing duplication, env_extra surface,
    and cycle.rs refactor are priced in.

- **v0.10 (2026-05-03):** Commit C reached READY in v0.9.
  Applied v0.9 critic's 5 sub-blocker deltas inline as polish
  (C.2 "no flags injected" carve-out for `--manifest-path`,
  C.3 `pi_sdk_version_pin` derivability statement, new
  C.3.1 `--out` semantics table including `--force` =
  wipe-then-write rationale, C.5 build-time soften for cold
  first build, C.7 added 3 negative-path tests).

  Now expanding Commit D from sketch to full spec
  (sub-sections D.1-D.9). Plus housekeeping fix for a residual
  `Commit Composition` artifact from the v0.3 § global
  replace.

  Commit D sub-sections added:
  - D.1 — background framing (halo today spawns
    `pi --orchestrate`; Commit D generalises to "any binary
    in halo.toml [[cycle]]"; killer use case is
    operator-authored fix-flaky-tests-style agents running
    forever under halo's outer loop).
  - D.2 — `halo.toml` `[[cycle]]` schema additions
    (`name`, `binary`, `args`, `prompt`, `on_exit` table,
    `timeout_secs`); halo ALWAYS forces `--jsonl` for spend
    attribution (D.5).
  - D.3 — subprocess plumbing (`spawn_cycle_subprocess` shared
    by orchestrate + compiled-agent shapes; new types
    `CycleCommand` / `CycleOutcome`).
  - D.4 — JSONL stdout parser (skip-and-log on bad lines or
    forward-compat unknown variants; cycle keeps reading).
  - D.5 — spend attribution (precise, computed from agent's
    own `Usage` events × `pi_sdk::cost::estimate_cost_usd`;
    NO "best-effort estimated" caveat unlike orchestrate
    rows).
  - D.6 — `on_exit` policy mapping table (continue / alert /
    throttle); unspecified codes default to "alert"; required
    row for exit 0; optional `"*"` catch-all.
  - D.7 — out-of-scope (cycle-kind plug-in trait, parallel
    cycles, in-process compiled agents, forward-compat
    parser auto-upgrade, live cancellation).
  - D.8 — test plan (e2e MockProvider, spend precision,
    on_exit plumbing, bad-JSONL-skip, `timeout_secs`,
    `binary` path resolution rules, stderr tail).
  - D.9 — deliverable: ~800 LoC across 4 files in
    crates/pi-coding-agent/src/halo/, plus integration
    tests (was ~600 in v0.4 sketch; expansion +
    JSONL parser + spend attribution + test harness add ~200).

- **v0.9 (2026-05-03):** Commit B reached READY in v0.8. Now
  expanding Commit C from sketch (~30 lines) to full spec
  (sub-sections C.1-C.7). Plus inline application of v0.8
  critic's cosmetic note: B.15 now has a maintainer-note
  forward-pointer about extending B.13 invariant 6 clause (b)
  if `Settings::builder()` gains new string-taking setters.

  Commit C sub-sections added:
  - C.1 — scope statement (minimum surface for non-build-host
    distribution; v2 deferrals listed).
  - C.2 — flag implementation table (`--build`, `--target`,
    `--release`, `--debug`); spawn cargo via tokio Command
    with **no hidden flag injection, no RUSTFLAGS, no -C
    overrides** beyond the manifest's profile.
  - C.3 — operator artifact tree, including the new
    `pi-build.lock` file (records pi-build version + manifest
    sha256; reserved for Commit C v2's `verify` verb; v1 just
    writes it).
  - C.4 — informational cross-compile matrix listing the 6
    targets pi-rs CI exercises (musl/gnu Linux, macOS arm64+x64,
    Windows MSVC).
  - C.5 — release-profile rationale (`lto = "thin"` +
    `strip = true` + `codegen-units = 1`); generic Rust
    tuning, nothing pi-rs-specific.
  - C.6 — explicit out-of-scope items (reproducibility,
    `verify`, `migrate` to v2; signing, containers, package
    managers, toolchain bundling to future RFDs).
  - C.7 — test plan: build smoke per `--target`, `--debug`
    smoke, cargo-failure → exit 75 mapping, `pi-build.lock`
    shape snapshot, NO-injected-flags assertion via cargo
    wrapping mock.

- **v0.8 (2026-05-03):** Commit B v0.7 critic returned
  `NEEDS_REVISION` — closed v0.6's 2 critical + 6 underspec
  cleanly but introduced 2 new defects in the new content + 1
  prose nit. v0.8 closes:
  - **Critical: `from_env_explicit([])` won't compile.** v0.7's
    invariant 8 specified the literal `[]` substring; the
    function signature is generic `<I, P, E>` and an empty
    array can't infer `P`/`E` (rustc emits E0282/E0283).
    v0.8 changes the canonical empty-allowlist rendering to
    `from_env_explicit(std::iter::empty::<(&str, &str)>())`.
    Updated invariant 8's snapshot substring to match.
    Added the special-case logic to B.6's lowering rule
    (now has explicit "0 → iter::empty / N → array literal"
    branches). Added the `{{auth_call}}` substitution marker
    to B.3 to make the two-branch shape visible in the
    template.
  - **Critical: B.13 #6 syn rule still false-positives on
    operator-authored `system_prompt` text.** The v0.7
    rewrite from regex to syn AST walk relocated the false
    positive without solving it. Added the OR-clause:
    string literals containing `_API_KEY|_TOKEN` may appear
    EITHER as children of `from_env_explicit(...)` OR as
    literal arguments to `Settings::builder().system_prompt
    /.model/.provider`. Operators commonly document env
    var names in prompts; that's not a leak.
  - **Nit: B.5 prose conflated membership-set ordering with
    output ordering.** `Vec::retain` preserves first-occurrence
    position; the `BTreeSet` is just for membership testing.
    Reworded — `HashSet` would also work for correctness;
    `BTreeSet` is the std-only no-extra-dep default.

- **v0.7 (2026-05-03):** Commit B v1 critic returned
  `NEEDS_REVISION` with 2 critical + 6 underspec'd. v0.7 closes:
  - **Critical: B.4 missing `tokio` `sync` feature.** Generated
    `main.rs` calls `tokio::sync::mpsc::unbounded_channel()`
    which requires the `sync` feature; without it every
    generated agent fails to compile. Replaced
    `["macros", "rt", "io-util", "io-std"]` with
    `["macros", "rt", "sync"]` (dropped `io-util`/`io-std`
    since the helper uses `std::io::stdin()`, not tokio I/O).
  - **Critical: B.12 line citation rotted.** `RuntimeError` is
    at `pi-agent-core/src/runtime.rs:1856-1912`, not
    `:50-115` (those lines contain `GateContext` + tests).
    Variant names are correct on substance.
  - **Underspec'd: B.6 prose.** Split into "lowering rule"
    (paired with `provider.name` regardless of canonical
    mapping) vs "canonical pairing reference table"
    (informational, for operator authoring).
  - **Underspec'd: B.5 dedup primitive.** Pinned to a std-only
    `BTreeSet`-based `Vec::retain` — preserves first-occurrence
    insertion order with deterministic ordering, no
    `itertools` dep.
  - **Underspec'd: B.11 `--` separator handling.** Spec'd that
    v1 drops every `^--` arg unconditionally (no POSIX
    end-of-options); operators with prompts starting with
    `--` use stdin. POSIX `--` reserved for v2.
  - **Underspec'd: B.13 invariant gap.** Added invariant 8
    (no-secret manifest produces no env reads) — catches
    "operator forgot to allowlist anything but agent still
    magically authenticates."
  - **Underspec'd: B.13 #6 regex approach.** Pure regex would
    false-positive on operator-authored `system_prompt` text
    mentioning env-var names. Spec'd `syn::parse_file` AST
    walk: string literals containing `_API_KEY|_TOKEN` MUST
    be children of a `from_env_explicit([...])` call.
  - **Underspec'd: B.2 cross-version determinism.** Added
    explicit note that pi-build N.x → N.(x+1) may produce
    different bytes (comment header carries version);
    operators needing cross-version reproducibility pin
    pi-build or use Commit C v2's `verify` verb.

- **v0.6 (2026-05-03):** Commit B expanded from sketch to full
  spec. Sub-sections B.1-B.15 added covering: pi-build CLI shape
  (B.1), codegen flow + determinism (B.2), byte-exact main.rs
  template with substitution markers (B.3), generated Cargo.toml
  template with caret-pinned pi-sdk + thin-LTO release profile
  (B.4), tool-allowlist lowering (B.5), auth-allowlist lowering
  with the canonical provider→env-var table (B.6),
  Settings::builder lowering (B.7), ProviderName→string lowering
  per A.3 wire-form parity (B.8), ThinkingLevel→ThinkingSetting
  lowering (B.9), u64→usize lowering for max_* fields (B.10),
  read_prompt_from_args_or_stdin helper (B.11),
  map_runtime_error_to_exit helper covering pi-sdk RuntimeError
  variants (B.12), 7 hard codegen invariants (B.13: same-allowlist,
  AgentEvent JSONL stable, stdout discipline, tokio flavour,
  determinism, no-secrets-in-source, stdout/stderr separation),
  test plan (B.14), out of scope (B.15). Removed the leftover
  v0.4 Commit B sketch that had been left below the new content
  by mistake. Plus: applied the v0.5 micro-nit fix
  (`validate(&mut Manifest)` signature in A.4 prose to match the
  dedup-mutation paragraph). Plus: fixed a residual
  `Commit Cross-cutting` token from the v0.3 § global replace
  back to `§Cross-cutting`.

- **v0.5 (2026-05-03):** Commit A expanded from sketch to full
  spec (sub-sections A.1-A.9, +303 lines in `e873917`); rfd-critic
  Commit A pass returned `NEEDS_REVISION` with 3 critical
  findings + 5 underspec'd. v0.5 closes:
  - **Critical: `max_recursion` default wrong (3 vs 8).** Verified
    real H2 default is 8 (`pi-agent-core/src/runtime.rs:447`).
    Fixed in both A.1 comment and A.3 default fn.
  - **Critical: type mismatch (`u32` vs `usize`).** Verified
    pi-sdk fields are `usize` (`runtime.rs:300,306`) and the
    builder setters take `usize` (`:386,394`). Manifest wire
    types changed to `u64` (platform-portable, then lowered via
    `usize::try_from(n)?` in Commit B); added `OutOfRangeForUsize`
    error variant + matching A.4 validation rule.
  - **Critical: missing `PartialEq`/`Eq` derives.** A.8's
    round-trip test required them but A.3 only derived
    `Debug, Clone, Serialize, Deserialize`. Added `PartialEq, Eq`
    to every struct.
  - **Underspec'd: `provider.thinking` validation.** Added a row
    explaining no semantic rule needed (closed enum, serde-layer
    enforcement). Same note added for `provider.name`.
  - **Underspec'd: `tools.allowlist` dedup behavior.** Specified
    silent dedup in `validate()`; `EmptyAllowlist` error fires
    only if dedup empties the list.
  - **Underspec'd: tool name case sensitivity.** Specified
    case-sensitive lowercase-only match (`"Read"` →
    `UnknownTool("Read")`, no normalization).
  - **Underspec'd: `schema_version = 0` reachability.** A.5
    parser routes `0` → `SchemaTooOld` and `> 1` → `SchemaTooNew`;
    `SchemaTooOld` variant is now reachable.
  - **Underspec'd: A.8 fixture coverage.** Added length-boundary
    (±1 byte at description/system_prompt/model boundaries),
    `max_recursion` boundaries (0/1/8/16/17),
    `OutOfRangeForUsize` (cfg-gated 32-bit), empty-file, and
    binary-garbage cases.
  - **A.9 wording fix:** changed `[tools.bash]` rejection error
    text from `Parse(unknown field 'tools.bash')` to
    `Parse(unknown field 'bash')` (serde's actual output —
    field is `bash` under `ToolsConfig`).
  - Added explicit "ProviderName wire-form parity" note in A.3
    confirming Commit B can pass `kebab-case` strings through to
    `Settings.provider` with no remapping.

- **v0.4 (2026-05-03):** rfd-critic v0.3 pass returned
  `NEEDS_REVISION` solely because the v0.3 `_runtime` lifetime
  comment claimed dropping `_runtime` would close the
  provider/event channel — false. `AgentSession` carries its own
  `Arc<RuntimeConfig>` clone (`runtime.rs:649,882`) and the
  provider is built lazily inside `prompt(...)`. v0.4 corrects
  the comment to state the binding is stylistic, not
  load-bearing. No other findings; critic explicitly stated
  "that's the only thing blocking READY."

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
    AgentSession)>`. Template binds via `let (_runtime,
    session) = ...;`. (v0.3 had a false rationale claiming
    `_runtime` must stay alive to keep the channel open;
    rfd-critic v0.3 caught it — `AgentSession` carries its own
    `Arc<RuntimeConfig>` clone at `runtime.rs:649,882`, so
    dropping the runtime is harmless. v0.4 corrects the comment
    to "binding is stylistic, not load-bearing.")
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
    §Cross-cutting #7-#10. Each is a hard contract for Commit B codegen.
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
