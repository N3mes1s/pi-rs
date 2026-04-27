You are running an **autoresearch loop** on the pi-rs Rust workspace
at /home/user/Playground/pi-rs to *jointly* optimise startup time and
binary size.

Session: `/home/user/Playground/pi-rs/.autoresearch-perf-size/`
- `autoresearch.config.json`: metric=`score`, direction=lower
- `autoresearch.sh`: emits THREE METRIC lines per run:
    `METRIC startup_us=<N>`  (mean of 200 `pi --list` invocations)
    `METRIC size_kib=<N>`    (stripped release binary size)
    `METRIC score=<N>`       (startup_us + size_kib — combined)
- `autoresearch.checks.sh`: full `cargo test --workspace` must pass

Tools available:
- `autoresearch_init`, `autoresearch_run`, `autoresearch_log`
  (use the parameter name `session_dir` — the `root` alias is
   deprecated)
- `read`, `write`, `edit`, `bash`, `grep`, `find`, `ls`

**Workflow:**

1. Call `autoresearch_init` ONCE with:
     session_dir = "/home/user/Playground/pi-rs/.autoresearch-perf-size"
     working_dir = "/home/user/Playground/pi-rs"
     name        = "pi-rs-perf-size"
     metric      = "score"
     unit        = "µs+KiB"
     direction   = "lower"
     max_iterations = 7

2. Run a baseline:
     autoresearch_run with command:
       "bash /home/user/Playground/pi-rs/.autoresearch-perf-size/autoresearch.sh"
     idea: "baseline (post-startup-pass)"
   The previous startup-only autoresearch pass already shipped:
   - `#[tokio::main]` removed for fast paths
   - tracing_subscriber deferred
   - `panic = "abort"`
   - `lto = "fat"`
   So baseline ≈ score 8440 (startup 1678 µs, size 6762 KiB).

3. **Loop** for up to 7 experiments. Pick from this idea bank
   (or invent your own; both is fine):

   **Size-leaning ideas:**
   - Tighten `tokio` features in `crates/*/Cargo.toml` from `full`
     to the minimum each crate actually uses. The fast path
     doesn't need fs/net/sync/parking_lot. Verify with
     `cargo tree -p <crate> -e features` if needed.
   - Drop unused workspace deps (`hmac`, `sha1`, `hex`, `which`,
     `shellexpand`, `glob`) if no source touches them.
   - Use `serde_json` without `arbitrary_precision`/`raw_value`.
   - Replace `chrono` with `time` (smaller) — only if used in 2-3
     places and the migration is mechanical.
   - Strip more aggressively: `RUSTFLAGS="-Cstrip=symbols
     -Clink-arg=-Wl,--gc-sections"` in `.cargo/config.toml`.
   - Disable LLVM coverage instrumentation in release.

   **Startup-leaning ideas:**
   - Argv-sniff `--list/--config/--update/--install/--help` BEFORE
     `Cli::parse()` runs, in `bin/pi.rs`. clap's command-tree build
     is non-trivial; for a flag like `--list` we can dispatch via a
     short manual match on `args[1]`.
   - Skip building `AuthStorage::open(...)` in startup for
     subcommands that don't need credentials.
   - Lazy-init `default_providers()` — only build the registry
     when `resolve()` is called.
   - Remove `dirs::home_dir()` for the env-fast path (small but
     real).
   - Replace `walkdir` / `ignore` walks for fast subcommands.

   **Joint ideas (improve both):**
   - Cargo.toml `[profile.release]` flags: try `opt-level = "z"`
     (size) vs current `"3"` — measure both; the score may drop
     on size while not regressing startup much.
   - Remove `tokio` from `pi-tools` and `pi-tui` if those crates
     don't actually need a runtime (they shouldn't).

4. After each idea:
   - make the smallest possible change
   - `autoresearch_run` with the new idea description
   - if `score` improved by ≥ 100 (≥ ~1.2% of baseline),
     `autoresearch_log` with `kept=true`
   - otherwise `kept=false` (auto-revert via git)

5. **Critical safety**: `autoresearch.checks.sh` must keep passing.
   Run it manually (`bash .autoresearch-perf-size/autoresearch.checks.sh`)
   after any experiment that touches Cargo.toml dependencies. If a
   change breaks tests, revert immediately and don't count it as
   a kept experiment.

6. After 7 experiments OR when you've exhausted ideas, output:

   SUMMARY:
   - baseline score / final score / Δ%
   - per-experiment table: idea | startup_us | size_kib | score | kept?
   - top 3 most impactful changes
   - what you'd try next

Stay focused — joint optimisation, not refactoring. Keep `Cli`
flags backwards compatible. Don't introduce new dependencies.
