//! Append-only JSONL log for autoresearch experiment runs.
//!
//! Each call to [`JsonlLog::append`] atomically writes one JSON line to the
//! log file (an entry is never partially written — we write the full line and
//! then flush before returning).
//!
//! [`JsonlLog::read_all`] reads every line back in insertion order.
//! [`JsonlLog::count_kept_results`] and [`JsonlLog::best_result`] provide
//! derived views without pulling every field into memory.

use std::fs::OpenOptions;
use std::io::{self, BufRead, BufReader, Write};
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::autoresearch::session::{MetricDirection, SessionConfig};

// ── LogEntry ─────────────────────────────────────────────────────────────────

/// A single line in `autoresearch.jsonl`.
///
/// The `id` field is a random UUID-v4 string; `timestamp` is a Unix
/// timestamp in seconds (UTC).  The `kind` field is flattened into the
/// top-level JSON object via `#[serde(flatten)]` so every log line is
/// self-describing.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LogEntry {
    pub id: String,
    pub timestamp: i64,
    #[serde(flatten)]
    pub kind: LogEntryKind,
}

// ── LogEntryKind ─────────────────────────────────────────────────────────────

/// Discriminated-union of all event types that can appear in the log.
///
/// Serialised with a `"kind"` tag so lines are self-describing even when read
/// outside of Rust (e.g. by jq).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum LogEntryKind {
    /// Session was initialised; contains the full [`SessionConfig`].
    Init {
        config: SessionConfig,
    },
    /// An experiment run was started.
    Run {
        /// One-line human description of the change being tested.
        idea: String,
        /// Git commit SHA recorded **before** the run's edits are applied.
        commit_before: String,
    },
    /// An experiment run completed.
    Result {
        /// References the `id` of the corresponding [`LogEntryKind::Run`] entry.
        run_id: String,
        /// Value reported by `METRIC <name>=<value>` in the benchmark stdout.
        metric_value: f64,
        /// Wall-clock duration of the benchmark command.
        duration_ms: u64,
        /// `true` if the result was an improvement and the commit was kept.
        kept: bool,
        /// Git commit SHA after the run (either the new commit or the
        /// reverted HEAD, depending on `kept`).
        commit_after: String,
        /// `true` if `autoresearch.checks.sh` exited 0 (or didn't exist).
        checks_passed: bool,
    },
    /// A lifecycle hook was executed.
    Hook {
        hook: String,
        output: String,
    },
    /// The experiment loop was stopped.
    Stop {
        reason: String,
    },
}

// ── JsonlLog ─────────────────────────────────────────────────────────────────

/// Append-only JSONL log backed by a file at `path`.
///
/// Thread-safety: the underlying file is opened fresh for each [`append`]
/// call with `O_APPEND` semantics, which is safe for concurrent writers on
/// POSIX systems.  For multi-threaded use within a single process wrap in
/// a `Mutex`.
pub struct JsonlLog {
    path: PathBuf,
    /// Direction used by [`best_result`] to determine which value "wins".
    direction: MetricDirection,
}

impl JsonlLog {
    /// Create a log handle.  The file is created on first [`append`] call if
    /// it does not already exist.
    ///
    /// `direction` controls whether [`best_result`] returns the minimum or
    /// maximum observed `metric_value`.
    pub fn new(path: impl Into<PathBuf>, direction: MetricDirection) -> Self {
        Self {
            path: path.into(),
            direction,
        }
    }

    /// Append a new [`LogEntry`] to the log and return it.
    ///
    /// Generates a fresh UUID v4 `id` and the current UTC Unix timestamp.
    pub fn append(&self, kind: LogEntryKind) -> io::Result<LogEntry> {
        let entry = LogEntry {
            id: new_id(),
            timestamp: utc_now(),
            kind,
        };
        let mut line = serde_json::to_string(&entry)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
        line.push('\n');

        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)?;
        file.write_all(line.as_bytes())?;
        file.flush()?;
        Ok(entry)
    }

    /// Read every log entry in insertion order.
    pub fn read_all(&self) -> io::Result<Vec<LogEntry>> {
        if !self.path.exists() {
            return Ok(Vec::new());
        }
        let file = std::fs::File::open(&self.path)?;
        let reader = BufReader::new(file);
        let mut entries = Vec::new();
        for (lineno, line) in reader.lines().enumerate() {
            let line = line?;
            if line.trim().is_empty() {
                continue;
            }
            let entry: LogEntry = serde_json::from_str(&line).map_err(|e| {
                io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!("line {}: {}", lineno + 1, e),
                )
            })?;
            entries.push(entry);
        }
        Ok(entries)
    }

    /// Count the number of [`LogEntryKind::Result`] entries where `kept` is `true`.
    pub fn count_kept_results(&self) -> io::Result<usize> {
        Ok(self
            .read_all()?
            .into_iter()
            .filter(|e| matches!(&e.kind, LogEntryKind::Result { kept, .. } if *kept))
            .count())
    }

    /// Return the best observed `metric_value` among all kept Result entries,
    /// honouring [`MetricDirection`].  Returns `None` when the log is empty
    /// or contains no Result entries.
    pub fn best_result(&self) -> io::Result<Option<f64>> {
        let values: Vec<f64> = self
            .read_all()?
            .into_iter()
            .filter_map(|e| {
                if let LogEntryKind::Result { metric_value, kept, .. } = e.kind {
                    if kept { Some(metric_value) } else { None }
                } else {
                    None
                }
            })
            .collect();

        if values.is_empty() {
            return Ok(None);
        }

        let best = match self.direction {
            MetricDirection::Lower => values
                .iter()
                .copied()
                .fold(f64::INFINITY, f64::min),
            MetricDirection::Higher => values
                .iter()
                .copied()
                .fold(f64::NEG_INFINITY, f64::max),
        };
        Ok(Some(best))
    }
}

// ── Helpers ───────────────────────────────────────────────────────────────────

/// Generate a UUID-v4-style random ID without pulling in the full `uuid` crate.
fn new_id() -> String {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};
    use std::time::{SystemTime, UNIX_EPOCH};

    // Mix thread ID + time + a cheap counter for enough uniqueness in tests.
    static COUNTER: std::sync::atomic::AtomicU64 =
        std::sync::atomic::AtomicU64::new(0);

    let count = COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.subsec_nanos())
        .unwrap_or(0);

    let mut h = DefaultHasher::new();
    count.hash(&mut h);
    nanos.hash(&mut h);
    std::thread::current().id().hash(&mut h);
    let bits = h.finish();

    // Format as a compact hex string that looks distinct in logs.
    format!(
        "{:08x}-{:04x}-4{:03x}-{:04x}-{:012x}",
        (bits >> 32) as u32,
        ((bits >> 16) & 0xffff) as u16,
        (bits & 0x0fff) as u16,
        0x8000 | ((bits >> 48) & 0x3fff) as u16,
        count.wrapping_mul(0x9e37_79b9_7f4a_7c15) & 0x0000_ffff_ffff_ffff,
    )
}

/// Current UTC Unix timestamp in seconds.
fn utc_now() -> i64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}
