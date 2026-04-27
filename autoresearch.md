# Autoresearch r5: clean-build wall-clock for `pi-coding-agent`

## Objective

Minimise the **clean release-build wall-clock time** of:

```
cargo build --release -p pi-coding-agent
```

i.e. the time from an empty `target/` directory to a finished, stripped
`./target/release/pi` binary. Every iteration of `autoresearch.sh`
deletes `target/` first, so we measure full cold builds.

## Hard size guardrail

The current binary is ≈ 4567 KiB stripped. Track `size_kib` as a
secondary metric and **discard any change that pushes it past 4700 KiB**.
Past optimisation work (in `.autoresearch-startup`, `.autoresearch-perf-size`,
`.autoresearch-r3`, `.autoresearch-r4`) bought that size; we must not
unwind it.

## Metrics

- **Primary**: `build_s` — clean-build wall-clock seconds (lower is better).
- **Secondary**:
  - `size_kib` — `stat -c%s` of the stripped final binary, in KiB.
  - `startup_us` *(optional)* — only sampled when build succeeds quickly,
    so the startup work isn't silently regressed. Not a hard gate.

## How to Run

`./autoresearch.sh` — wipes `target/`, runs `cargo build --release -p pi-coding-agent`,
emits `METRIC build_s=…`, `METRIC size_kib=…`.

## Files in Scope

- `Cargo.toml` — workspace `[profile.release]` (lto, codegen-units,
  opt-level, debug, incremental, strip, panic).
- `.cargo/config.toml` — target, rustflags, linker selection
  (mold / lld), parallel-frontend flag.
- `crates/*/Cargo.toml` — feature flags on heavy deps (reqwest,
  tokio, regex, etc.) — pruning features cuts code & monomorphisation.
- `Makefile` — only the `build-release` post-link strip pipeline, if
  we want to take eh_frame strip out of the hot path.
- `autoresearch.sh` — bench wrapper, may pass env vars.

## Off Limits

- Don't break `pi --list`, `pi --config`, `pi --update`, `pi --install`
  or async modes — `autoresearch.checks.sh` exercises `pi --list` and
  `pi --version` after every build.
- No source-level behaviour changes that affect CLI output.
- Don't bump dependency versions in `Cargo.lock` (workspace-level only).

## Constraints

- `size_kib` ≤ 4700 — anything above is auto-discarded.
- `cargo build --release -p pi-coding-agent` must succeed and produce
  a working binary.

## Available toolchain on this box

- `mold 2.30.0` (`/usr/bin/mold`) — installed this session.
- `ld.lld-18` (`/usr/bin/ld.lld`).
- `clang-18`.
- 4 cores, 15 GiB RAM, rustc 1.94.1 nightly-style.
- No `sccache` (irrelevant: we measure clean builds).

## Levers

| Lever | Build-time effect | Size effect |
|-------|-------------------|-------------|
| `lto = "fat"` → `"thin"` / `false` | thin ≈ 30–50% faster than fat; off ≈ 2× faster | thin grows ~2–8%, off grows more |
| `codegen-units = 1` → 16+ | huge parallelism win | small size growth (often <2%) |
| `opt-level = "z"` → `"s"`/`3` | small | `s` grows a hair, `3` grows more |
| `lto = "thin"` + `codegen-units = 16` | best build-time/size knee | usually within 5% of fat |
| linker = `mold` | shaves seconds off the link step | none |
| `-Zthreads=N` (rustc parallel frontend) | nightly-only; on stable, ignore | none |
| Drop unused features on big deps (reqwest, tokio, ring) | fewer crates compile | smaller |
| `incremental = false` in release | already off | none |

## What's Been Tried

| # | Change | build_s | size_kib | verdict |
|---|--------|---------|----------|---------|
| 1 | **Baseline** (lto=fat, cgu=1, opt=z, ld) | **73.26** | **4261** | kept |
| 2 | lto=thin + cgu=16 + mold | 60.58 | 6291 | size +2030 KiB → discard |
| 3 | lto=thin + cgu=1 + mold | 49.87 | 5614 | size +1353 KiB → discard |
| 4 | fat LTO + opt=s + mold | 79.29 | 5175 | slower AND bigger → discard |
| 5 | deps profile opt=0 cgu=256 + fat LTO | 102.07 | 4901 | unoptimised dep IR explodes LTO step → discard |
| 6 | fat LTO + opt=3 (hoping LTO skips slow size passes) | 104.43 | 6205 | -O3 inlining explodes IR → discard |
| 7 | fat LTO + cgu=16 on deps via release.package.* | 76.27 | 4337 | within noise, no win → discard |

### Conclusions

- **The 73 s baseline is at the size-optimal Pareto front** under the
  4700 KiB ceiling. Every lever that bought build-time speed cost
  >900 KiB of binary size that the size guardrail rejects.
- **Fat LTO is doing 1.3+ MB of dead-code elimination we cannot lose.**
  Switching to ThinLTO (any cgu, any linker) regresses size by
  900–2000 KiB — far past the 4700 KiB ceiling.
- The 35 s `pi-coding-agent` fat-LTO step is the **serial bottleneck**.
  Nothing we can do at the `Cargo.toml` / `.cargo/config.toml` /
  per-package-profile level shortens it without losing fat LTO.
- Within fat LTO, **`opt-level = "z"` is paradoxically the fastest**
  setting we tried: `s` and `3` both inflate IR before/during LTO,
  so the LLVM whole-program pass does *more* work, not less.
- **Mold linker** had no measurable effect on link time — the
  release link is dominated by rustc/LLVM's LTO step, not the
  system linker pass that follows it.
- Per-dep `codegen-units = 16` (parallelise within slow deps like
  rustls/tokio/regex-automata) had no wall-clock benefit because dep
  builds already overlap each other on the 4-core box; the critical
  path is the final LTO step.

### Untried but unlikely-to-help (dead-end backlog)

- **Nightly `-Z threads=N`** (parallel rustc frontend) — not on this
  toolchain and the bottleneck is LLVM, not rustc.
- **Aggressive feature pruning of `tokio` / `reqwest`** — tried in
  prior r3 session, regressed (slow startup-test gain didn't pay back
  in build-time terms). Source uses `tokio::{fs,io,net,process,sync,
  time,runtime,spawn}` so we'd lose at most one feature gate.
- **Splitting `pi-coding-agent` into smaller crates** — would just
  shift the LTO cost; fat LTO unifies the whole tree anyway.
- **PGO** — tried in r3, regressed score; orthogonal to build-time
  goal here.
