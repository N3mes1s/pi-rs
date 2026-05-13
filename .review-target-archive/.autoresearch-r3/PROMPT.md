You are running an **autoresearch loop** (round 3) on the pi-rs Rust
workspace at /home/user/Playground/pi-rs. Goal: **continue jointly
optimising startup + binary size**.

Session: `/home/user/Playground/pi-rs/.autoresearch-r3/`
- `autoresearch.config.json` (metric=score, direction=lower)
- `autoresearch.sh` emits `METRIC startup_us=<N>` (200× `pi --list`),
  `METRIC size_kib=<N>` (stripped release size), and
  `METRIC score=<N>` (sum, both lower=better, comparable units).
- `autoresearch.checks.sh` runs `cargo test --workspace --no-fail-fast`.

Tools:
- `autoresearch_init`, `autoresearch_run`, `autoresearch_log`
  → use `session_dir` (NOT the deprecated `root` alias).
- `read`, `write`, `edit`, `bash`, `grep`, `find`, `ls`.

**Already on the branch (rounds 1 + 2):**
- Sync `main`, deferred `tracing_subscriber`, `panic = "abort"`,
  `lto = "fat"`, `opt-level = "z"`, argv pre-sniff for fast-path
  subcommands.

**Baseline (just measured):** score=6664, startup=1649µs,
size=5015KiB.

**Idea bank for round 3 (Opus 4.7's own next-step suggestions):**

1. **PGO (profile-guided optimisation)** with a `--list` workload.
   Likely 5–10% on startup. Tooling available:
   - `/root/.rustup/toolchains/stable-x86_64-unknown-linux-gnu/bin/llvm-profdata`
   - `/usr/bin/llvm-profdata` (also works)
   Workflow:
   - `cargo +stable rustc --profile=release -p pi-coding-agent --
     -Cprofile-generate=/tmp/pgo-data` (or set `RUSTFLAGS` and
     rebuild the release binary)
   - run `./target/release/pi --list` and `pi --config` and
     `pi --help` ~50 times each to populate `/tmp/pgo-data/*.profraw`
   - merge: `llvm-profdata merge -o /tmp/pgo-data/merged.profdata
     /tmp/pgo-data`
   - rebuild with `RUSTFLAGS=-Cprofile-use=/tmp/pgo-data/merged.profdata`
   - benchmark
   You'll need to fold the PGO build into `autoresearch.sh` for it
   to be reproducible. Or split into `autoresearch.sh` (build with
   PGO if a marker file exists) + an extra `pgo-train.sh`. Be
   careful — the build script in autoresearch.sh has to produce
   the same binary path each time.

2. **`mold` / `lld` linker.** `/usr/bin/lld` and `/usr/bin/mold`
   may exist (check). Add to `.cargo/config.toml`:
       [target.x86_64-unknown-linux-gnu]
       linker = "clang"
       rustflags = ["-Clink-arg=-fuse-ld=lld"]
   Or if mold is around: `-Clink-arg=-fuse-ld=mold`. May tighten
   relocation tables and the `.text` layout (cold-cache wins).

3. **Drop `dirs` dependency** (~30–60 KiB, plus its `dirs-sys`
   transitive). Used in 1–2 places (search via
   `grep -rn "dirs::" crates/`). Replace with:
       std::env::var_os("HOME").map(PathBuf::from)
   For Windows fallback we'd need `USERPROFILE`, but pi-rs is
   Linux-first; this is acceptable.

4. **`reqwest` without `gzip`/`brotli`/`deflate`.** Audit
   `crates/pi-ai/Cargo.toml` and the workspace `reqwest` line.
   Currently:
       reqwest = { features = ["json", "stream", "rustls-tls-native-roots"], default-features = false }
   None of those imply gzip, but verify with `cargo tree`. If
   `reqwest` pulls `flate2`/`brotli`/`zstd` transitively, switch
   off the relevant features.

5. **Optional `nucleo-matcher`** behind a `cfg`/feature flag for
   pi-coding-agent. It's only used by the interactive picker. A
   `default-features=false` build of pi-rs that omits
   nucleo-matcher would shrink the binary visibly — but only
   when the feature is off. Adding the feature without any caller
   that disables it doesn't help. Skip this idea if it doesn't
   actually shrink the default release.

6. **Replace `chrono` with a 30-line `std::time::SystemTime`
   wrapper.** Audit usage with `grep -rn "chrono::" crates/`. If
   we only call `Utc::now().timestamp_millis()` and
   `format("%Y-%m-%d %H:%M")`, we can drop chrono entirely.

7. **`reqwest` blocking out of the dependency tree.** Confirm
   `reqwest = { default-features = false, features = […] }` does
   NOT pull `reqwest::blocking`.

**Workflow:**
1. `autoresearch_init` ONCE with:
     session_dir = "/home/user/Playground/pi-rs/.autoresearch-r3"
     working_dir = "/home/user/Playground/pi-rs"
     name = "pi-rs-r3-pgo"
     metric = "score" / unit = "µs+KiB" / direction = "lower"
     max_iterations = 7

2. Baseline `autoresearch_run` with idea: "round-3 baseline (post r1+r2)".

3. **For each of up to 7 experiments:**
   - Pick an idea (or invent one). Make the smallest change that
     could possibly work.
   - `autoresearch_run` with a clear idea description.
   - **Threshold:** keep if score improved by ≥ **80** (≈ 1.2% of
     baseline). Otherwise revert.
   - Run `autoresearch.checks.sh` if the change touches Cargo.toml
     dependencies — broken tests = mandatory revert.

4. After 7 experiments OR exhaustion, output:

   SUMMARY:
   - baseline / final / Δ%
   - per-experiment table: idea | startup_us | size_kib | score | kept?
   - top 3 changes that worked
   - what you'd try next (if anything is left)

Stay disciplined: no behaviour changes, no flag renames. Cargo
locks acceptable. Don't burn iterations on size wins under 50 KiB.
