//! halo — M1 helpers: config, bundled-agent bootstrap, status surface,
//! and halo-owned-clone precondition validator.

pub mod config;

use std::path::{Path, PathBuf};
use anyhow::{anyhow, Result};
use chrono::Utc;

// ── Bundled-agent bootstrap ───────────────────────────────────────────────────

use include_dir::{include_dir, Dir};

// Bundle the agents directory into the binary via `include_dir!` as required
// by the RFD. The macro expands to a `Dir` which we can query for files.
static AGENTS_DIR: Dir<'_> = include_dir!("$CARGO_MANIFEST_DIR/agents");

/// Write bundled agent files to `<repo>/.pi/agents/` if they don't exist yet.
/// Uses `include_dir!` to find all files embedded in the `agents/` directory.
/// Operator-managed files are left untouched.
pub fn bootstrap_bundled_agents(repo_root: &Path) -> Result<Vec<PathBuf>> {
    let dir = repo_root.join(".pi").join("agents");
    std::fs::create_dir_all(&dir)?;
    let mut written = Vec::new();
    for f in AGENTS_DIR.files() {
        let name = f.path().file_name()
            .and_then(|n| n.to_str())
            .ok_or_else(|| anyhow!("bundled agent has no file name"))?;
        let dest = dir.join(name);
        if dest.exists() { continue; }
        std::fs::write(&dest, f.contents())?;
        written.push(dest);
    }
    Ok(written)
}

// ── JSONL parsing helpers ─────────────────────────────────────────────────────

#[derive(serde::Deserialize, Default, Clone)]
#[serde(default)]
struct StateEvent {
    kind:   String,
    ts:     Option<String>,
    cycle:  Option<u64>,
    step:   Option<String>,
    meta:   Option<String>,
    detail: Option<serde_json::Value>,
}

#[derive(serde::Deserialize, Default, Clone)]
#[serde(default)]
struct BacklogEvent {
    kind:   String,
    id:     Option<String>,
    status: Option<String>,
}

#[derive(serde::Deserialize, Default, Clone)]
#[serde(default)]
struct UsageRow {
    cost_usd:   Option<f64>,
    ts:         Option<String>,
    supersedes: Option<serde_json::Value>,
}

fn parse_jsonl<T: serde::de::DeserializeOwned>(path: &Path) -> Vec<T> {
    let Ok(text) = std::fs::read_to_string(path) else { return vec![]; };
    text.lines()
        .filter(|l| !l.trim().is_empty())
        .filter_map(|l| serde_json::from_str(l).ok())
        .collect()
}

// ── Status snapshot ───────────────────────────────────────────────────────────

/// Full status snapshot that mirrors the §Status surface in RFD 0025.
#[derive(Debug, Default, serde::Serialize)]
pub struct HaloStatusSnapshot {
    pub repo: String,
    pub halo_dir: String,
    /// Canonical state string: e.g. "SUPERVISOR_NOT_RUNNING", "RUNNING",
    /// "CYCLE_47 (step: STEP_ORCHESTRATE)", "PAUSED", etc.
    pub state: String,
    pub name: String,
    pub last_cycle: Option<u64>,
    pub last_cycle_outcome: Option<String>,
    /// Last step in an active cycle.
    pub active_step: Option<String>,
    pub backlog_pending:    u32,
    pub backlog_dispatched: u32,
    pub backlog_merged:     u32,
    pub backlog_failed:     u32,
    pub backlog_dropped:    u32,
    pub spend_today_usd: f64,
    pub failed_streak:   u32,
    pub paused:  bool,
    pub pid: Option<String>,
    /// Commit-rate (merged commits) in the trailing 60m window.
    pub commit_rate_60m: u32,
    /// Commit-rate cap (from halo.toml guardrails).
    pub commits_per_hour_cap: u32,
    /// Cycles executed since UTC midnight.
    pub cycles_today: u32,
    /// Cycles-per-day cap from config.
    pub cycles_per_day_cap: u32,
    /// Daily budget cap from config.
    pub daily_budget_cap_usd: f64,
    /// Failed-build streak cap from config.
    pub failed_build_streak_cap: u32,
    /// Last N (up to 5) completed cycles: (cycle_n, outcome, cost, ts, title).
    pub last_cycles: Vec<CycleSummary>,
}

#[derive(Debug, Default, serde::Serialize)]
pub struct CycleSummary {
    pub cycle:   u64,
    pub outcome: String,
    pub cost:    f64,
    pub ts:      String,
    pub title:   String,
}

fn halo_dir_for(cwd: &Path) -> Option<PathBuf> {
    let home = dirs::home_dir()?;
    let slug = cwd.canonicalize().unwrap_or_else(|_| cwd.to_path_buf())
        .display().to_string()
        .replace(['/', '\\', ':'], "_");
    Some(home.join(".pi").join("halo").join(slug))
}

fn today_spend(rows: &[UsageRow]) -> f64 {
    let midnight = Utc::now().date_naive().and_hms_opt(0, 0, 0).unwrap().and_utc();
    let mut total = 0.0_f64;
    for row in rows {
        if row.supersedes.as_ref().map_or(false, |v| !v.is_null()) { continue; }
        if let Some(ts) = &row.ts {
            if let Ok(t) = chrono::DateTime::parse_from_rfc3339(ts) {
                if t.with_timezone(&Utc) < midnight { continue; }
            }
        }
        total += row.cost_usd.unwrap_or(0.0);
    }
    total
}

/// Reconstruct failed-build streak from STREAK_* meta events.
fn replay_streak(events: &[StateEvent]) -> u32 {
    let mut streak = 0u32;
    // Find the last STREAK_RESET, then count STREAK_INCREMENTED after it.
    let reset_pos = events.iter().enumerate().rev()
        .find(|(_, e)| e.kind == "meta" && e.meta.as_deref() == Some("STREAK_RESET"))
        .map(|(i, _)| i)
        .unwrap_or(0);
    for e in &events[reset_pos..] {
        if e.kind == "meta" {
            match e.meta.as_deref() {
                Some("STREAK_INCREMENTED") => streak += 1,
                Some("STREAK_RESET")       => streak = 0,
                _ => {}
            }
        }
    }
    streak
}

/// Build last-5-cycles list from state events.
fn last_five_cycles(events: &[StateEvent]) -> Vec<CycleSummary> {
    let mut cycles: Vec<CycleSummary> = Vec::new();
    for e in events {
        if e.kind == "meta" && e.meta.as_deref() == Some("CYCLE_DONE") {
            if let Some(det) = &e.detail {
                let cycle = det.get("cycle").or_else(|| e.cycle.as_ref().map(|_| det))
                    .and_then(|_| e.cycle)
                    .or_else(|| det.get("cycle").and_then(|v| v.as_u64()))
                    .unwrap_or(0);
                let outcome = det.get("outcome").and_then(|v| v.as_str())
                    .unwrap_or("?").to_string();
                let ts = e.ts.clone().unwrap_or_default();
                cycles.push(CycleSummary {
                    cycle,
                    outcome,
                    cost: 0.0, // per-cycle cost requires usage rows keyed by cycle; M1 stub
                    ts,
                    title: String::new(),
                });
            }
        }
    }
    // Return last 5, newest first.
    cycles.reverse();
    cycles.truncate(5);
    cycles
}

/// Attempt to load `halo.toml` from `<cwd>/.pi/halo.toml` or the path provided.
fn try_load_config(cwd: &Path, override_path: Option<&Path>) -> Option<config::Config> {
    let path = if let Some(p) = override_path {
        p.to_path_buf()
    } else {
        cwd.join(".pi").join("halo.toml")
    };
    std::fs::read_to_string(&path).ok()
        .and_then(|s| config::parse(&s).ok())
}

/// Count how many CYCLE_DONE events landed since UTC midnight.
fn cycles_today(events: &[StateEvent]) -> u32 {
    let midnight = Utc::now().date_naive().and_hms_opt(0, 0, 0).unwrap().and_utc();
    events.iter().filter(|e| {
        e.kind == "meta" && e.meta.as_deref() == Some("CYCLE_DONE")
            && e.ts.as_deref().and_then(|ts| chrono::DateTime::parse_from_rfc3339(ts).ok())
                .map_or(false, |t| t.with_timezone(&Utc) >= midnight)
    }).count() as u32
}

/// Count merged commits on the configured target_branch in the trailing 60m window.
/// This is a best-effort count derived from CYCLE_DONE { outcome:"applied" } events.
fn commit_rate_60m(events: &[StateEvent]) -> u32 {
    let cutoff = Utc::now() - chrono::Duration::minutes(60);
    events.iter().filter(|e| {
        if e.kind != "meta" || e.meta.as_deref() != Some("CYCLE_DONE") { return false; }
        let outcome_ok = e.detail.as_ref().and_then(|d| d.get("outcome"))
            .and_then(|v| v.as_str()) == Some("applied");
        let recent = e.ts.as_deref()
            .and_then(|ts| chrono::DateTime::parse_from_rfc3339(ts).ok())
            .map_or(false, |t| t.with_timezone(&Utc) >= cutoff);
        outcome_ok && recent
    }).count() as u32
}

/// Build a snapshot from the three halo log files for the repo containing `cwd`.
/// Optionally takes a config override path for `--halo-config`.
pub fn snapshot_with_config(cwd: &Path, config_path: Option<&Path>) -> Result<HaloStatusSnapshot> {
    let repo = cwd.canonicalize().unwrap_or_else(|_| cwd.to_path_buf())
        .display().to_string();
    let halo_dir = halo_dir_for(cwd).ok_or_else(|| anyhow!("no home dir"))?;

    let state_p   = halo_dir.join("state.jsonl");
    let backlog_p = halo_dir.join("backlog.jsonl");
    let usage_p   = halo_dir.join("usage.jsonl");
    let paused_p  = halo_dir.join("paused");
    let pid_p     = halo_dir.join("pid");

    let state_events:   Vec<StateEvent>   = parse_jsonl(&state_p);
    let backlog_events: Vec<BacklogEvent> = parse_jsonl(&backlog_p);
    let usage_rows:     Vec<UsageRow>     = parse_jsonl(&usage_p);

    let paused = paused_p.exists();
    let pid = std::fs::read_to_string(&pid_p).ok()
        .and_then(|s| s.lines().next().map(str::to_string))
        .filter(|s| !s.is_empty());

    // Try to load halo.toml for config values.
    let cfg = try_load_config(cwd, config_path);
    let (name, daily_budget_cap_usd, commits_per_hour_cap, cycles_per_day_cap,
         failed_build_streak_max) = cfg.as_ref().map_or(
        ("".into(), 10.0, 4u32, 24u32, 2u32),
        |c| (c.name.clone(),
             c.guardrails.daily_spend_budget_usd,
             c.guardrails.commits_per_hour_max,
             c.guardrails.cycles_per_day_max,
             c.guardrails.failed_build_streak_max)
    );

    // Derive state string and cycle info from events + flag files.
    let (state, last_cycle, last_cycle_outcome, active_step) =
        derive_state(&state_events, paused, &pid_p);

    // failed_build_streak_max pulled from config earlier
    let failed_build_streak_max = cfg.as_ref().map(|c| c.guardrails.failed_build_streak_max).unwrap_or(2u32);

    // Backlog counts.
    let mut proposal_status: std::collections::HashMap<String, String> = Default::default();
    for ev in &backlog_events {
        let Some(id) = &ev.id else { continue };
        if ev.kind == "proposal_created" {
            proposal_status.entry(id.clone()).or_insert_with(|| "pending".into());
        } else if ev.kind == "proposal_status_changed" {
            if let Some(s) = &ev.status {
                proposal_status.insert(id.clone(), s.clone());
            }
        } else if ev.kind == "proposal_dropped" {
            proposal_status.insert(id.clone(), "dropped".into());
        }
    }
    let (mut pending, mut dispatched, mut merged, mut failed, mut dropped) = (0u32,0u32,0u32,0u32,0u32);
    for s in proposal_status.values() {
        match s.as_str() {
            "pending"                          => pending    += 1,
            "dispatched"                       => dispatched += 1,
            "merged"                           => merged     += 1,
            "failed"|"blocked"|"rolled_back"   => failed     += 1,
            "dropped"                          => dropped    += 1,
            _ => {}
        }
    }

    let failed_streak    = replay_streak(&state_events);
    let spend_today      = today_spend(&usage_rows);
    let last_cycles      = last_five_cycles(&state_events);
    let commit_rate      = commit_rate_60m(&state_events);
    let cycles_today_n   = cycles_today(&state_events);

    Ok(HaloStatusSnapshot {
        repo,
        halo_dir: halo_dir.display().to_string(),
        state,
        name,
        last_cycle,
        last_cycle_outcome,
        active_step,
        backlog_pending: pending,
        backlog_dispatched: dispatched,
        backlog_merged: merged,
        backlog_failed: failed,
        backlog_dropped: dropped,
        spend_today_usd: spend_today,
        failed_streak,
        paused,
        pid,
        commit_rate_60m: commit_rate,
        commits_per_hour_cap,
        cycles_today: cycles_today_n,
        cycles_per_day_cap,
        daily_budget_cap_usd,
        failed_build_streak_cap: failed_build_streak_max,
        last_cycles,
    })
}

/// Build a snapshot from the three halo log files for the repo containing `cwd`.
pub fn snapshot(cwd: &Path) -> Result<HaloStatusSnapshot> {
    snapshot_with_config(cwd, None)
}

fn derive_state(
    events: &[StateEvent],
    paused: bool,
    pid_file: &Path,
) -> (String, Option<u64>, Option<String>, Option<String>) {
    if events.is_empty() && !paused {
        return ("SUPERVISOR_NOT_RUNNING".into(), None, None, None);
    }
    if paused && !pid_file.exists() {
        return ("PAUSED".into(), None, None, None);
    }

    let mut last_cycle:   Option<u64>    = None;
    let mut last_outcome: Option<String> = None;
    let mut active_step:  Option<String> = None;

    for e in events {
        match e.kind.as_str() {
            "meta" => {
                match e.meta.as_deref() {
                    Some("CYCLE_DONE") | Some("CYCLE_ABORTED") => {
                        if let Some(c) = e.cycle { last_cycle = Some(c); }
                        if let Some(det) = &e.detail {
                            if let None = last_cycle {
                                last_cycle = det.get("cycle").and_then(|v| v.as_u64());
                            }
                            last_outcome = det.get("outcome")
                                .or_else(|| det.get("reason"))
                                .and_then(|v| v.as_str())
                                .map(str::to_string);
                        }
                        active_step = None; // cycle terminal clears active step
                    }
                    _ => {}
                }
            }
            "step" => {
                if let Some(s) = &e.step { active_step = Some(s.clone()); }
                if let Some(c) = e.cycle { last_cycle = Some(c); }
            }
            _ => {}
        }
    }

    if pid_file.exists() {
        let state = match (last_cycle, &active_step) {
            (Some(c), Some(s)) => format!("CYCLE_{c} (step: {s})"),
            (Some(c), None)    => format!("RUNNING (cycle {c})"),
            _                  => "RUNNING".into(),
        };
        return (state, last_cycle, last_outcome, active_step);
    }

    let state = match last_cycle {
        Some(c) => format!("SUPERVISOR_NOT_RUNNING (last cycle: {c})"),
        None    => "SUPERVISOR_NOT_RUNNING".into(),
    };
    (state, last_cycle, last_outcome, active_step)
}

/// Render a status snapshot in the §Status surface layout from RFD 0025.
pub fn render_snapshot_human(s: &HaloStatusSnapshot) {
    // Header: "halo  <name>  cycle <n>"
    let header_name = if s.name.is_empty() { s.repo.as_str() } else { s.name.as_str() };
    if let Some(c) = s.last_cycle {
        println!("halo  {}  cycle {c}", header_name);
    } else {
        println!("halo  {}", header_name);
    }
    // State line includes active campaign/milestone when running
    if let (Some(c), Some(step)) = (s.last_cycle, &s.active_step) {
        println!("state: {}  (campaign halo-cycle-{c}, step: {})", s.state, step);
    } else {
        println!("state: {}", s.state);
    }
    // Spend / cycles-today line (uses config values from snapshot)
    println!(
        "spend today: ${:.2} / ${:.2}       cycles today: {} / {}",
        s.spend_today_usd, s.daily_budget_cap_usd,
        s.cycles_today, s.cycles_per_day_cap
    );
    // Commit-rate line (derived from applied CYCLE_DONE events in last 60m)
    println!("commit-rate (60m): {} / {}", s.commit_rate_60m, s.commits_per_hour_cap);
    // Failed-build streak
    println!(
        "failed-build streak: {} / {}",
        s.failed_streak, /* max cap from config or default 2 */
        if s.failed_streak > 0 { s.failed_streak.max(2) } else { 2 }
    );
    // Backlog summary
    println!(
        "backlog: {} pending, {} dispatched, {} merged, {} failed, {} dropped",
        s.backlog_pending, s.backlog_dispatched, s.backlog_merged,
        s.backlog_failed,  s.backlog_dropped
    );
    // Last 5 cycles table
    if !s.last_cycles.is_empty() {
        println!("last {} cycle(s):", s.last_cycles.len());
        for c in &s.last_cycles {
            println!(
                "  {:>3}  {:<12}  ${:.2}  {}",
                c.cycle, c.outcome, c.cost, c.ts
            );
        }
    }
    if s.paused {
        println!("paused: yes (run `pi --halo-resume` to clear)");
    }
    if let Some(pid) = &s.pid {
        println!("pid: {pid}");
    }
}

/// Thin wrapper that returns a single-line state string (used by tests).
pub fn render_status(repo_root: &Path) -> Result<String> {
    let snap = snapshot(repo_root)?;
    Ok(format!("state: {}", snap.state))
}

// ── Halo-owned-clone precondition validator ───────────────────────────────────

/// Verify halo-owned-clone preconditions (RFD 0025 §Halo-owned clone precondition).
///
/// Returns `Ok(())` iff:
/// 1. `clone.expected_root` is set and the repo path matches the glob.
/// 2. `git status --porcelain` is empty (clean working tree).
/// 3. Local `target_branch` exists (`git rev-parse --verify`).
/// 4. `<repo_root>/AGENTS.md` exists.
pub fn check_halo_clone_preconditions(repo_root: &Path, cfg: &config::Config) -> Result<()> {
    let expected = cfg.clone_config.expected_root.as_ref()
        .ok_or_else(|| anyhow!("clone.expected_root not set"))?;
    if expected.trim().is_empty() {
        return Err(anyhow!("clone.expected_root not set"));
    }
    let expanded = shellexpand::tilde(expected).to_string();
    let pat = glob::Pattern::new(&expanded)
        .map_err(|e| anyhow!("invalid expected_root pattern: {}", e))?;
    let repo_s = repo_root.canonicalize()?.display().to_string();
    if !pat.matches(&repo_s) {
        return Err(anyhow!(
            "clone path '{}' does not match expected_root '{}'",
            repo_s, expected
        ));
    }

    let out = std::process::Command::new("git")
        .args(["-C", &repo_s, "status", "--porcelain"])
        .output()
        .map_err(|e| anyhow!("git status failed: {}", e))?;
    if !out.status.success() {
        return Err(anyhow!("git status failed: {}", String::from_utf8_lossy(&out.stderr)));
    }
    if !out.stdout.is_empty() {
        return Err(anyhow!("git working tree is not clean (git status --porcelain non-empty)"));
    }

    let rev = std::process::Command::new("git")
        .args(["-C", &repo_s, "rev-parse", "--verify", &cfg.target_branch])
        .output()
        .map_err(|e| anyhow!("git rev-parse failed: {}", e))?;
    if !rev.status.success() {
        return Err(anyhow!("target branch '{}' not found locally", cfg.target_branch));
    }

    if !repo_root.join("AGENTS.md").is_file() {
        return Err(anyhow!(
            "repo-local AGENTS.md not found at {}",
            repo_root.join("AGENTS.md").display()
        ));
    }

    Ok(())
}
