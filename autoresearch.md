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

(populated during the loop)

## Ideas Backlog

1. `lto = "thin"` + `codegen-units = 16` baseline-buster.
2. `lto = false` (no LTO) + `codegen-units = 16` + still `opt-level = "z"`.
3. mold linker on the musl static target.
4. Drop reqwest features we don't use (`stream` if unused, etc.) — but
   careful of behaviour change.
5. Move `[profile.release]` to keep `opt-level = "z"` only on
   `pi-coding-agent` and let deps build at `s` (or vice-versa).
6. Combination: thin LTO + cgu=16 + mold.
