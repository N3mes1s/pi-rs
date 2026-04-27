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

## Round-4 Results (final)

| # | Idea | score | startup_µs | size_KiB | verdict |
|---|------|-------|-----------:|---------:|---------|
| 1 | baseline (post-r3) | 6814 | 1785 | 5029 | keep (anchor) |
| 2 | `strip = true` instead of `"symbols"` | 6731 | 1702 | 5029 | discard (no size delta — already minimal) |
| 3 | UPX `--best --lzma` post-link | 158204 | 156553 | 1651 | discard (-67% size, +88× startup) |
| 4 | default target = `x86_64-unknown-linux-musl` | **5839** | **680** | 5159 | **keep** (-14.3%, startup -62%) |
| 5 | `std::process::exit(0)` after fast-path | 5855 | 696 | 5159 | discard (within noise) |
| 6 | non-PIE (`relocation-model=static`) + objcopy strip `.eh_frame*`/`.gcc_except_table` | **5303** | 736 | **4567** | **keep** (-9.2%, size -592 KiB) |

**Net round 4: 6814 → 5303 (−22.2%, −1511 score points).**
- Cold startup: **1785 µs → 736 µs (−59%)**
- On-disk size: **5029 KiB → 4567 KiB (−9.2%)**

### How the wins fit together

- *musl static link* removes the dynamic loader hand-off entirely (`/lib64/ld-linux-x86-64.so.2` no longer involved). The kernel maps the binary, jumps to `_start`, done. That alone saves 1.1 ms of cold-start.
- *`relocation-model=static`* turns the ELF from `ET_DYN` (PIE) into `ET_EXEC`, which means rustc no longer has to emit `.rela.dyn` runtime relocations — those are ~312 KiB on disk and a small one-shot CPU pass at startup.
- *post-link `eh_frame` strip* — with `panic = "abort"` already in `[profile.release]`, the unwind tables generated for libstd / libcore are unreachable. `objcopy` removes them after link, saving another ~286 KiB. Safe because nothing in our code calls `set_hook` to inspect a panic.

All wins reproduce via `make build-release`, which mirrors the `target/x86_64-unknown-linux-musl/release/pi` artefact to `target/release/pi` and runs the post-link `objcopy` so downstream callers see the optimised binary at the canonical path.

### Out of scope but promising

- `-Z build-std=std,panic_abort` on nightly — would let rustc rebuild libstd
  without unwind metadata, recovering more `.eh_frame` from inside libstd
  itself.  Estimated extra ~200 KiB.
- `cargo bloat --crates` to find the largest contributors in the 3.0 MiB
  `.text`. Likely candidates: `reqwest`/`rustls`, `clap`, `regex`. Cutting
  any of them is a multi-day refactor.
- Replace `clap` (already pre-sniffed for fast-path) with a hand-rolled
  argv parser everywhere — would let us drop the `derive` proc-macro and
  much of the help/usage printing code.

