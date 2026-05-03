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
- `ToolRegistry::with_unsafe_extras()` — full tool set (read/write/edit/bash + grep/find/ls + web_search); the name itself signals risk (Commit H7).
- `LocalProcessProvider::with_readonly_defaults()` (Commit H7).
- README + 5 examples + doc-tested README (Commit F).

### Added — Track 2 (hardening, pre-1.0 hard gate)
- `catch_unwind` boundary around `Tool::invoke` (Commit H1). Panicking custom tools no longer crash the worker thread; surfaced as `RuntimeError::ToolPanicked` + `ToolResult::is_error = true`.
- `RuntimeError::EmptyTurn` replaces panicking `last_assistant.unwrap()` (Commit H1).
- Stream-event validation + per-session token budget guards (Commit H2). New `RuntimeConfig::{max_session_tokens, max_tool_invocations_per_turn, max_recursion}`. New `RuntimeError::{BudgetExhausted, InvocationCapExceeded, ToolUseFinishWithoutCalls, DepthExceeded}`. Cumulative-Usage providers (Google, Anthropic) are NOT double-counted.
- `ToolGate::approve` now takes `GateContext { session_id, turn_index, parent_session, recursion_depth }` (Commit H3). `GateContext::top_level()` constructor for embedders.
- `ToolRegistry::register` returns `Result<(), DuplicateName>` (Commit H3). New `register_or_replace` for explicit overrides.
- `bash` tool: `cwd` argument canonicalize-jailed against `ctx.cwd`; `timeout_ms` clamped at 600 s; per-tool input cap (64 KiB) (Commit H4).
- `AuthStorage` hardening (Commit H5): `0o600` perms + atomic temp + rename on write; `from_env_explicit(allowlist)` for opt-in env scanning replacing the H5-deprecated (and polish-12-removed) implicit-slurp `from_env()`; `scoped(allowlist)` for per-tenant restriction; `sealed()` for post-init immutability.
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
- `AuthStorage::from_env_explicit_iter` for IntoIterator-shaped allowlists (polish-2, pass-3 #6). **Removed in polish-13** — collapsed into `from_env_explicit` which now accepts the same IntoIterator shape.
- Display smoke tests for the four future-additive `Error` variants (polish, pass-1 #12).
- Extension-collision panic message names BOTH colliding extensions, not just the tool (polish-2, pass-3 #5).
- `MIGRATION.md` scaffold + sandbox network-omission doc on `LocalProcessProvider::with_readonly_defaults` (polish-3, pass-1 #11).
- Link to `MIGRATION.md` / `COMPATIBILITY.md` / `CHANGELOG.md` from the README "See also" section (polish-4).
- `tests/end_to_end_safe_path.rs` integration test: asserts `quick_start` produces a runtime with empty AuthStorage + readonly tool surface + sandbox provider wired (polish-5; renamed in polish-7 per pass-8 NIT #1).
- `RuntimeConfig::with_max_session_tokens / with_max_tool_invocations_per_turn / with_max_recursion` post-build setters (polish-6, mirror of `ConfigBuilder`); doc-comments call out last-write-wins composition vs the builder (pass-9 NIT #2) and clarify they do NOT compose with `quick_start` (pass-9 NON-BLOCKING #1).
- `tests/no_features_smoke.rs` — 5 default-features tests covering the surface that ships when `mocks` is OFF; symbol-existence sweep over ~30 re-exports including the 6 concrete provider types and the dyn-trait surfaces (polish-7; expanded in polish-9 per pass-9 NIT #3). Top-of-file EDITORS POLICY warns against importing mocks-gated symbols (pass-9 NIT #4).
- `DuplicateName` re-exported through pi-sdk (was reachable only via pi-tools).
- Strengthened `rootfs_version_current_matches_inlined_const` test: also asserts `cache::ROOTFS_VERSION == microvm::ROOTFS_VERSION` so a future literal-duplicate regression fails immediately (review-feedback-8 + pass-7 NIT #2).
- `Settings::builder()` + `SettingsBuilder` — fluent builder for the most-set fields (provider/model/thinking/compact_threshold/theme/route/no_tools) plus a `with(impl FnOnce(&mut Settings))` escape hatch for the long tail. Additive prerequisite for the eventual `#[non_exhaustive]` mark on Settings (polish-8, pass-1 #8 partial).
- `Pricing::cost_for(usage) -> f64` — compute USD cost directly without a CostRegistry lookup. Same arithmetic as `estimate_cost_usd`; useful for hot loops where the embedder already has a Pricing in hand (polish-9).
- Re-exports through pi-sdk for `RouteMode`, `ThinkingSetting`, `QueueMode`, `MonitorSettings`, `EvolveSettings` (polish-17, pass-12 NIT #9). `SettingsBuilder::thinking(t)` / `.route(r)` previously took types not reachable through `pi_sdk::*`; embedders now have the full settings-field type set.
- Re-exports through pi-sdk for `AiError`, `InterceptAction`, `OutcomeSource` (polish-19, pass-13 NIT #4-5 + NB2). `AiError` is the wrapped type in `pi_sdk::Error::Provider(_)` and the return type in the `Provider::stream` trait method (example 04 used to reach into `pi_ai::AiError` directly); `InterceptAction` is the return type of `StreamInterceptor::on_text_delta`; `OutcomeSource` is a public field of `SessionEntryKind::Outcome`.
- `scoped_then_scoped_replaces_rather_than_intersects` + `sealed_then_scoped_preserves_seal` regression tests in pi-ai/src/auth.rs (polish-17, pass-12 NIT #13). Lock the documented composition semantics into the test suite.

### Removed
- `pi_coding_agent::sdk` deprecated shim (Commit K). Embedders use `pi-sdk` directly. The shim was added in Commit A as the back-compat bridge during the SDK extraction; its removal closes the SDK-extraction track.
- `AuthStorage::from_env()` (polish-12). Was `#[deprecated]` since H5; the unsafe slurp-all-17-vars shape is gone. Binary callers use `AuthStorage::from_env_explicit(AuthStorage::ENV_KEYS)` (own-machine trust model is auditable in code-review). SDK embedders use `from_env_explicit` with a narrower allowlist.
- `ToolRegistry::with_extras()` (polish-12). Was a name carrying no safety signal — replaced by `with_unsafe_extras()` per RFD §4.5 #12. The post-H7 alias was kept only for migration during 0.1; pre-publish there are no embedders to migrate, so the alias was dropped. Internal pi-rs binary callers (startup.rs, sandbox-worker dispatch) updated to `with_unsafe_extras()`.
- `#[allow(deprecated)]` annotations across pi-coding-agent (startup, cmd, halo) and pi-ai tests — no longer needed once the deprecated symbol is gone.
- `AuthStorage::from_env_explicit_iter` (polish-13). Consolidated into `from_env_explicit` which now accepts any `IntoIterator<Item = (impl Into<String>, impl AsRef<str>)>`. Slice callers migrate via bare-array literal (`[("a","b")]`) or `.iter().copied()` for the static `ENV_KEYS` slice.
- `BuildConfig` + `build_runtime_config` (polish-15). Were the seed of the SDK extraction (originally `pi_coding_agent::sdk::BuildConfig`); when Commit K removed the `pi_coding_agent::sdk` shim they became pure overlap with `RuntimeConfig::builder()`. Per the user's pre-publish "remove migration cruft" direction they were dropped. Embedders use `RuntimeConfig::builder()` directly. `quick_start` survives as the one-liner first-touch convenience.

### Notes
- This is the pre-0.1.0 working tree. Once Commit J publishes 0.1.0
  to crates.io, this `[Unreleased]` block freezes to `[0.1.0] —
  YYYY-MM-DD` and a fresh `[Unreleased]` block opens above it.
- All hardening commits include regression tests; see the per-commit
  message for the specific tests.

[Unreleased]: https://github.com/n3mes1s/playground/compare/main...main
