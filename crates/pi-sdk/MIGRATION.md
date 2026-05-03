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

No breaking changes since 0.1.0 (not yet published).

---

## 0.x → 1.0 (planned, not yet shipped)

The 1.0 release will collapse 0.x's residual ergonomic warts. The
expected migration shape:

### 1. `BuildConfig` deprecated in favour of `RuntimeConfig::builder()`

Pre-1.0 (current 0.x — both work):

```rust
let cfg = build_runtime_config(BuildConfig {
    auth: AuthStorage::from_env_explicit(&[("anthropic", "MY_KEY")])?,
    tools: ToolRegistry::with_readonly_extras(),
    settings: Settings::default(),
    ..BuildConfig::default()
});
```

Post-1.0 (recommended):

```rust
let cfg = RuntimeConfig::builder()
    .auth_storage(AuthStorage::from_env_explicit(&[("anthropic", "MY_KEY")])?)
    .tools(ToolRegistry::with_readonly_extras())
    .settings(Settings::default())
    // ... other required setters
    .build()?;
```

`BuildConfig` continues to compile under 1.x with a `#[deprecated]`
warning until 1.0+4 MINOR releases (~6 months past 1.0) per RFD §3
deprecation policy. Final removal in 2.0.

### 2. `AuthStorage::from_env()` removed

Pre-1.0 (`#[deprecated]` since 0.1):

```rust
let auth = AuthStorage::from_env();  // slurps 17 env vars
```

Post-1.0 (only path):

```rust
let auth = AuthStorage::from_env_explicit(&[
    ("anthropic", "MY_TENANT_ANTHROPIC_KEY"),
    ("openai",    "MY_TENANT_OPENAI_KEY"),
])?;
```

### 3. `ToolRegistry::with_extras()` renamed `with_unsafe_extras()`

Both names exist in 0.x via H7. After 1.0+4 MINOR, `with_extras()`
is removed; `with_unsafe_extras()` is the only name. The semantics
(includes `bash`) are unchanged.

```rust
- let tools = ToolRegistry::with_extras();
+ let tools = ToolRegistry::with_unsafe_extras();
```

### 4. Sandbox launcher traits leave `*-unstable` features

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
| `ToolRegistry::with_extras()`              | `ToolRegistry::with_unsafe_extras()` (alias)     | 0.1.0    |
| `AuthStorage::from_env()`                  | `AuthStorage::from_env_explicit(allowlist)`      | deprecated 0.1.0; removed 1.0+4 MINOR |
| `ToolGate::approve(name, input)`           | `ToolGate::approve(ctx, name, input)`            | 0.1.0 (breaking; pre-1.0 ok per §3)   |
| `ToolRegistry::register(tool)` returns `()` | returns `Result<(), DuplicateName>`              | 0.1.0 (breaking; pre-1.0 ok per §3)   |
| `pi_sandbox_rootfs::ROOTFS_VERSION`        | `pi_sandbox::microvm::ROOTFS_VERSION`            | 0.1.0    |

## See also

- [CHANGELOG.md](CHANGELOG.md) — full per-release change list.
- [RFD 0027 §3](../../rfd/0027-pi-rs-sdk.md) — stability commitment + deprecation policy.
- [RFD 0027 §6](../../rfd/0027-pi-rs-sdk.md) — distribution + migration guide format.
