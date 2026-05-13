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
    // v0.27 fix (canary bug #16): the M3 implementation read the
    // bundled halo-proposer.md as static-bullet content and re-emitted
    // its hardcoded exemplars on every call. With cooldown=60s this
    // produced an infinite "burst" of duplicate proposals and zero
    // diverse work. The proper fix is to invoke the bundled agent's
    // system prompt against a real LLM, with the repo's AGENTS.md +
    // recent commit log as context, and parse `## Proposals` bullets
    // out of the LLM's actual response.
    //
    // Implementation: shell out to `pi -p --no-tools` (read-only,
    // single-shot), pass the agent file content + repo context + a
    // diversity-aware ask as the user message. The LLM generates
    // fresh proposals with the format the bullet parser expects.
    //
    // Falls back to the M3 static-file-read on any subprocess failure
    // so the supervisor keeps making forward progress even if the
    // proposer subprocess is broken (e.g. no LLM credentials).
    let agent_path = repo_root.join(".pi").join("agents").join("halo-proposer.md");
    let agent_md = std::fs::read_to_string(&agent_path)
        .with_context(|| format!("read proposer agent {}", agent_path.display()))?;
    let context = build_repo_context(repo_root);
    let ask = format!(
        "You are the halo proposer subagent. Below is your system prompt (the bundled \
         halo-proposer.md), followed by repo context (AGENTS.md head + recent commit log).\n\n\
         Your task: emit a markdown response with a `## Proposals` heading followed by \
         exactly {n} bullet items. Each bullet MUST end in `(priority: <0..1>, est_cost: \
         $<float>, files: <comma-separated paths>)`. AVOID re-proposing work that already \
         appears in the recent commit log — that's already done.\n\n\
         === BUNDLED halo-proposer.md ===\n{agent}\n\n\
         === Repo context ===\n{ctx}",
        n = cfg.proposer.proposals_per_refill,
        agent = agent_md.trim(),
        ctx = context,
    );
    let pi_bin = std::env::current_exe()
        .with_context(|| "locating pi binary for proposer subprocess")?;
    let output = std::process::Command::new(&pi_bin)
        .args(["-p", "--no-tools"])
        .arg(&ask)
        .current_dir(repo_root)
        .output();
    match output {
        Ok(o) if o.status.success() => {
            let response = String::from_utf8_lossy(&o.stdout).to_string();
            let parsed = parse_proposals(&response, cfg.proposer.proposals_per_refill);
            if parsed.is_empty() {
                warn!(
                    response_len = response.len(),
                    "proposer LLM returned no parseable bullets; falling back to bundled file"
                );
                return Ok(parse_proposals(&agent_md, cfg.proposer.proposals_per_refill));
            }
            Ok(parsed)
        }
        Ok(o) => {
            warn!(
                exit_code = ?o.status.code(),
                stderr = %String::from_utf8_lossy(&o.stderr).chars().take(200).collect::<String>(),
                "proposer subprocess exited non-zero; falling back to bundled file"
            );
            Ok(parse_proposals(&agent_md, cfg.proposer.proposals_per_refill))
        }
        Err(e) => {
            warn!(
                error = %e,
                "proposer subprocess spawn failed; falling back to bundled file"
            );
            Ok(parse_proposals(&agent_md, cfg.proposer.proposals_per_refill))
        }
    }
}

/// Build a compact repo-context string for the proposer's user message:
/// AGENTS.md head + last 20 git commits + a brief workspace summary.
fn build_repo_context(repo_root: &Path) -> String {
    let mut buf = String::with_capacity(4096);
    if let Ok(agents_md) = std::fs::read_to_string(repo_root.join("AGENTS.md")) {
        buf.push_str("--- AGENTS.md (first 4 KiB) ---\n");
        let head = if agents_md.len() > 4096 { &agents_md[..4096] } else { agents_md.as_str() };
        buf.push_str(head);
        buf.push_str("\n\n");
    }
    let log = std::process::Command::new("git")
        .args(["-C", repo_root.to_string_lossy().as_ref(), "log", "--oneline", "-20"])
        .output();
    if let Ok(o) = log {
        if o.status.success() {
            buf.push_str("--- git log --oneline -20 ---\n");
            buf.push_str(&String::from_utf8_lossy(&o.stdout));
            buf.push('\n');
        }
    }
    buf
}

pub fn budget_exceeded(cfg: &Config, today_spend: f64) -> bool {
    let cycle_estimated_cost = (cfg.orchestrate.per_cycle_overspend_threshold_usd + cfg.proposer.estimated_cost_usd_per_call).max(0.50);
    today_spend + cycle_estimated_cost >= cfg.guardrails.daily_spend_budget_usd
}
