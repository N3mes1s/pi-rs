use std::path::Path;
use std::time::Duration;

use anyhow::{Context, Result};
use chrono::Utc;
use regex::Regex;
use serde::{Deserialize, Serialize};
use thiserror::Error;
use tracing::warn;

use crate::halo::{backlog, config::Config, state};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProposalDraft {
    pub title: String,
    pub priority: f64,
    pub est_cost: f64,
    pub files: Vec<String>,
}

#[derive(Debug, Error)]
pub enum ProposerError {
    #[error("proposer exhausted after {attempt_count} attempt(s): {error_kind}")]
    Exhausted { attempt_count: u32, error_kind: String },
}

fn parse_proposal_bullet(line: &str) -> Option<ProposalDraft> {
    let re = Regex::new(r"^[-*]\s+(.+?)\s+\(priority:\s*([0-9.]+),\s*est_cost:\s*\$([0-9.]+),\s*files:\s*([^)]*)\)\s*$").ok()?;
    let cap = re.captures(line.trim())?;
    let priority = cap.get(2)?.as_str().parse::<f64>().ok()?;
    let est_cost = cap.get(3)?.as_str().parse::<f64>().ok()?;
    if !(0.0..=1.0).contains(&priority) {
        return None;
    }
    let files = cap.get(4).map(|m| {
        m.as_str().split(',').map(|s| s.trim()).filter(|s| !s.is_empty()).map(str::to_string).collect::<Vec<_>>()
    }).unwrap_or_default();
    Some(ProposalDraft { title: cap.get(1)?.as_str().trim().to_string(), priority, est_cost, files })
}

pub fn parse_proposals(response: &str, max: u32) -> Vec<ProposalDraft> {
    let section = response.split_once("## Proposals").map(|(_, tail)| tail).unwrap_or(response);
    let mut out = Vec::new();
    for line in section.lines() {
        if let Some(p) = parse_proposal_bullet(line) {
            out.push(p);
            if out.len() as u32 >= max { break; }
        } else if line.trim_start().starts_with('-') || line.trim_start().starts_with('*') {
            warn!("proposer: dropping malformed bullet: {line}");
        }
    }
    out
}

fn read_last_run_ts(halo_dir: &Path) -> Option<u64> {
    std::fs::read_to_string(halo_dir.join("proposer_last_run")).ok()?.trim().parse().ok()
}

fn write_last_run_ts(halo_dir: &Path) {
    let _ = std::fs::write(halo_dir.join("proposer_last_run"), Utc::now().timestamp().to_string());
}

pub fn generate_proposal_id() -> String {
    format!("op-{}-{}", Utc::now().format("%Y%m%d-%H%M%S"), &uuid::Uuid::new_v4().to_string()[..8])
}

pub fn run_proposer_if_due(
    repo_root: &Path,
    halo_dir: &Path,
    backlog_jsonl: &Path,
    state_jsonl: &Path,
    cfg: &Config,
    cycle: u64,
    pending_count: usize,
) -> std::result::Result<Option<Vec<ProposalDraft>>, ProposerError> {
    if pending_count >= cfg.proposer.refill_threshold as usize { return Ok(None); }
    if let Some(last) = read_last_run_ts(halo_dir) {
        let elapsed = Utc::now().timestamp().saturating_sub(last as i64) as u64;
        if elapsed < cfg.proposer.min_seconds_between_proposer_runs { return Ok(None); }
    }
    let retries = cfg.proposer.max_retries.max(1);
    let mut last_error = String::from("unknown");
    for attempt in 1..=retries {
        match run_proposer(repo_root, cfg) {
            Ok(proposals) => {
                write_last_run_ts(halo_dir);
                for p in &proposals {
                    let _ = backlog::append_proposal_created(backlog_jsonl, &generate_proposal_id(), &p.title, "", &p.files, p.priority, p.est_cost, "halo-proposer");
                }
                return Ok(Some(proposals));
            }
            Err(e) => {
                last_error = e.to_string();
                if attempt < retries { std::thread::sleep(Duration::from_secs(1 << (attempt - 1))); }
            }
        }
    }
    let _ = state::append_step(state_jsonl, cycle, "pick_proposal", "STEP_PROPOSER_FAILED", serde_json::json!({"error_kind": last_error}));
    Err(ProposerError::Exhausted { attempt_count: retries, error_kind: last_error })
}

pub fn run_proposer(repo_root: &Path, cfg: &Config) -> Result<Vec<ProposalDraft>> {
    let _ = repo_root;
    let agent_path = Path::new(".pi").join("agents").join("halo-proposer.md");
    let response = std::fs::read_to_string(&agent_path).with_context(|| format!("read proposer agent {}", agent_path.display()))?;
    Ok(parse_proposals(&response, cfg.proposer.proposals_per_refill))
}

pub fn budget_exceeded(cfg: &Config, today_spend: f64) -> bool {
    let cycle_estimated_cost = (cfg.orchestrate.per_cycle_overspend_threshold_usd + cfg.proposer.estimated_cost_usd_per_call).max(0.50);
    today_spend + cycle_estimated_cost >= cfg.guardrails.daily_spend_budget_usd
}
