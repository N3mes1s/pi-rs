//! Rollback watchdog (RFD 0013).
//!
//! After [`super::apply::commit`] swaps AGENTS.md, the daemon records
//! an `apply` row in `history.jsonl`. [`tick`] is invoked periodically
//! by the orchestrator with the most recent N outcome means; if the
//! observed mean has dropped below `pre_apply_mean - min_margin`, the
//! prior body (stored inline on the apply row) is restored and a
//! `rollback` row is appended.

use chrono::Utc;
use std::fs;
use std::io;
use std::path::Path;

use super::apply::{append_history, read_history, HistoryEntry};

/// Result of one rollback poll.
#[derive(Debug, Clone, PartialEq)]
pub enum RollbackOutcome {
    /// No `apply` row in history; nothing to watch.
    NoPendingApply,
    /// Fewer than `min_new_outcomes` post-apply samples seen; keep waiting.
    InsufficientSamples { have: usize, need: usize },
    /// Rolling mean held above `pre_mean - min_margin`; no rollback.
    Held { observed_mean: f32 },
    /// Rolling mean dropped; AGENTS.md restored, rollback row appended.
    RolledBack { observed_mean: f32 },
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
    if n == 0 { f32::NAN } else { (sum / n as f64) as f32 }
}

/// Latest `apply` row that has not yet been undone by a `rollback` row
/// (matched on `to_hash`).
fn latest_pending_apply(history: &[HistoryEntry]) -> Option<HistoryEntry> {
    let mut pending: Option<HistoryEntry> = None;
    for e in history {
        match e.action.as_str() {
            "apply" => pending = Some(e.clone()),
            "rollback" => {
                if let Some(p) = &pending {
                    if e.from_hash == p.to_hash {
                        pending = None;
                    }
                }
            }
            _ => {}
        }
    }
    pending
}

/// Walk the last `min_new_outcomes` Outcome means in `recent_outcomes`.
/// If the mean dropped below `pre_apply_mean - min_margin`, restore
/// the previous AGENTS.md body (from the apply row) and append a
/// `rollback` row.
pub fn tick(
    agents_md_path: &Path,
    history_path: &Path,
    recent_outcomes: &[f32],
    min_new_outcomes: usize,
    min_margin: f32,
) -> io::Result<RollbackOutcome> {
    let history = read_history(history_path);
    let Some(pending) = latest_pending_apply(&history) else {
        return Ok(RollbackOutcome::NoPendingApply);
    };
    if recent_outcomes.len() < min_new_outcomes {
        return Ok(RollbackOutcome::InsufficientSamples {
            have: recent_outcomes.len(),
            need: min_new_outcomes,
        });
    }
    let window = &recent_outcomes[recent_outcomes.len() - min_new_outcomes..];
    let observed = finite_mean(window);
    let pre = pending.pre_mean.unwrap_or(f32::NAN);
    if !observed.is_finite() || !pre.is_finite() {
        return Ok(RollbackOutcome::Held { observed_mean: observed });
    }
    if observed >= pre - min_margin {
        return Ok(RollbackOutcome::Held { observed_mean: observed });
    }

    // Restore previous body inline from the apply row.
    let prev_body = pending.prev_body.clone().unwrap_or_default();
    if let Some(parent) = agents_md_path.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(agents_md_path, &prev_body)?;

    let entry = HistoryEntry {
        ts: Utc::now().to_rfc3339(),
        action: "rollback".to_string(),
        from_hash: pending.to_hash.clone(),
        to_hash: pending.from_hash.clone(),
        pre_mean: pending.pre_mean,
        post_mean_estimate: pending.post_mean_estimate,
        margin: Some(min_margin),
        observed_mean: Some(observed),
        trigger: Some(format!(
            "rolling {min_new_outcomes}-session mean dropped"
        )),
        prev_body: None,
    };
    append_history(history_path, &entry)?;
    Ok(RollbackOutcome::RolledBack { observed_mean: observed })
}
