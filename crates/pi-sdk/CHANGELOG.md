# Changelog

All notable changes to `pi-sdk` are documented here. The format
follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/);
versioning follows [SemVer](https://semver.org/) per RFD 0027 §3.

## [Unreleased]

### Added — Track 1 (façade + ergonomics)
- `pi-sdk` façade crate with workspace registration (Commit A).
- `RuntimeConfig::builder()` with `#[non_exhaustive]` semantics (Commit B).
- Top-level `pi_sdk::Error` with `#[from]` for every underlying crate's error type (Commit C).
- `pi_sdk::mocks::{MockProvider, MockProviderFactory, MockSandboxProvider, MockSandboxCall}` (gated on the `mocks` feature) (Commit D).
- `pi_sdk::cost::{CostRegistry, Pricing, estimate_cost_usd, sum_session_cost_usd}` with bundled price defaults for major models (Commit E).
- `pi_sdk::quick_start(provider, model)` convenience for first-touch demos (safe-by-default: in_memory auth, readonly tools, no shell) (Commit H7).
- `ToolRegistry::with_readonly_extras()` — read/grep/find/ls only, no shell, no fs mutation (Commit H7).
- `ToolRegistry::with_unsafe_extras()` — alias for `with_extras()`, name signals risk (Commit H7).
- `LocalProcessProvider::with_readonly_defaults()` (Commit H7).
- README + 5 examples + doc-tested README (Commit F).

### Added — Track 2 (hardening, pre-1.0 hard gate)
- `catch_unwind` boundary around `Tool::invoke` (Commit H1). Panicking custom tools no longer crash the worker thread; surfaced as `RuntimeError::ToolPanicked` + `ToolResult::is_error = true`.
- `RuntimeError::EmptyTurn` replaces panicking `last_assistant.unwrap()` (Commit H1).
- Stream-event validation + per-session token budget guards (Commit H2). New `RuntimeConfig::{max_session_tokens, max_tool_invocations_per_turn, max_recursion}`. New `RuntimeError::{BudgetExhausted, InvocationCapExceeded, ToolUseFinishWithoutCalls, DepthExceeded}`. Cumulative-Usage providers (Google, Anthropic) are NOT double-counted.
- `ToolGate::approve` now takes `GateContext { session_id, turn_index, parent_session, recursion_depth }` (Commit H3). `GateContext::top_level()` constructor for embedders.
- `ToolRegistry::register` returns `Result<(), DuplicateName>` (Commit H3). New `register_or_replace` for explicit overrides.
- `bash` tool: `cwd` argument canonicalize-jailed against `ctx.cwd`; `timeout_ms` clamped at 600 s; per-tool input cap (64 KiB) (Commit H4).
- `AuthStorage` hardening (Commit H5): `0o600` perms + atomic temp + rename on write; `from_env_explicit(allowlist)` for opt-in env scanning; `scoped(allowlist)` for per-tenant restriction; `sealed()` for post-init immutability; `from_env()` deprecated.
- `WireSerializer` for JSONL session entries (Commit H6): 1 MiB per-field cap, ANSI escape stripping, C1 + bidi-override `\u`-escape.
- `SessionEntryKind::InterceptorInjection` variant distinguishes synthetic-user from real operator input (Commit H6).

### Added — Track 3 (distribution + CI)
- `pi-sdk-canary` test crate exercising the public surface (Commit G). 10 unit tests + 1 integration test.
- `crates/pi-sdk/compatibility.toml` + generated `COMPATIBILITY.md` (Commit G). `scripts/gen-compatibility-matrix.sh` regenerates the markdown (anchored awk regexes per pass-6 #7).
- `.github/workflows/pi-sdk-supply-chain.yml`: cargo-audit, cargo-deny, pi-sdk-canary, examples-build, doc-tests, COMPATIBILITY-up-to-date (Commit I). cargo-semver-checks gate scaffolded; enabled in Commit J's PR after first crates.io publish. `concurrency` block added per pass-6 #5.
- `deny.toml` — bans (no MAJOR-version duplicates), licenses (MIT/Apache-2.0/BSD/ISC/MPL/Zlib allow-list, GPL/AGPL denied per RFD §6), sources (crates.io only) (Commit I).
- `SECURITY.md` — coordinated disclosure via GitHub Security Advisories, RUSTSEC namespace reservation, supported-versions table (Commit I).
- `RELEASING.md` — per-release checklist, publish order (9 crates in dependency order), rate-limited publish loop, post-publish steps, yank/recovery procedures (Commit J-prep).
- Workspace-deps `version = "0.1.0"` on every path-dep; `license/repository/authors.workspace = true` on the 9 publishable crates + `publish = false` on the 5 binary-side crates (Commit J-prep + pass-6 #3).
- `ROOTFS_VERSION` const inlined into `pi-sandbox/src/microvm/types.rs` (was `pi_sandbox_rootfs::ROOTFS_VERSION`); the rootfs scaffolding crate stays `publish = false` so `pi-sandbox` is a publishable leaf (pass-6 #1).
- `ConfigBuilder::cwd_from_env()` helper + `build()` defaults `cwd` to `current_dir()` (polish, pass-1 #9).
- `AuthStorage::from_env_explicit_iter` for IntoIterator-shaped allowlists (polish-2, pass-3 #6).
- Display smoke tests for the four future-additive `Error` variants (polish, pass-1 #12).
- Extension-collision panic message names BOTH colliding extensions, not just the tool (polish-2, pass-3 #5).

### Removed
- `pi_coding_agent::sdk` deprecated shim (Commit K). Embedders use `pi-sdk` directly. The shim was added in Commit A as the back-compat bridge during the SDK extraction; its removal closes the SDK-extraction track.

### Notes
- This is the pre-0.1.0 working tree. Once Commit J publishes 0.1.0
  to crates.io, this `[Unreleased]` block freezes to `[0.1.0] —
  YYYY-MM-DD` and a fresh `[Unreleased]` block opens above it.
- All hardening commits include regression tests; see the per-commit
  message for the specific tests.

[Unreleased]: https://github.com/n3mes1s/playground/compare/main...main
