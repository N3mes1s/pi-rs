//! Experiment-session configuration and path helpers.
//!
//! A [`Session`] owns a [`SessionConfig`] (name, metric, direction, …) and
//! knows where every artefact file lives under its `root` directory:
//!
//! | Helper | File |
//! |--------|------|
//! | [`Session::config_path`] | `autoresearch.config.json` |
//! | [`Session::jsonl_path`]  | `autoresearch.jsonl`        |
//! | [`Session::md_path`]     | `autoresearch.md`           |
//! | [`Session::checks_script`] | `autoresearch.checks.sh`  |
//! | [`Session::benchmark_script`] | `autoresearch.sh`      |

use std::io;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

// ── MetricDirection ──────────────────────────────────────────────────────────

/// Whether a *lower* or *higher* metric value counts as "better".
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MetricDirection {
    /// Smaller values are better (e.g. latency, error rate).
    Lower,
    /// Larger values are better (e.g. throughput, accuracy).
    Higher,
}

// ── SessionConfig ────────────────────────────────────────────────────────────

/// Persistent configuration for an autoresearch experiment.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionConfig {
    /// Human-readable experiment name (used in commit messages and the
    /// markdown header).
    pub name: String,
    /// Name of the metric being optimised (must match the `METRIC <name>=…`
    /// lines emitted by the benchmark script).
    pub metric: String,
    /// Display unit for the metric (e.g. `"ms"`, `"ops/s"`).
    pub unit: String,
    /// Whether lower or higher values are considered improvements.
    pub direction: MetricDirection,
    /// Optional hard cap on the total number of experiment iterations.
    /// `None` means run until explicitly stopped.
    pub max_iterations: Option<u32>,
    /// Working directory for git operations.  Falls back to the session
    /// `root` directory when `None`.
    pub working_dir: Option<PathBuf>,
}

// ── Session ──────────────────────────────────────────────────────────────────

/// A handle to a single autoresearch experiment on disk.
///
/// Provides path helpers and serialisation/deserialisation of the
/// [`SessionConfig`].  The actual JSONL log lives in a separate [`JsonlLog`]
/// (see [`crate::autoresearch::log`]).
pub struct Session {
    pub config: SessionConfig,
    pub root: PathBuf,
}

impl Session {
    /// Create a new, in-memory session.  The config is **not** written to disk
    /// yet; call [`Session::save_config`] or [`InitExperimentTool`] to persist.
    pub fn new(root: impl Into<PathBuf>, config: SessionConfig) -> Self {
        Self {
            root: root.into(),
            config,
        }
    }

    /// Load a session that was previously persisted via [`Session::save_config`].
    ///
    /// Reads `<root>/autoresearch.config.json` and deserialises the
    /// [`SessionConfig`].
    pub fn load(root: impl AsRef<Path>) -> io::Result<Self> {
        let root = root.as_ref().to_path_buf();
        let config_path = root.join("autoresearch.config.json");
        let raw = std::fs::read_to_string(&config_path)?;
        let config: SessionConfig = serde_json::from_str(&raw)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
        Ok(Self { root, config })
    }

    /// Persist the [`SessionConfig`] to `<root>/autoresearch.config.json`.
    pub fn save_config(&self) -> io::Result<()> {
        let json = serde_json::to_string_pretty(&self.config)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
        std::fs::write(self.config_path(), json)
    }

    /// Write (or overwrite) the markdown summary header at [`Session::md_path`].
    ///
    /// The file contains a brief description of the experiment configuration
    /// and is intended to give the agent easy contextual access to the session
    /// state.
    pub fn save_md(&self) -> io::Result<()> {
        let direction_label = match self.config.direction {
            MetricDirection::Lower => "lower is better",
            MetricDirection::Higher => "higher is better",
        };
        let max_iter = match self.config.max_iterations {
            Some(n) => format!("{}", n),
            None => "unlimited".to_string(),
        };
        let working_dir = match &self.config.working_dir {
            Some(p) => p.display().to_string(),
            None => self.root.display().to_string(),
        };

        let md = format!(
            "# autoresearch: {name}\n\n\
             | Field | Value |\n\
             |-------|-------|\n\
             | Metric | `{metric}` ({unit}) |\n\
             | Direction | {direction} |\n\
             | Max iterations | {max_iter} |\n\
             | Working dir | `{working_dir}` |\n\
             | Config | `autoresearch.config.json` |\n\
             | Log | `autoresearch.jsonl` |\n\
             | Benchmark | `autoresearch.sh` |\n\
             | Checks | `autoresearch.checks.sh` |\n\
             \n\
             _Generated by pi-autoresearch — do not edit manually._\n",
            name = self.config.name,
            metric = self.config.metric,
            unit = self.config.unit,
            direction = direction_label,
            max_iter = max_iter,
            working_dir = working_dir,
        );
        std::fs::write(self.md_path(), md)
    }

    // ── Path helpers ─────────────────────────────────────────────────────────

    /// `<root>/autoresearch.config.json`
    pub fn config_path(&self) -> PathBuf {
        self.root.join("autoresearch.config.json")
    }

    /// `<root>/autoresearch.jsonl`
    pub fn jsonl_path(&self) -> PathBuf {
        self.root.join("autoresearch.jsonl")
    }

    /// `<root>/autoresearch.md`
    pub fn md_path(&self) -> PathBuf {
        self.root.join("autoresearch.md")
    }

    /// `<root>/autoresearch.checks.sh`
    pub fn checks_script(&self) -> PathBuf {
        self.root.join("autoresearch.checks.sh")
    }

    /// `<root>/autoresearch.sh`
    pub fn benchmark_script(&self) -> PathBuf {
        self.root.join("autoresearch.sh")
    }
}
