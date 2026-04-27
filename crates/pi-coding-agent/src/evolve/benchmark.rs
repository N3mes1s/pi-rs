//! Benchmark replay harness (G7).
//!
//! Pulls outcome-labelled past sessions for the current cwd, extracts the
//! user's original prompt as a benchmark case, and replays each case
//! against a candidate AGENTS.md by spawning `pi --print`. Scores the
//! resulting session with the trajectory judge and aggregates.
//!
//! The actual subprocess invocation is behind a [`Replay`] trait so the
//! evolution daemon (G8) can plug in `SubprocessReplay` while tests can
//! pass a deterministic `MockReplay`.
//!
//! Synthetic flag: every Outcome entry produced by replay is tagged with
//! `OutcomeSource::Replay` (already wired in [`pi_agent_core::OutcomeSource`])
//! so future benchmark loads exclude them — prevents the loop from
//! self-reinforcing on its own outputs.

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::time::Duration;

use pi_agent_core::{OutcomeSource, SessionEntry, SessionEntryKind};
use pi_ai::Message;

/// One past session distilled into a benchmark target.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BenchmarkCase {
    pub session_id: String,
    /// First user message text — the original task. Replay uses this as
    /// the prompt to the candidate AGENTS.md.
    pub user_prompt: String,
    /// Historical verdict, if any, recorded for this session.
    pub historical_success: Option<bool>,
    pub historical_score: Option<f32>,
    /// Source path of the trajectory file (for traceability).
    pub trajectory_path: PathBuf,
}

/// Result of replaying one case against a candidate AGENTS.md.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RolloutResult {
    pub session_id: String,
    pub success: bool,
    pub score: f32,
    pub tokens_in: u64,
    pub tokens_out: u64,
    pub cost_usd: f32,
    pub duration_ms: u64,
    /// Free-form note from the judge or fallback (e.g. "tests passed",
    /// "agent looped on src/x.rs").
    pub notes: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BenchmarkSummary {
    pub n_cases: usize,
    pub pass_rate: f32,
    pub mean_score: f32,
    pub mean_tokens_in: f32,
    pub mean_tokens_out: f32,
    pub p95_tokens_in: u64,
    pub total_cost_usd: f32,
    pub mean_duration_ms: f32,
}

#[derive(Debug, thiserror::Error)]
pub enum BenchmarkError {
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("replay backend: {0}")]
    Replay(String),
    #[error("no benchmark cases available")]
    NoCases,
}

/// Load up to `max_cases` benchmark cases from the on-disk session
/// directory for the cwd-slug. Filters:
///
/// - Must have at least one User entry (extract as prompt).
/// - Must have an Outcome entry whose source is NOT `Replay` (don't
///   benchmark against synthetic data).
/// - Sorted most-recent first.
///
/// `sessions_root` is typically `~/.pi/agent/sessions`.
/// `cwd_slug` is `SessionManager::cwd_slug(cwd)` — the per-cwd subdir.
pub fn load_cases(
    sessions_root: &Path,
    cwd_slug: &str,
    max_cases: usize,
) -> Result<Vec<BenchmarkCase>, BenchmarkError> {
    let dir = sessions_root.join(cwd_slug);
    let Ok(read) = std::fs::read_dir(&dir) else {
        return Ok(Vec::new());
    };

    let mut cases: Vec<(i64, BenchmarkCase)> = Vec::new();
    for ent in read.flatten() {
        let path = ent.path();
        if path.extension().and_then(|s| s.to_str()) != Some("jsonl") {
            continue;
        }
        let Ok(txt) = std::fs::read_to_string(&path) else {
            continue;
        };
        let entries: Vec<SessionEntry> = txt
            .lines()
            .filter(|l| !l.trim().is_empty())
            .filter_map(|l| serde_json::from_str(l).ok())
            .collect();
        let Some(case) = case_from_entries(&path, &entries) else {
            continue;
        };
        let last_ts = entries.last().map(|e| e.timestamp).unwrap_or(0);
        cases.push((last_ts, case));
    }

    cases.sort_by(|a, b| b.0.cmp(&a.0));
    Ok(cases.into_iter().take(max_cases).map(|(_, c)| c).collect())
}

fn case_from_entries(path: &Path, entries: &[SessionEntry]) -> Option<BenchmarkCase> {
    let mut user_prompt: Option<String> = None;
    let mut last_outcome: Option<(bool, Option<f32>, OutcomeSource)> = None;

    for e in entries {
        match &e.kind {
            SessionEntryKind::User { message } if user_prompt.is_none() => {
                user_prompt = Some(message.text());
            }
            SessionEntryKind::Outcome {
                success,
                source,
                score,
                ..
            } => {
                last_outcome = Some((*success, *score, *source));
            }
            _ => {}
        }
    }

    let prompt = user_prompt?;
    let (success, score, source) = last_outcome?;
    if matches!(source, OutcomeSource::Replay) {
        return None; // never benchmark against synthetic outcomes
    }
    let session_id = path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("unknown")
        .to_string();
    Some(BenchmarkCase {
        session_id,
        user_prompt: prompt,
        historical_success: Some(success),
        historical_score: score,
        trajectory_path: path.to_path_buf(),
    })
}

/// Replay backend. The default subprocess impl spawns
/// `pi --print --no-session` with the candidate AGENTS.md staged in a
/// fresh tempdir as cwd; tests use a mock.
#[async_trait]
pub trait Replay: Send + Sync {
    async fn run(
        &self,
        case: &BenchmarkCase,
        agents_md_text: &str,
    ) -> Result<RolloutResult, BenchmarkError>;
}

/// Run all cases against `agents_md_text` and return per-case results.
/// Sequential by default — caller can parallelise with futures::join_all
/// if budget allows. (We keep it sequential here so cost-cap enforcement
/// in the daemon (G8) is straightforward.)
pub async fn run_all<R: Replay>(
    replay: &R,
    cases: &[BenchmarkCase],
    agents_md_text: &str,
) -> Result<Vec<RolloutResult>, BenchmarkError> {
    if cases.is_empty() {
        return Err(BenchmarkError::NoCases);
    }
    let mut out = Vec::with_capacity(cases.len());
    for case in cases {
        out.push(replay.run(case, agents_md_text).await?);
    }
    Ok(out)
}

/// Aggregate rollout results into a benchmark summary.
pub fn summarize(results: &[RolloutResult]) -> BenchmarkSummary {
    let n = results.len();
    if n == 0 {
        return BenchmarkSummary {
            n_cases: 0,
            pass_rate: 0.0,
            mean_score: 0.0,
            mean_tokens_in: 0.0,
            mean_tokens_out: 0.0,
            p95_tokens_in: 0,
            total_cost_usd: 0.0,
            mean_duration_ms: 0.0,
        };
    }
    let nf = n as f32;
    let pass_rate = results.iter().filter(|r| r.success).count() as f32 / nf;
    let mean_score = results.iter().map(|r| r.score).sum::<f32>() / nf;
    let mean_tokens_in = results.iter().map(|r| r.tokens_in as f32).sum::<f32>() / nf;
    let mean_tokens_out = results.iter().map(|r| r.tokens_out as f32).sum::<f32>() / nf;
    let mut tok_in: Vec<u64> = results.iter().map(|r| r.tokens_in).collect();
    tok_in.sort_unstable();
    let p95_idx = ((n as f32 * 0.95).ceil() as usize).saturating_sub(1).min(n - 1);
    let p95_tokens_in = tok_in[p95_idx];
    let total_cost_usd = results.iter().map(|r| r.cost_usd).sum();
    let mean_duration_ms =
        results.iter().map(|r| r.duration_ms as f32).sum::<f32>() / nf;
    BenchmarkSummary {
        n_cases: n,
        pass_rate,
        mean_score,
        mean_tokens_in,
        mean_tokens_out,
        p95_tokens_in,
        total_cost_usd,
        mean_duration_ms,
    }
}

// ─── default subprocess Replay (used by the evolve daemon, G8) ─────────

/// Default subprocess-based Replay. Spawns
/// `<pi_binary> --print --no-session --no-context-files -p <prompt>`
/// in a tempdir whose only AGENTS.md is the candidate. Captures the
/// resulting session id (from --json mode would be cleaner; print mode
/// returns the final reply, which is what we score with the judge).
///
/// Currently a stub: the daemon (G8) will wire the actual subprocess
/// invocation once mode wiring lands and we know the session-id capture
/// path. The trait shape is locked here so tests + benchmark-summary
/// math can land independently.
pub struct SubprocessReplay {
    pub pi_binary: PathBuf,
    pub timeout: Duration,
}

#[async_trait]
impl Replay for SubprocessReplay {
    async fn run(
        &self,
        _case: &BenchmarkCase,
        _agents_md_text: &str,
    ) -> Result<RolloutResult, BenchmarkError> {
        Err(BenchmarkError::Replay(
            "SubprocessReplay not yet wired (pending G8 mode-wiring)".into(),
        ))
    }
}

// ─── synthesise a User message from a prompt string ────────────────────

/// Helper used by the daemon (G8) when re-feeding a benchmark prompt
/// through pi as if it came from the user.
pub fn user_message(text: &str) -> Message {
    Message::user_text(text)
}
