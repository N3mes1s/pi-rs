//! Pareto frontier + apply / rollback for evolution candidates (G9).
//!
//! Composition:
//! 1. After each tick, the daemon has a baseline summary + N candidate
//!    summaries. [`pareto_frontier`] picks the non-dominated subset.
//! 2. [`best_strict_improvement`] picks ONE candidate that strictly
//!    improves on the baseline (pass rate doesn't regress). If found,
//!    [`backup_and_apply`] writes the candidate to AGENTS.md after
//!    archiving the previous file.
//! 3. After apply, the recorder watches the next K outcome-labelled
//!    sessions. [`should_rollback`] compares that window's pass rate
//!    to the baseline. On regression, [`rollback`] restores the prior
//!    AGENTS.md and marks the offending hash poisoned so the daemon
//!    won't try it again.
//!
//! Storage layout (continuation of `tick`):
//!
//! ```text
//! <cwd>/.pi/evolve/
//!   generations.jsonl    # append-only ledger of (hash, summary, applied?, rolled_back?)
//!   poisoned.txt         # one hash per line, never re-apply
//!   history/<ts>-<hash>.md   # backup of replaced AGENTS.md
//!   pending_apply.json   # state for the rollback monitor
//! ```

use chrono::Utc;
use serde::{Deserialize, Serialize};
use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use super::benchmark::BenchmarkSummary;
use super::tick::evolve_dir;

// ─── Margin gate (RFD 0013) ────────────────────────────────────────────

/// Outcome of comparing a candidate's per-rollout score vector against
/// the current AGENTS.md's score vector. Used by the orchestrator to
/// decide whether to atomically swap AGENTS.md.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ApplyDecision {
    pub apply: bool,
    pub reason: String,
    pub current_mean: f32,
    pub candidate_mean: f32,
    pub margin: f32,
}

fn finite_mean(xs: &[f32]) -> f32 {
    let mut sum = 0.0_f64;
    let mut n = 0_u64;
    for &x in xs {
        if x.is_finite() {
            sum += x as f64;
            n += 1;
        }
    }
    if n == 0 {
        f32::NAN
    } else {
        (sum / n as f64) as f32
    }
}

/// Pure margin gate: candidate must beat current by at least `min_margin`.
///
/// Empty / all-NaN inputs produce a `NaN` mean and refuse the apply
/// (so an evolve tick that produced no scores can never silently swap).
pub fn decide(current: &[f32], candidate: &[f32], min_margin: f32) -> ApplyDecision {
    let cur = finite_mean(current);
    let cand = finite_mean(candidate);
    let margin = cand - cur;
    let apply = cur.is_finite() && cand.is_finite() && margin >= min_margin;
    let reason = if !cur.is_finite() {
        "current mean is undefined (no finite samples); declined".to_string()
    } else if !cand.is_finite() {
        "candidate mean is undefined (no finite samples); declined".to_string()
    } else if apply {
        format!("candidate mean {cand:.3} ≥ current {cur:.3} + margin {min_margin}")
    } else {
        format!("candidate mean {cand:.3} < current {cur:.3} + margin {min_margin}; declined")
    };
    ApplyDecision {
        apply,
        reason,
        current_mean: cur,
        candidate_mean: cand,
        margin,
    }
}

// ─── history.jsonl atomic apply (RFD 0013) ─────────────────────────────

/// One row in `~/.pi/agent/evolve/history.jsonl`. Captures both apply
/// and rollback events so the rollback watchdog can reconstruct the
/// previous AGENTS.md body.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct HistoryEntry {
    pub ts: String,
    pub action: String,
    pub from_hash: String,
    pub to_hash: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pre_mean: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub post_mean_estimate: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub margin: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub observed_mean: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trigger: Option<String>,
    /// Inline copy of the AGENTS.md body that was replaced. Lets the
    /// rollback watchdog restore without a separate backup file.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prev_body: Option<String>,
}

/// Append one JSON line to `history_path`, creating parents as needed.
pub fn append_history(history_path: &Path, entry: &HistoryEntry) -> io::Result<()> {
    if let Some(parent) = history_path.parent() {
        fs::create_dir_all(parent)?;
    }
    let mut f = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(history_path)?;
    let line = serde_json::to_string(entry)
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
    f.write_all(line.as_bytes())?;
    f.write_all(b"\n")?;
    Ok(())
}

/// Read the entire history file (best effort). Malformed lines are
/// skipped silently.
pub fn read_history(history_path: &Path) -> Vec<HistoryEntry> {
    let Ok(txt) = fs::read_to_string(history_path) else {
        return Vec::new();
    };
    txt.lines()
        .filter(|l| !l.trim().is_empty())
        .filter_map(|l| serde_json::from_str(l).ok())
        .collect()
}

fn body_hash(body: &str) -> String {
    use sha2::{Digest, Sha256};
    let mut h = Sha256::new();
    h.update(body.as_bytes());
    format!("{:x}", h.finalize())
}

/// Atomically swap `agents_md_path`'s contents to `new_body` and append
/// a matching `apply` entry to `history_path`. Order:
///
/// 1. Write `new_body` to a sibling tempfile.
/// 2. Append history entry (containing the previous body for rollback).
/// 3. `fs::rename` tempfile → `agents_md_path` (atomic on the same FS).
///
/// If any step fails, AGENTS.md is left unchanged.
pub fn commit(
    agents_md_path: &Path,
    new_body: &str,
    history_path: &Path,
    decision: &ApplyDecision,
) -> io::Result<HistoryEntry> {
    let prev_body = if agents_md_path.exists() {
        fs::read_to_string(agents_md_path)?
    } else {
        String::new()
    };
    let from_hash = body_hash(&prev_body);
    let to_hash = body_hash(new_body);

    let parent = agents_md_path.parent().ok_or_else(|| {
        io::Error::new(io::ErrorKind::InvalidInput, "AGENTS.md path has no parent")
    })?;
    fs::create_dir_all(parent)?;
    let tmp = parent.join(format!(
        ".AGENTS.md.evolve.tmp.{}",
        std::process::id()
    ));
    // Step 1: stage new body next to the destination.
    {
        let mut f = fs::File::create(&tmp)?;
        f.write_all(new_body.as_bytes())?;
        f.sync_all()?;
    }

    let entry = HistoryEntry {
        ts: Utc::now().to_rfc3339(),
        action: "apply".to_string(),
        from_hash: from_hash.clone(),
        to_hash: to_hash.clone(),
        pre_mean: Some(decision.current_mean),
        post_mean_estimate: Some(decision.candidate_mean),
        margin: Some(decision.margin),
        observed_mean: None,
        trigger: None,
        prev_body: Some(prev_body),
    };

    // Step 2: log the intent before swapping. If this fails, drop tempfile.
    if let Err(e) = append_history(history_path, &entry) {
        let _ = fs::remove_file(&tmp);
        return Err(e);
    }

    // Step 3: atomic swap.
    if let Err(e) = fs::rename(&tmp, agents_md_path) {
        let _ = fs::remove_file(&tmp);
        return Err(e);
    }

    Ok(entry)
}

/// Default location of the cross-cwd evolve history.jsonl
/// (under `pi_coding_agent::context::agent_dir()`).
pub fn default_history_path() -> PathBuf {
    crate::context::agent_dir().join("evolve").join("history.jsonl")
}

// ─── Pareto frontier ───────────────────────────────────────────────────

/// One candidate in the population. The summary is what `pareto_frontier`
/// inspects; the hash + body let the daemon write the winner to disk.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Candidate {
    pub hash: String,
    pub summary: BenchmarkSummary,
    /// Full rendered AGENTS.md for this candidate.
    pub body: String,
    /// Section index that was mutated (None for the baseline).
    pub mutated_section: Option<usize>,
    /// Reason from the mutator (or "baseline" / "regression-rollback").
    pub note: String,
}

/// Return the indices of candidates that are not strictly dominated by
/// any other candidate. Axes (and direction):
///
/// - pass_rate (higher better)
/// - mean_score (higher better)
/// - p95_tokens_in (lower better)
/// - total_cost_usd (lower better)
///
/// "Strictly dominated" means another candidate is *not worse* on every
/// axis AND *strictly better* on at least one.
pub fn pareto_frontier(candidates: &[Candidate]) -> Vec<usize> {
    let n = candidates.len();
    let mut frontier = Vec::with_capacity(n);
    for i in 0..n {
        let dominated = (0..n).any(|j| i != j && dominates(&candidates[j], &candidates[i]));
        if !dominated {
            frontier.push(i);
        }
    }
    frontier
}

/// `a` strictly dominates `b` when `a` is at least as good on every
/// axis and strictly better on at least one.
fn dominates(a: &Candidate, b: &Candidate) -> bool {
    let a_pr = a.summary.pass_rate;
    let b_pr = b.summary.pass_rate;
    let a_ms = a.summary.mean_score;
    let b_ms = b.summary.mean_score;
    let a_tk = a.summary.p95_tokens_in;
    let b_tk = b.summary.p95_tokens_in;
    let a_co = a.summary.total_cost_usd;
    let b_co = b.summary.total_cost_usd;

    let no_worse = a_pr >= b_pr && a_ms >= b_ms && a_tk <= b_tk && a_co <= b_co;
    let strictly_better = a_pr > b_pr || a_ms > b_ms || a_tk < b_tk || a_co < b_co;
    no_worse && strictly_better
}

/// Pick the single candidate that strictly improves on the baseline
/// without regressing pass rate. Returns its index or `None`.
///
/// Constraints (in order):
/// 1. Skip the baseline itself.
/// 2. pass_rate >= baseline.pass_rate (never accept regression).
/// 3. Must be on the Pareto frontier.
/// 4. Among qualifiers, prefer the one with highest pass_rate; tie-break
///    on lower p95_tokens; tie-break on lower cost.
pub fn best_strict_improvement(
    candidates: &[Candidate],
    baseline_idx: usize,
) -> Option<usize> {
    if candidates.is_empty() || baseline_idx >= candidates.len() {
        return None;
    }
    let baseline = &candidates[baseline_idx];
    let frontier = pareto_frontier(candidates);

    let mut best: Option<usize> = None;
    for &i in &frontier {
        if i == baseline_idx {
            continue;
        }
        let c = &candidates[i];
        if c.summary.pass_rate < baseline.summary.pass_rate {
            continue;
        }
        // Must dominate or be incomparable-but-improving baseline.
        // We've already filtered to non-dominated; require at least one
        // strictly-better axis vs baseline.
        if !is_improvement(&c.summary, &baseline.summary) {
            continue;
        }

        match best {
            None => best = Some(i),
            Some(bi) => {
                let bsum = &candidates[bi].summary;
                let csum = &c.summary;
                let better = (csum.pass_rate, -(csum.p95_tokens_in as i64), -csum.total_cost_usd as i32)
                    > (bsum.pass_rate, -(bsum.p95_tokens_in as i64), -bsum.total_cost_usd as i32);
                if better {
                    best = Some(i);
                }
            }
        }
    }
    best
}

fn is_improvement(c: &BenchmarkSummary, b: &BenchmarkSummary) -> bool {
    c.pass_rate > b.pass_rate
        || c.mean_score > b.mean_score
        || c.p95_tokens_in < b.p95_tokens_in
        || c.total_cost_usd < b.total_cost_usd
}

// ─── Apply / Backup ────────────────────────────────────────────────────

/// Backup the current AGENTS.md to `history/<ts>-<old-hash>.md` and
/// write `new_body` in its place. Returns the backup path.
pub fn backup_and_apply(
    cwd: &Path,
    agents_md_path: &Path,
    new_body: &str,
    old_hash: &str,
) -> io::Result<PathBuf> {
    let dir = evolve_dir(cwd)?;
    let history = dir.join("history");
    fs::create_dir_all(&history)?;

    let ts = Utc::now().format("%Y%m%dT%H%M%S").to_string();
    let backup = history.join(format!("{ts}-{}.md", short(old_hash, 12)));

    if agents_md_path.exists() {
        let current = fs::read(agents_md_path)?;
        fs::write(&backup, current)?;
    } else {
        // No prior AGENTS.md to back up — write a marker so rollback
        // knows the original state was "no file".
        fs::write(&backup, "")?;
    }
    fs::write(agents_md_path, new_body)?;
    Ok(backup)
}

/// Restore an AGENTS.md from a backup file. Used by the rollback path.
/// If the backup is empty (the special "there was no prior file"
/// marker), the AGENTS.md is removed instead.
pub fn rollback(agents_md_path: &Path, backup: &Path) -> io::Result<()> {
    let body = fs::read(backup)?;
    if body.is_empty() {
        if agents_md_path.exists() {
            fs::remove_file(agents_md_path)?;
        }
    } else {
        fs::write(agents_md_path, body)?;
    }
    Ok(())
}

// ─── Pending-apply marker (rollback monitor) ──────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PendingApply {
    pub applied_hash: String,
    pub previous_hash: String,
    pub backup_path: PathBuf,
    pub baseline_pass_rate: f32,
    pub applied_at_ms: i64,
    /// Sessions completed at the time of apply — used by the monitor
    /// to know which sessions are post-apply.
    pub outcomes_seen_at_apply: u32,
}

impl PendingApply {
    pub fn save(&self, cwd: &Path) -> io::Result<()> {
        let path = evolve_dir(cwd)?.join("pending_apply.json");
        let txt = serde_json::to_string_pretty(self)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
        fs::write(path, txt)
    }

    pub fn load(cwd: &Path) -> Option<Self> {
        let path = evolve_dir(cwd).ok()?.join("pending_apply.json");
        let txt = fs::read_to_string(path).ok()?;
        serde_json::from_str(&txt).ok()
    }

    pub fn clear(cwd: &Path) -> io::Result<()> {
        let path = evolve_dir(cwd)?.join("pending_apply.json");
        if path.exists() {
            fs::remove_file(path)?;
        }
        Ok(())
    }
}

/// Compute whether the post-apply pass rate has regressed enough to
/// trigger rollback. Pure: caller supplies the post-apply pass rate
/// and the monitor window size.
///
/// Triggers rollback iff:
/// - At least `min_window_size` post-apply outcome-labelled sessions
///   have been seen, AND
/// - The post-apply pass rate is below `baseline - regression_threshold`.
pub fn should_rollback(
    baseline_pass_rate: f32,
    post_apply_pass_rate: f32,
    post_apply_count: u32,
    min_window_size: u32,
    regression_threshold: f32,
) -> bool {
    if post_apply_count < min_window_size {
        return false;
    }
    post_apply_pass_rate < (baseline_pass_rate - regression_threshold)
}

// ─── Poison list ───────────────────────────────────────────────────────

/// Once a candidate triggers a rollback, its hash is added here so the
/// daemon never re-applies it. The list lives in a plain text file so
/// users can inspect / edit it manually.
pub fn poisoned_hashes(cwd: &Path) -> Vec<String> {
    let Ok(dir) = evolve_dir(cwd) else {
        return Vec::new();
    };
    let path = dir.join("poisoned.txt");
    let Ok(txt) = fs::read_to_string(path) else {
        return Vec::new();
    };
    txt.lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .map(str::to_string)
        .collect()
}

pub fn add_poison(cwd: &Path, hash: &str) -> io::Result<()> {
    let path = evolve_dir(cwd)?.join("poisoned.txt");
    let mut f = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)?;
    writeln!(f, "{hash}")?;
    Ok(())
}

pub fn is_poisoned(cwd: &Path, hash: &str) -> bool {
    poisoned_hashes(cwd).iter().any(|h| h == hash)
}

// ─── Generations log (append-only) ────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GenerationLogEntry {
    pub timestamp_ms: i64,
    pub hash: String,
    pub parent_hash: Option<String>,
    pub mutated_section: Option<usize>,
    pub summary: BenchmarkSummary,
    pub applied: bool,
    pub note: String,
}

pub fn append_generation(cwd: &Path, entry: &GenerationLogEntry) -> io::Result<()> {
    let path = evolve_dir(cwd)?.join("generations.jsonl");
    let mut f = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)?;
    let line = serde_json::to_string(entry)
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
    f.write_all(line.as_bytes())?;
    f.write_all(b"\n")?;
    Ok(())
}

pub fn read_generations(cwd: &Path) -> Vec<GenerationLogEntry> {
    let Ok(dir) = evolve_dir(cwd) else {
        return Vec::new();
    };
    let path = dir.join("generations.jsonl");
    let Ok(txt) = fs::read_to_string(path) else {
        return Vec::new();
    };
    txt.lines()
        .filter(|l| !l.trim().is_empty())
        .filter_map(|l| serde_json::from_str(l).ok())
        .collect()
}

fn short(s: &str, n: usize) -> String {
    s.chars().take(n).collect()
}
