# RFD 0028 — Compiled agents from TOML manifest (meta + split into A/B/C/D)

- **Status:** Draft (v0.7; meta READY in v0.4, Commit A READY in v0.5, Commit B v1 spec pending critic)
- **Author:** Giuseppe Massaro (drafted with claude-opus-4-7, revised after rfd-critic v0.1, v0.2, v0.3, Commit A v1, Commit A v0.5, Commit B v1 passes)
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
    // Auth: per-manifest env-var allowlist (B.6).
    let auth = match AuthStorage::from_env_explicit([
        {{auth_pairs}}                               // e.g., ("anthropic", "ANTHROPIC_API_KEY"),
    ]) {
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
list across runs and across hosts (BTreeSet's deterministic
ordering means even the temporary `seen` set is stable).

If `tools.allowlist == ["read", "write", "edit", "bash", "grep",
"find", "ls", "web_search"]` (the full set), the codegen still
emits the full `keep_only` call rather than skipping it — the
explicit list is the audit surface, even when redundant.

##### B.6 — Auth allowlist lowering

**Lowering rule:** each `secrets.required[i]` pairs with
`provider.name`'s kebab string. Pseudocode:

```rust
auth_pairs = secrets.required.iter()
    .map(|env| (provider.name.as_kebab(), env.clone()))
    .collect();
```

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
   string literal containing `_API_KEY|_TOKEN` MUST be a child
   of a `from_env_explicit([...])` call expression. (Pure
   regex is insufficient — a `system_prompt` mentioning
   "ANTHROPIC_API_KEY" by name in operator-authored prose
   would false-positive.) Test asserts via syn walk; pi-build
   takes a dev-dep on `syn` for tests only.
7. **Stdout vs stderr separation.** Integration test runs
   the generated binary with `--jsonl` against a MockProvider
   that emits known events; asserts stdout contains only valid
   JSONL and stderr contains zero JSONL-shaped lines.
8. **No-secret manifest produces no env reads.** A manifest
   with `secrets.required = []` codegens
   `AuthStorage::from_env_explicit([])` — the literal empty
   slice. Snapshot test asserts the exact rendered substring
   `from_env_explicit([])` appears in `main.rs` and that NO
   `_API_KEY|_TOKEN` literal appears anywhere outside string
   literals in `system_prompt`. Catches the "operator forgot
   to allowlist anything but the agent still magically
   authenticates" failure mode.

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

Commit B's deliverable: `crates/pi-build/src/{lib.rs (codegen),
codegen.rs (template engine), bin/pi-build.rs (CLI)}` + the
`tests/snapshots/` + `tests/invariant_*.rs` fleet. ~1200 LoC
(codegen 500 + template engine 300 + CLI 200 + tests 200).

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
