//! Append-only JSONL log for autoresearch sessions.
//!
//! On-disk schema is the upstream pi-autoresearch one (see
//! `davebcn87/pi-autoresearch/extensions/pi-autoresearch/jsonl.ts`):
//!
//! ```text
//! {"type":"config","name":"…","metricName":"total_µs","metricUnit":"µs","bestDirection":"lower"}
//! {"run":1,"commit":"abc1234","metric":1620,"metrics":{"size_kib":5015},"status":"keep","description":"…","timestamp":1777278226000,"confidence":null,"iterationTokens":null,"asi":{"note":"…"}}
//! {"run":2,"commit":"abc1234","metric":1700,"metrics":{},"status":"discard","description":"…","timestamp":1777278284000,"confidence":1.4,"iterationTokens":null}
//! ```
//!
//! Re-running `init_experiment` writes a new config header and starts a new
//! "segment" (= empty subsequent run set). State reconstruction on resume
//! reads the latest header.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

// ── primitives ───────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum BestDirection {
    Lower,
    Higher,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RunStatus {
    Keep,
    Discard,
    Crash,
    ChecksFailed,
}

impl RunStatus {
    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "keep" => Self::Keep,
            "discard" => Self::Discard,
            "crash" => Self::Crash,
            "checks_failed" => Self::ChecksFailed,
            _ => return None,
        })
    }

    pub fn is_kept(self) -> bool {
        matches!(self, Self::Keep)
    }
}

// ── entries ──────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConfigEntry {
    /// Always `"config"`.
    #[serde(rename = "type")]
    pub kind: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(rename = "metricName", skip_serializing_if = "Option::is_none")]
    pub metric_name: Option<String>,
    #[serde(rename = "metricUnit", skip_serializing_if = "Option::is_none")]
    pub metric_unit: Option<String>,
    #[serde(rename = "bestDirection", skip_serializing_if = "Option::is_none")]
    pub best_direction: Option<BestDirection>,
}

impl ConfigEntry {
    pub fn new(
        name: impl Into<String>,
        metric_name: impl Into<String>,
        metric_unit: impl Into<String>,
        direction: BestDirection,
    ) -> Self {
        Self {
            kind: "config".into(),
            name: Some(name.into()),
            metric_name: Some(metric_name.into()),
            metric_unit: Some(metric_unit.into()),
            best_direction: Some(direction),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RunEntry {
    pub run: u32,
    pub commit: String,
    pub metric: f64,
    #[serde(default)]
    pub metrics: BTreeMap<String, f64>,
    pub status: RunStatus,
    pub description: String,
    pub timestamp: i64,
    #[serde(default)]
    pub confidence: Option<f64>,
    #[serde(rename = "iterationTokens", default)]
    pub iteration_tokens: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub asi: Option<serde_json::Value>,

    // ── RAO (RFD 0032): recursive-delegation fields ──────────────────────────
    /// Delegation depth in the recursive execution tree.
    /// `0` = top-level (root) agent; `1` = first child; etc.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub depth: Option<u32>,
    /// Delegation bonus applied to this run:
    /// `λ × mean(child_success_rate)`.  `None` when no children were spawned.
    #[serde(rename = "delegationBonus", default, skip_serializing_if = "Option::is_none")]
    pub delegation_bonus: Option<f64>,
    /// Run numbers of child experiments spawned by this run.
    #[serde(rename = "childRunIds", default, skip_serializing_if = "Vec::is_empty")]
    pub child_run_ids: Vec<u32>,
}

// ── log file ─────────────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct JsonlLog {
    pub path: PathBuf,
}

impl JsonlLog {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }

    pub fn append_config(&self, entry: &ConfigEntry) -> std::io::Result<()> {
        self.append_line(&serde_json::to_string(entry)?)
    }

    pub fn append_run(&self, entry: &RunEntry) -> std::io::Result<()> {
        self.append_line(&serde_json::to_string(entry)?)
    }

    fn append_line(&self, line: &str) -> std::io::Result<()> {
        use std::io::Write;
        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let mut f = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)?;
        f.write_all(line.as_bytes())?;
        f.write_all(b"\n")?;
        Ok(())
    }

    pub fn read_lines(&self) -> std::io::Result<Vec<String>> {
        if !self.path.exists() {
            return Ok(Vec::new());
        }
        let txt = std::fs::read_to_string(&self.path)?;
        Ok(txt
            .lines()
            .filter(|l| !l.trim().is_empty())
            .map(|s| s.to_string())
            .collect())
    }

    pub fn read_runs(&self) -> std::io::Result<Vec<RunEntry>> {
        let mut out = Vec::new();
        for line in self.read_lines()? {
            if let Ok(v) = serde_json::from_str::<serde_json::Value>(&line) {
                if v.get("run").is_some() {
                    if let Ok(r) = serde_json::from_value::<RunEntry>(v) {
                        out.push(r);
                    }
                }
            }
        }
        Ok(out)
    }

    pub fn read_latest_config(&self) -> std::io::Result<Option<ConfigEntry>> {
        let mut last: Option<ConfigEntry> = None;
        for line in self.read_lines()? {
            if let Ok(v) = serde_json::from_str::<serde_json::Value>(&line) {
                if v.get("type").and_then(|t| t.as_str()) == Some("config") {
                    if let Ok(c) = serde_json::from_value::<ConfigEntry>(v) {
                        last = Some(c);
                    }
                }
            }
        }
        Ok(last)
    }

    pub fn next_run_number(&self) -> std::io::Result<u32> {
        Ok(self
            .read_runs()?
            .iter()
            .map(|r| r.run)
            .max()
            .map(|n| n + 1)
            .unwrap_or(1))
    }

    pub fn best_kept(&self, direction: BestDirection) -> std::io::Result<Option<f64>> {
        let kept: Vec<f64> = self
            .read_runs()?
            .iter()
            .filter(|r| r.status == RunStatus::Keep)
            .map(|r| r.metric)
            .collect();
        if kept.is_empty() {
            return Ok(None);
        }
        Ok(Some(match direction {
            BestDirection::Lower => kept.iter().copied().fold(f64::INFINITY, f64::min),
            BestDirection::Higher => kept.iter().copied().fold(f64::NEG_INFINITY, f64::max),
        }))
    }

    pub fn baseline_metric(&self) -> std::io::Result<Option<f64>> {
        Ok(self.read_runs()?.first().map(|r| r.metric))
    }
}

/// `<working_dir>/autoresearch.jsonl` — matches upstream's path helper.
pub fn jsonl_path(working_dir: &Path) -> PathBuf {
    working_dir.join("autoresearch.jsonl")
}
