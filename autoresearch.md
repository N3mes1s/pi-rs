# Autoresearch r4: pi-rs binary perf+size — musl/upx/post-link round

## Objective
Drive the composite `score = startup_us + size_kib` of `./target/release/pi`
as low as possible.  The binary is a CLI; `pi --list` (fast-path, sync, no
tokio, no tracing) is the cold-start benchmark workload.

## Metrics

- **Primary**: `score` (µs+KiB, lower is better)
- **Secondary**:
  - `startup_us` — median of 5 trials × 200 sequential `pi --list` runs
  - `size_kib` — `stat -c%s` of `./target/release/pi`

## How to Run

`./autoresearch.sh`

Knobs the script honours (so experiments don't have to hand-edit the bench):

- `PI_AR_TARGET=x86_64-unknown-linux-musl` — pick a non-default rustc target.
  Output is mirrored back to `./target/release/pi` so callers stay simple.
- `PI_AR_POSTBUILD='upx --best -q'` — bash snippet receiving the binary path
  as `$1`; runs after the build.  Use for upx, eu-strip, sstrip, etc.

## Files in Scope

- `Cargo.toml` (workspace `[profile.release]`)
- `crates/pi-coding-agent/src/bin/pi.rs` — main entry, fast-path dispatch
- `crates/pi-coding-agent/src/cmd.rs` — list/config/update implementations
- `crates/pi-coding-agent/src/context.rs` — path helpers
- `.cargo/config.toml` (linker / target flags)
- `autoresearch.sh` — the bench/build wrapper

## Off Limits
- Don't break `pi --list`, `pi --config`, or any of the existing async modes.
- No source-level behaviour changes — output of every CLI surface stays
  byte-identical.

## What's Been Tried (rounds 1–3 distilled)

Kept (already in baseline):
- `#[tokio::main]` removed for sync subcommands (r1 −382µs)
- defer `tracing_subscriber::fmt().init()` past fast-path (r1 −188µs)
- `panic = "abort"` (r1)
- `lto = "fat"`, `codegen-units = 1`, `strip = "symbols"` (r1)
- `opt-level = "z"` (r2 −1657 score)
- argv pre-sniff before `Cli::parse()` (r2)

Discarded (within noise or harmful):
- `lld` linker via `.cargo/config.toml` (no win, r3)
- PGO with `-Cprofile-use` from the same workload (regression, r3)
- Drop `dirs` crate / use `HOME` env directly (within noise, r3)
- Drop 4 unused workspace deps (regex/shellexpand/glob/walkdir) (within noise, r3)
- `tracing/release_max_level_warn` (within noise, r3)
- Tighten tokio per-crate features from `full` to minimal (broke tests, r2)

## Round-4 Plan (≤ 6 experiments)

1. **Baseline** under the new 5-trial median bench (re-anchor noise floor).
2. **`strip = true`** in `[profile.release]` — full strip vs `"symbols"`
   only.  Free if it works.
3. **musl static target** (`x86_64-unknown-linux-musl`) — removes dynamic
   loader/PLT cost from cold start, and a static musl binary often shrinks
   vs glibc once `panic=abort` + `lto=fat` already removed unwind tables.
4. **UPX `--best --lzma`** post-link compression — typically 60–70% size
   cut, costs a few hundred µs to self-decompress.  Worth checking the
   trade in this score function.
5. **`std::process::exit(0)`** at end of fast-path subcommands — skip
   Rust's static-dtor / TLS dtor / atexit runs on exit.  Cheap, safe.
6. **Stack the winners.**
