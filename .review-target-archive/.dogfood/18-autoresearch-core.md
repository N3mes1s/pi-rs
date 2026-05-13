You are working inside the pi-rs Cargo workspace at /home/user/Playground/pi-rs.

Goal: native Rust port of pi-autoresearch — the core experiment
loop and JSONL persistence. (Confidence + dashboard + slash
commands ship in the next dogfood pass.)

Background:
- pi-autoresearch is upstream pi's autonomous experiment-loop
  extension. It tracks runs in `autoresearch.jsonl`, edits code,
  runs benchmarks, keeps improvements, reverts regressions.
- We're porting it as a NATIVE module in pi-rs, not a subprocess
  extension — so it integrates with the agent loop and the TUI
  directly. Lives in
  `crates/pi-coding-agent/src/autoresearch/`.

Step 1. Create the module structure:

    crates/pi-coding-agent/src/autoresearch/mod.rs
    crates/pi-coding-agent/src/autoresearch/log.rs
    crates/pi-coding-agent/src/autoresearch/session.rs
    crates/pi-coding-agent/src/autoresearch/tools.rs

Step 2. In `mod.rs`:

    pub mod log;
    pub mod session;
    pub mod tools;
    pub use log::{LogEntry, LogEntryKind, JsonlLog};
    pub use session::{Session, SessionConfig, MetricDirection};
    pub use tools::{InitExperimentTool, RunExperimentTool, LogExperimentTool};

Step 3. `session.rs`:

    #[derive(Debug, Clone, Serialize, Deserialize)]
    pub struct SessionConfig {
        pub name: String,
        pub metric: String,
        pub unit: String,
        pub direction: MetricDirection,  // Lower / Higher
        pub max_iterations: Option<u32>,
        pub working_dir: Option<PathBuf>,
    }

    #[derive(Debug, Clone, Copy, Serialize, Deserialize)]
    #[serde(rename_all = "lowercase")]
    pub enum MetricDirection { Lower, Higher }

    pub struct Session {
        pub config: SessionConfig,
        pub root: PathBuf,
    }

    impl Session {
        pub fn new(root: impl Into<PathBuf>, config: SessionConfig) -> Self;
        pub fn load(root: impl AsRef<Path>) -> std::io::Result<Self>;
        pub fn save_md(&self) -> std::io::Result<()>;  // writes autoresearch.md
        pub fn config_path(&self) -> PathBuf;          // autoresearch.config.json
        pub fn jsonl_path(&self) -> PathBuf;           // autoresearch.jsonl
        pub fn md_path(&self) -> PathBuf;              // autoresearch.md
        pub fn checks_script(&self) -> PathBuf;        // autoresearch.checks.sh
        pub fn benchmark_script(&self) -> PathBuf;     // autoresearch.sh
    }

Step 4. `log.rs` — append-only JSONL:

    #[derive(Debug, Clone, Serialize, Deserialize)]
    pub struct LogEntry {
        pub id: String,
        pub timestamp: i64,
        #[serde(flatten)]
        pub kind: LogEntryKind,
    }

    #[derive(Debug, Clone, Serialize, Deserialize)]
    #[serde(tag = "kind", rename_all = "snake_case")]
    pub enum LogEntryKind {
        Init { config: SessionConfig },
        Run { idea: String, commit_before: String },
        Result {
            run_id: String,
            metric_value: f64,
            duration_ms: u64,
            kept: bool,
            commit_after: String,
            checks_passed: bool,
        },
        Hook { hook: String, output: String },
        Stop { reason: String },
    }

    pub struct JsonlLog { path: PathBuf }
    impl JsonlLog {
        pub fn new(path: impl Into<PathBuf>) -> Self;
        pub fn append(&self, kind: LogEntryKind) -> std::io::Result<LogEntry>;
        pub fn read_all(&self) -> std::io::Result<Vec<LogEntry>>;
        pub fn count_kept_results(&self) -> std::io::Result<usize>;
        pub fn best_result(&self) -> std::io::Result<Option<f64>>;  // honour direction
    }

Step 5. `tools.rs` — three Tool impls (use `pi_tools::Tool` trait):

- `InitExperimentTool` — input: name, metric, unit, direction.
  Writes `autoresearch.config.json` and an initial
  `autoresearch.md` with a header. Appends Init log entry.
- `RunExperimentTool` — input: command (string), idea (string).
  Records git HEAD as commit_before, runs the command via
  `bash -lc` capturing stdout, parses lines matching
  `^METRIC <name>=<number>$` to extract metric_value. Returns it.
  Appends Run log entry.
- `LogExperimentTool` — input: run_id, metric_value, kept (bool).
  If kept=false, runs `git reset --hard <commit_before>`.
  Otherwise runs `git add -A && git commit -m "autoresearch: <idea>"`.
  Appends Result log entry. Captures `checks_passed` by running
  `autoresearch.checks.sh` if it exists (exit 0 = pass).

Step 6. Tests in `crates/pi-coding-agent/tests/autoresearch_core.rs`:
- Session::new + save_md writes a markdown header.
- Config round-trip via save/load.
- JsonlLog: append → read_all preserves order + kinds.
- count_kept_results counts only Result entries with kept=true.
- best_result honours Lower/Higher direction.
- Tool: parsing `METRIC name=42.5` from stdout works.
- Tool: parsing fails when no METRIC line present (returns None).

Build clean: `cargo build --workspace`
Tests green: `cargo test -p pi-coding-agent --test autoresearch_core`

When done output: DONE.
