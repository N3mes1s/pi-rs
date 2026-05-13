# Autoresearch session: pi-rs startup time

**Objective:** minimise the wall-clock time from spawning the `pi`
binary to first useful execution (currently `pi --list` returning).

**Metric:** `startup_us` — microseconds per `pi --list` invocation,
averaged over 200 sequential runs (the `autoresearch.sh` script).

**Direction:** lower is better.

**Baseline (release, lto=thin, codegen-units=1, strip=symbols):**
2338 µs/run.

## In scope

* `crates/pi-ai/src/registry.rs` — model/provider registration
  loop (16 providers, dozens of models).
* `crates/pi-coding-agent/src/startup.rs` — `assemble()` builds
  Settings, AuthStorage, ModelRegistry, ToolRegistry, prompts,
  skills, themes, keymap, extensions, slash registry, etc.
* `crates/pi-coding-agent/src/cmd.rs::run_list` — what `pi --list`
  actually does after assemble.
* `crates/pi-coding-agent/src/main.rs` (`bin/pi.rs`) — pre-assemble
  fast paths.
* `Cargo.toml` `[profile.release]` settings.
* Top-level `pub mod` ordering in `lib.rs` files (Rust generally
  doesn't care, but binary loader / inlining hints can change).

## Out of scope

* Behaviour changes — every existing test must still pass.
* External crate version bumps (we keep the lockfile).

## Ideas to try (agent picks the order)

1. Make `--list` exit before `assemble()` — it doesn't need
   most of the startup machinery; `discover_packages` is enough.
2. Lazy-load `default_providers()` in `ModelRegistry::new` — it's
   only iterated when `resolve()` or `providers()` is called.
3. Skip `discover_context_files` walk for fast subcommands.
4. Strip extra panic-handler / unwind metadata via release flags.
5. Avoid env-var sweeps when no auth file exists.
6. Defer `notify` watcher creation in `HotThemes` until first
   draw.
7. Drop `tracing_subscriber::fmt().init()` when stderr is a pipe
   (may save constructor cost).
