# pi-sdk migration guide

Per RFD 0027 §6: every MAJOR release ships a section in this file with
a search-and-replace table for renamed/removed symbols, code-snippet
diffs for non-mechanical migrations, and behavior-change callouts.

This file is a **hard release blocker** — no MAJOR ships without
the corresponding migration entry. PATCH and MINOR releases land
without entries (they're additive by RFD §3 contract).

The current SDK is `0.1.x` — pre-1.0. Per RFD §3 the pre-1.0 contract
allows breaking changes in any 0.x → 0.x+1; embedders pinning
`pi-sdk = "0.1"` should expect surface drift between minors. The
`[Unreleased]` section below collects breaking changes between
the in-flight working tree and the most recent published version
(once 0.1.0 ships).

---

## [Unreleased]

No breaking changes since 0.1.0 (not yet published — the working
tree pre-publish dropped `BuildConfig` + `build_runtime_config` +
`AuthStorage::from_env()` + `ToolRegistry::with_extras()` so these
never appear in any published version). Several additive APIs are
available for embedders to migrate to ahead of the eventual breaking
changes documented in the "0.x → 1.0" section below:

- **`Settings::builder()` (polish-8)** — additive. The breaking
  `#[non_exhaustive]` mark on `Settings` is deferred to 1.0; the
  builder is the migration target. Embedders constructing via
  struct literal today (`Settings { provider: "x".into(),
  ..Settings::default() }`) can switch to `Settings::builder().
  provider("x").build()` now to avoid the migration churn at 1.0.
- **`AuthStorage::from_env_explicit` (polish-13 consolidation)** —
  one method, one signature: `IntoIterator<Item = (impl Into<String>,
  impl AsRef<str>)>`. Bare-array literals work directly
  (`[("a","b")]`); the static `AuthStorage::ENV_KEYS` slice works
  via `.iter().copied()`; `Vec<(String, String)>` works directly.
  The earlier two-method split (slice form + `_iter` variant) was
  collapsed pre-publish.
- **`ConfigBuilder::cwd_from_env()` + `build()`-time default to
  `current_dir()` (polish)** — additive ergonomics. `.cwd(path)`
  call sites still work; if omitted, defaults to current_dir.
- **`RuntimeConfig::with_max_session_tokens / with_max_tool_
  invocations_per_turn / with_max_recursion` (polish-6)** —
  additive. Post-build setters mirror the builder for the
  `RuntimeConfig::builder().build()?.with_max_*(N)` chain
  pattern. NOT for `quick_start` (which returns
  `AgentSessionRuntime`, not a config — embedders bumping caps
  after `quick_start` should rebuild via `RuntimeConfig::builder()`
  directly).
- **`Pricing::cost_for(usage)` (polish-9)** — additive. Compute
  USD cost without a CostRegistry lookup; same arithmetic as
  `estimate_cost_usd`. Useful for hot loops.

---

## 0.x → 1.0 (planned, not yet shipped)

The 1.0 release will collapse 0.x's residual ergonomic warts. The
expected migration shape:

### 1. `Settings { ..Settings::default() }` → `Settings::builder()`

`Settings` becomes `#[non_exhaustive]` at 1.0 per RFD §3 blanket
policy. The struct-literal-with-spread pattern stops compiling
from external crates:

```rust
- let s = Settings {
-     provider: "anthropic".into(),
-     model: "claude-haiku-4-5-20251001".into(),
-     ..Settings::default()
- };
+ let s = Settings::builder()
+     .provider("anthropic")
+     .model("claude-haiku-4-5-20251001")
+     .build();
```

For fields not surfaced as named setters (LSP, monitor, evolve,
task overrides), use the `with` escape hatch:

```rust
let s = Settings::builder()
    .provider("anthropic")
    .with(|s| s.evolve.enabled = false)
    .build();
```

The builder ships in 0.x (polish-8) so embedders can migrate
ahead of the 1.0 freeze.

### 2. Sandbox launcher traits leave `*-unstable` features

In 0.x (and 1.0 + 1.1) the microvm + remote sandbox launcher traits
ship behind:

```toml
pi-sdk = { version = "0.1", features = ["sandbox-microvm-unstable", "sandbox-remote-unstable"] }
```

At 1.2, those features get renamed (no `-unstable` suffix) and the
traits join the stable surface:

```toml
pi-sdk = { version = "1.2", features = ["sandbox-microvm", "sandbox-remote"] }
```

The `-unstable`-suffixed features stay as deprecated aliases for
4 MINOR before removal at 1.6.

---

## Search-and-replace table

| Old path                                   | New path                                         | When     |
|--------------------------------------------|--------------------------------------------------|----------|
| `pi_coding_agent::sdk::*`                  | `pi_sdk::*`                                      | 0.1.0    |
| `LocalProcessProvider::with_defaults()`    | `LocalProcessProvider::with_readonly_defaults()` (safer) or unchanged | 0.1.0    |
| `ToolGate::approve(name, input)`           | `ToolGate::approve(ctx, name, input)`            | 0.1.0 (breaking; pre-1.0 ok per §3)   |
| `ToolRegistry::register(tool)` returns `()` | returns `Result<(), DuplicateName>`              | 0.1.0 (breaking; pre-1.0 ok per §3)   |
| `pi_sandbox_rootfs::ROOTFS_VERSION`        | `pi_sandbox::microvm::ROOTFS_VERSION`            | 0.1.0    |
| `Settings { ..Settings::default() }` literal | `Settings::builder().<...>.build()`            | 1.0 (builder ships in 0.x as additive prerequisite, polish-8) |
| `AuthStorage::from_env_explicit(&[...])` slice form | bare-array `[("a","b")]` or `slice.iter().copied()` (single IntoIterator signature) | polish-13 (collapse) |
| `RuntimeConfig::builder().build()?` then mutate `cfg.max_session_tokens = N` | `.build()?.with_max_session_tokens(N)` (post-build setter, polish-6) | 0.1.0 |

## See also

- [CHANGELOG.md](CHANGELOG.md) — full per-release change list.
- [RFD 0027 §3](../../rfd/0027-pi-rs-sdk.md) — stability commitment + deprecation policy.
- [RFD 0027 §6](../../rfd/0027-pi-rs-sdk.md) — distribution + migration guide format.
