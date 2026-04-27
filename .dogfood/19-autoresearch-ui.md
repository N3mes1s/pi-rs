You are working inside the pi-rs Cargo workspace at /home/user/Playground/pi-rs.

Goal: pi-autoresearch confidence scoring, dashboard widget, slash
commands, and hook support. Builds on Pass 18.

Step 1. `crates/pi-coding-agent/src/autoresearch/confidence.rs`:

    pub struct ConfidenceScore {
        pub multiplier: f64,   // best_improvement / MAD
        pub band: ConfidenceBand,
    }

    #[derive(Debug, Clone, Copy, PartialEq)]
    pub enum ConfidenceBand { Green, Yellow, Red, Insufficient }

    pub fn compute(samples: &[f64], baseline: f64, direction: MetricDirection)
        -> ConfidenceScore

  Compute the Median Absolute Deviation (MAD) of `samples`,
  measure |best_improvement|/MAD against the baseline (where
  improvement is `baseline - best` for Lower direction, `best -
  baseline` for Higher). Return Insufficient if samples.len() < 3.
  Bands: ≥2.0 Green, ≥1.0 Yellow, otherwise Red.

Step 2. Dashboard widget — pure rendering, no I/O. Add to
`crates/pi-coding-agent/src/autoresearch/dashboard.rs`:

    pub struct DashboardState {
        pub session_name: String,
        pub runs: usize,
        pub kept: usize,
        pub metric_name: String,
        pub baseline: f64,
        pub current_best: f64,
        pub direction: MetricDirection,
        pub confidence: ConfidenceScore,
    }

    pub fn render_inline(state: &DashboardState) -> String

  Returns the single-line widget like:
  `🔬 autoresearch 12 runs 8 kept │ ★ total_µs: 15,200 (-12.3%) │ conf: 2.1×`

  `render_table(state, runs: &[(String, f64, bool)]) -> String`
  returns a multi-line table for the expanded view.

Step 3. Slash commands. In
`crates/pi-coding-agent/src/slash.rs` register `/autoresearch` as a
built-in. Handle in `modes/interactive.rs`:
- `/autoresearch <text>` — enter mode (calls
  `Session::new(...)` if no `autoresearch.config.json` exists,
  otherwise resume), pushes a Note block: "autoresearch active".
- `/autoresearch off` — sets a `view.autoresearch_active = false`
  flag; preserves jsonl.
- `/autoresearch clear` — deletes `autoresearch.{jsonl,md,config.json}`
  in cwd.
- `/autoresearch export` — calls
  `crate::autoresearch::dashboard::render_table` and writes the
  output to `autoresearch-dashboard.html`. (No browser-open — just
  print the path.)

Step 4. Hooks. In
`crates/pi-coding-agent/src/autoresearch/hooks.rs`:

    pub async fn run_before(session: &Session, run_state: &serde_json::Value)
        -> Option<String>;
    pub async fn run_after(session: &Session, run_state: &serde_json::Value)
        -> Option<String>;

  Both walk `<root>/autoresearch.hooks/` for `before.sh` /
  `after.sh`, run via `bash -lc`, write the JSON state to the
  hook's stdin, capture stdout (capped 8 KB), append a `Hook`
  entry to the JSONL log, and return the captured stdout
  (intended as a steer message for the agent).

Step 5. Tests:
- `crates/pi-coding-agent/tests/autoresearch_confidence.rs`:
  MAD on a known sample, band thresholds, Insufficient case,
  Lower vs Higher direction.
- `crates/pi-coding-agent/tests/autoresearch_dashboard.rs`:
  inline render contains the right tokens, table renders all
  rows, percent formatting handles 0 baseline.
- `crates/pi-coding-agent/tests/autoresearch_hooks.rs`: write a
  fake `before.sh` that echoes its stdin to a sentinel file;
  assert the stdout is captured and the sentinel matches.
- `crates/pi-coding-agent/tests/autoresearch_slash.rs`: drive
  the `/autoresearch <text>` and `/autoresearch clear` flows
  through pure helpers (don't shell out git).

Build clean: `cargo build --workspace`
Tests green: `cargo test -p pi-coding-agent --tests autoresearch_*`

When done output: DONE.
