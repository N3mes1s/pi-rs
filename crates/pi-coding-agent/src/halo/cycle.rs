//! Single-cycle executor — RFD 0025 §Implementation plan / M2.
//!
//! `run_cycle` drives one halo cycle through the canonical eight steps:
//! 1. pick_proposal
//! 2. synthesise_campaign
//! 3. prep_branch
//! 4. orchestrate
//! 5. keep_marker_scan
//! 6. smoke
//! 7. rollback_if_regress
//! 8. evolve_tick

use anyhow::{bail, Context, Result};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::io::Write as _;
use std::sync::atomic::{AtomicBool, AtomicI32, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::halo::{backlog, proposer, spend, state};

/// Outcome of a single halo cycle.
#[derive(Debug, Clone, PartialEq)]
pub enum CycleOutcome {
    /// Cycle completed normally. `outcome` matches CYCLE_DONE detail.outcome.
    Done { outcome: String },
    /// Cycle was aborted (signal, guardrail, mid-step hard failure).
    Aborted { reason: String },
}

/// Per-cycle mutable context shared among steps.
pub struct CycleCtx<'a> {
    pub repo_root: &'a Path,
    pub halo_dir: &'a Path,
    pub state_jsonl: PathBuf,
    pub backlog_jsonl: PathBuf,
    pub usage_jsonl: PathBuf,
    pub cycle: u64,
    pub target_branch: String,
    pub slug: String,
    /// Signal received (SIGINT/SIGTERM) — set by the run.rs handler.
    pub signal_received: Arc<AtomicBool>,
    /// Per-signal flags for event reporting (set by run.rs handler).
    pub sigint_received: Arc<AtomicBool>,
    pub sigterm_received: Arc<AtomicBool>,
    /// Shared atomic holding the orchestrate child PID so the signal
    /// handler in run.rs can kill the PG.
    pub orchestrate_pid_shared: Option<Arc<AtomicI32>>,
    /// Pre-orchestrate target_branch HEAD SHA (captured in prep_branch).
    pub pre_target_head: Option<String>,
    /// Post-orchestrate target_branch HEAD SHA (captured in postcheckout).
    pub post_target_head: Option<String>,
    /// Proposal id currently dispatched.
    pub proposal_id: Option<String>,
    /// Proposal title (captured at pick_proposal).
    pub proposal_title: Option<String>,
    /// Proposal rationale (captured at pick_proposal).
    pub proposal_rationale: Option<String>,
    /// Proposal files_touched (captured at pick_proposal).
    pub proposal_files: Vec<String>,
    /// Did orchestrate produce at least one MERGED milestone?
    pub orchestrate_merged: bool,
    /// Was there a keep-marker violation?
    pub keep_marker_violated: bool,
    /// Did smoke fail?
    pub smoke_failed: bool,
    /// Orchestrate exit code.
    pub orchestrate_exit: i32,
    /// Orchestrate start time (for spend estimation).
    pub orchestrate_start: Option<Instant>,
    /// Orchestrate elapsed seconds.
    pub orchestrate_elapsed_secs: f64,
    /// Config reference for refill + budget gating.
    pub cfg: crate::halo::config::Config,
    pub daily_budget_usd: f64,
    pub orchestrate_overspend_threshold: f64,
    pub refill_threshold: u32,
    pub budget_per_minute: f64,
    pub proposer_cost: f64,
    pub smoke_cmd: String,
    pub smoke_timeout: u64,
    pub interrupt_grace_secs: u64,
    /// Child process id (set during STEP_ORCHESTRATE).
    pub orchestrate_child_pid: Option<u32>,
    /// Evolve candidate cost for ledger.
    pub evolve_cost_usd: f64,
}

type StepResult = std::result::Result<(), StepError>;

#[derive(Debug)]
enum StepError {
    /// Soft failure: record FAILED event but continue to terminal.
    Failed(String),
    /// Hard abort: write CYCLE_ABORTED and bail.
    Aborted(String),
}

impl From<anyhow::Error> for StepError {
    fn from(e: anyhow::Error) -> Self {
        StepError::Failed(e.to_string())
    }
}

/// Entry point: run one full cycle with a default-built context.
///
/// Loads `<repo>/.pi/halo.toml`, derives the halo dir, and calls
/// [`run_cycle_with_ctx`].
pub fn run_cycle(repo_root: &Path, cycle_n: u64) -> Result<CycleOutcome> {
    let halo_dir = halo_dir_for_repo(repo_root)
        .ok_or_else(|| anyhow::anyhow!("could not derive halo dir"))?;
    std::fs::create_dir_all(&halo_dir).ok();

    // Load config (best-effort — fall through to defaults).
    let cfg_path = repo_root.join(".pi").join("halo.toml");
    let cfg = std::fs::read_to_string(&cfg_path)
        .ok()
        .and_then(|s| crate::halo::config::parse(&s).ok())
        .unwrap_or_else(default_config);

    let pid_shared = Arc::new(AtomicI32::new(0));
    let sigint_flag = Arc::new(AtomicBool::new(false));
    let sigterm_flag = Arc::new(AtomicBool::new(false));
    let ctx = build_ctx(
        repo_root,
        &halo_dir,
        cycle_n,
        &cfg,
        Arc::new(AtomicBool::new(false)),
        sigint_flag,
        sigterm_flag,
        pid_shared,
    );
    run_cycle_with_ctx(repo_root, cycle_n, ctx)
}

/// Build a default [`crate::halo::config::Config`] (used when no halo.toml
/// is present — primarily for tests and the recovery path).
pub fn default_config() -> crate::halo::config::Config {
    crate::halo::config::Config {
        name: "halo".into(),
        target_branch: "halo/auto-merge".into(),
        clone_config: Default::default(),
        guardrails: Default::default(),
        supervisor: Default::default(),
        smoke: Default::default(),
        proposer: Default::default(),
        cycle: Default::default(),
        orchestrate: Default::default(),
    }
}

/// Build a [`CycleCtx`] from a parsed config.
pub fn build_ctx<'a>(
    repo_root: &'a Path,
    halo_dir: &'a Path,
    cycle_n: u64,
    cfg: &crate::halo::config::Config,
    signal_received: Arc<AtomicBool>,
    sigint_received: Arc<AtomicBool>,
    sigterm_received: Arc<AtomicBool>,
    orchestrate_pid_shared: Arc<AtomicI32>,
) -> CycleCtx<'a> {
    let slug = format!("c{}", cycle_n);
    CycleCtx {
        repo_root,
        halo_dir,
        state_jsonl: halo_dir.join("state.jsonl"),
        backlog_jsonl: halo_dir.join("backlog.jsonl"),
        usage_jsonl: halo_dir.join("usage.jsonl"),
        cycle: cycle_n,
        target_branch: cfg.target_branch.clone(),
        slug,
        signal_received,
        sigint_received,
        sigterm_received,
        orchestrate_pid_shared: Some(orchestrate_pid_shared),
        pre_target_head: None,
        post_target_head: None,
        proposal_id: None,
        proposal_title: None,
        proposal_rationale: None,
        proposal_files: Vec::new(),
        orchestrate_merged: false,
        keep_marker_violated: false,
        smoke_failed: false,
        orchestrate_exit: 0,
        orchestrate_start: None,
        orchestrate_elapsed_secs: 0.0,
        cfg: cfg.clone(),
        daily_budget_usd: cfg.guardrails.daily_spend_budget_usd,
        orchestrate_overspend_threshold: cfg.orchestrate.per_cycle_overspend_threshold_usd,
        refill_threshold: cfg.proposer.refill_threshold,
        budget_per_minute: cfg.orchestrate.budget_dollars_per_minute_estimate,
        proposer_cost: cfg.proposer.estimated_cost_usd_per_call,
        smoke_cmd: cfg.smoke.cmd.clone(),
        smoke_timeout: cfg.smoke.timeout_seconds,
        interrupt_grace_secs: cfg.supervisor.interrupt_grace_seconds,
        orchestrate_child_pid: None,
        evolve_cost_usd: 0.0,
    }
}

/// Run a cycle with a pre-built [`CycleCtx`].
pub fn run_cycle_with_ctx(
    repo_root: &Path,
    cycle_n: u64,
    ctx_in: CycleCtx,
) -> Result<CycleOutcome> {
    let mut ctx = ctx_in;
    ctx.cycle = cycle_n;
    let _ = repo_root;

    // Append cycle start marker.
    state::append_meta(
        &ctx.state_jsonl,
        "CYCLE_STARTED",
        json!({"cycle": cycle_n}),
    )?;

    if let Err(e) = step_check_budget(&mut ctx) {
        return finish_aborted(&ctx, match e { StepError::Aborted(m) | StepError::Failed(m) => m });
    }


    // Check signal after each step.
    macro_rules! check_signal {
        () => {
            if ctx.signal_received.load(Ordering::SeqCst) {
                return finish_aborted(&ctx, "sigint".into());
            }
        };
    }

    check_signal!();

    // 1. pick_proposal
    if let Err(e) = step_pick_proposal(&mut ctx) {
        match e {
            StepError::Aborted(m) => return finish_aborted(&ctx, m),
            StepError::Failed(_) => {
                state::cycle_done(&ctx.state_jsonl, cycle_n, "failed")?;
                return Ok(CycleOutcome::Done { outcome: "failed".into() });
            }
        }
    }
    check_signal!();

    // 2. synthesise_campaign
    if let Err(e) = step_synthesise_campaign(&mut ctx) {
        match e {
            StepError::Aborted(m) => return finish_aborted(&ctx, m),
            StepError::Failed(_) => {
                state::cycle_done(&ctx.state_jsonl, cycle_n, "failed")?;
                return Ok(CycleOutcome::Done { outcome: "failed".into() });
            }
        }
    }
    check_signal!();

    // 3. prep_branch
    if let Err(e) = step_prep_branch(&mut ctx) {
        match e {
            StepError::Aborted(m) => return finish_aborted(&ctx, m),
            StepError::Failed(_) => {
                // prep_branch failure → proposal becomes blocked + paused
                append_proposal_status(
                    &ctx.backlog_jsonl,
                    ctx.proposal_id.as_deref().unwrap_or(""),
                    "blocked",
                    json!({"reason": "prep_branch_failed"}),
                )?;
                state::cycle_aborted(
                    &ctx.state_jsonl,
                    cycle_n,
                    json!({"reason": "paused", "subreason": "prep_branch_failed"}),
                )?;
                write_paused(&ctx.halo_dir)?;
                return Ok(CycleOutcome::Aborted { reason: "prep_branch_failed".into() });
            }
        }
    }
    check_signal!();

    // 4. orchestrate
    let orch_result = step_orchestrate(&mut ctx);
    // Write orchestrate spend row regardless.
    let elapsed_mins = ctx.orchestrate_elapsed_secs / 60.0;
    let _ = spend::write_orchestrate_row(
        &ctx.usage_jsonl,
        cycle_n,
        elapsed_mins,
        ctx.budget_per_minute,
    );

    // Post-orchestrate: unconditionally checkout target_branch.
    let post_result = step_orchestrate_postcheckout(&mut ctx);
    match post_result {
        Err(StepError::Aborted(_)) | Err(StepError::Failed(_)) => {
            // postcheckout failure → PAUSED
            state::cycle_aborted(
                &ctx.state_jsonl,
                cycle_n,
                json!({"reason": "paused", "subreason": "postcheckout_failed"}),
            )?;
            write_paused(&ctx.halo_dir)?;
            return Ok(CycleOutcome::Aborted { reason: "postcheckout_failed".into() });
        }
        Ok(()) => {}
    }

    if let Err(e) = orch_result {
        match e {
            StepError::Aborted(m) => return finish_aborted(&ctx, m),
            StepError::Failed(_) => {
                // orchestrate failed/blocked — still run keep_marker_scan
            }
        }
    }

    check_signal!();

    // 5. keep_marker_scan
    let marker_result = step_keep_marker_scan(&mut ctx);
    if marker_result.is_err() {
        ctx.keep_marker_violated = true;
    }

    check_signal!();

    // 6. smoke (skipped on keep_marker_violation)
    if !ctx.keep_marker_violated {
        let smoke_result = step_smoke(&mut ctx);
        if let Err(StepError::Failed(_)) = &smoke_result {
            ctx.smoke_failed = true;
        }
    } else {
        state::append_step(
            &ctx.state_jsonl,
            cycle_n,
            "smoke",
            "STEP_SMOKE_SKIPPED",
            json!({"reason": "keep_marker_violation"}),
        )?;
    }
    check_signal!();

    // 7. rollback_if_regress
    let rollback_result = step_rollback_if_regress(&mut ctx);
    let go_paused_after_rollback = matches!(
        &rollback_result,
        Err(StepError::Failed(_)) | Err(StepError::Aborted(_))
    );
    if go_paused_after_rollback {
        state::cycle_aborted(
            &ctx.state_jsonl,
            cycle_n,
            json!({"reason": "paused", "subreason": "revert_failed"}),
        )?;
        write_paused(&ctx.halo_dir)?;
        return Ok(CycleOutcome::Aborted { reason: "revert_failed".into() });
    }

    // If keep_marker_violated → also emit blocked proposal + CYCLE_ABORTED + paused.
    if ctx.keep_marker_violated {
        append_proposal_status(
            &ctx.backlog_jsonl,
            ctx.proposal_id.as_deref().unwrap_or(""),
            "blocked",
            json!({"reason": "keep_marker_violation"}),
        )?;
        state::cycle_aborted(
            &ctx.state_jsonl,
            cycle_n,
            json!({"reason": "paused", "subreason": "keep_marker_violation"}),
        )?;
        write_paused(&ctx.halo_dir)?;
        return Ok(CycleOutcome::Aborted { reason: "keep_marker_violation".into() });
    }

    check_signal!();

    // 8. evolve_tick (only when cycle completed without keep-marker or revert failure)
    let evolve_skipped = ctx.smoke_failed;
    if !evolve_skipped {
        let _ = step_evolve_tick(&mut ctx);
        // Write evolve spend row.
        if ctx.evolve_cost_usd > 0.0 {
            let _ = spend::write_evolve_tick_row(
                &ctx.usage_jsonl,
                cycle_n,
                ctx.evolve_cost_usd,
            );
        }
    } else {
        state::append_step(
            &ctx.state_jsonl,
            cycle_n,
            "evolve_tick",
            "STEP_EVOLVE_TICK_DONE",
            json!({"skipped_reason": "smoke_failed_or_paused"}),
        )?;
    }

    // Determine final outcome.
    let outcome = if ctx.orchestrate_exit == 3 {
        "blocked"
    } else if ctx.orchestrate_merged {
        if ctx.smoke_failed { "rolled_back" } else { "applied" }
    } else {
        "skipped"
    };

    // Update proposal status. v0.27 fix: only mark `merged` when
    // orchestrate actually merged (outcome=="applied"); a `skipped`
    // outcome means orchestrate ran but produced no merged commit
    // (merged_count==0), so the proposal was NOT applied — mark it
    // `failed` so cooldown-based retry kicks in. Prior versions
    // mapped `skipped` to `merged` which made the backlog status
    // diverge from branch truth (canary day-1 surfaced this as bug
    // #10: "1 cycle applied, 7 marked merged in backlog").
    if let Some(ref pid) = ctx.proposal_id {
        let status = match outcome {
            "applied" => "merged",
            "skipped" => "failed",
            "rolled_back" => "rolled_back",
            "blocked" => "blocked",
            _ => "failed",
        };
        append_proposal_status(&ctx.backlog_jsonl, pid, status, json!({}))?;
    }

    state::cycle_done(&ctx.state_jsonl, cycle_n, outcome)?;
    Ok(CycleOutcome::Done { outcome: outcome.into() })
}

// ── Step implementations ─────────────────────────────────────────────────────

#[allow(dead_code)]
fn step_tree_clean_check(ctx: &mut CycleCtx) -> StepResult {
    let out = std::process::Command::new("git")
        .args(["-C", &ctx.repo_root.display().to_string(), "status", "--porcelain"])
        .output()
        .map_err(|e| StepError::Failed(format!("git status failed: {e}")))?;
    if !out.stdout.is_empty() {
        state::append_step(
            &ctx.state_jsonl,
            ctx.cycle,
            "tree_clean_check",
            "STEP_TREE_DIRTY_REFUSED",
            json!({"detail": "working tree is not clean"}),
        )
        .ok();
        return Err(StepError::Aborted("dirty working tree".into()));
    }
    state::append_step(
        &ctx.state_jsonl,
        ctx.cycle,
        "tree_clean_check",
        "STEP_TREE_CLEAN_OK",
        json!({}),
    )
    .ok();
    Ok(())
}

fn step_check_budget(ctx: &mut CycleCtx) -> StepResult {
    let today = crate::halo::spend::today_spend(&ctx.usage_jsonl);
    if proposer::budget_exceeded(&ctx.cfg, today) {
        state::cycle_aborted(
            &ctx.state_jsonl,
            ctx.cycle,
            json!({"reason": "cost_cap"}),
        )?;
        return Err(StepError::Aborted("cost_cap".into()));
    }
    Ok(())
}

fn step_pick_proposal(ctx: &mut CycleCtx) -> StepResult {
    let mut backlog = crate::halo::backlog::replay(&ctx.backlog_jsonl);
    let pending = crate::halo::backlog::pending_proposals(&backlog).len();
    if pending < ctx.refill_threshold as usize {
        let _ = proposer::run_proposer_if_due(
            ctx.repo_root,
            ctx.halo_dir,
            &ctx.backlog_jsonl,
            &ctx.state_jsonl,
            &ctx.cfg,
            ctx.cycle,
            pending,
        );
        backlog = crate::halo::backlog::replay(&ctx.backlog_jsonl);
    }
    let pending = crate::halo::backlog::pending_proposals(&backlog);
    if pending.is_empty() {
        state::append_step(
            &ctx.state_jsonl,
            ctx.cycle,
            "pick_proposal",
            "NO_PROPOSAL_AVAILABLE",
            json!({}),
        )
        .ok();
        return Err(StepError::Failed("no proposal available".into()));
    }
    let proposal = pending[0];
    ctx.proposal_id = Some(proposal.id.clone());
    ctx.proposal_title = Some(proposal.title.clone());
    ctx.proposal_rationale = Some(proposal.rationale.clone());
    ctx.proposal_files = proposal.files.clone();
    append_proposal_status(
        &ctx.backlog_jsonl,
        &proposal.id,
        "dispatched",
        json!({"cycle": ctx.cycle}),
    )?;
    let _ = spend::write_proposer_row(&ctx.usage_jsonl, ctx.cycle, ctx.proposer_cost);
    state::append_step(
        &ctx.state_jsonl,
        ctx.cycle,
        "pick_proposal",
        "STEP_PICK_PROPOSAL_DONE",
        json!({"proposal_id": proposal.id}),
    )
    .ok();
    Ok(())
}

fn step_synthesise_campaign(ctx: &mut CycleCtx) -> StepResult {
    // Build a complete campaign.toml for the picked proposal — filled in
    // with implementer/reviewer agents from halo.toml plus the proposal's
    // title/rationale/files in the assignment.
    let proposal_id = ctx.proposal_id.as_deref().unwrap_or("unknown");
    let title = ctx.proposal_title.as_deref().unwrap_or("untitled");
    let rationale = ctx.proposal_rationale.as_deref().unwrap_or("");
    let campaign_name = format!("halo-cycle-{}", ctx.cycle);
    let branch = format!("halo/cycle-{}-{}", ctx.cycle, &ctx.slug);
    let campaign_path = ctx.halo_dir.join(format!("cycle-{}-campaign.toml", ctx.cycle));
    let reviewer = ctx.cfg.orchestrate.reviewer_agent.as_str();
    let files_block = if ctx.proposal_files.is_empty() {
        String::new()
    } else {
        format!(
            "\nFiles mentioned in the proposal:\n{}\n",
            ctx.proposal_files
                .iter()
                .map(|f| format!("- {f}"))
                .collect::<Vec<_>>()
                .join("\n"),
        )
    };
    // Escape triple-quotes in the assignment by replacing `"""` with `\"\"\"`.
    let assignment = format!(
        "{title}\n\n{rationale}\n{files_block}\nStay tightly scoped: this is one cycle of the halo autonomous loop. If the change is larger than estimated, leave the rest as a follow-up proposal.",
    )
    .replace("\"\"\"", "\\\"\\\"\\\"");

    let campaign_toml = format!(
        r#"name = "{campaign_name}"
description = "halo cycle {cycle} — {title} (proposal {proposal_id})"
target_branch = "{target_branch}"

[defaults]
reviewer = "{reviewer}"
fix_loop_max = 5

[[milestones]]
id = "{proposal_id}"
branch = "{branch}"
depends_on = []
implementer = "halo-implementer"
reviewer = "{reviewer}"
assignment = """
{assignment}
"""
"#,
        campaign_name = campaign_name,
        cycle = ctx.cycle,
        title = title,
        proposal_id = proposal_id,
        target_branch = ctx.target_branch,
        reviewer = reviewer,
        branch = branch,
        assignment = assignment,
    );

    std::fs::create_dir_all(ctx.halo_dir)
        .map_err(|e| StepError::Failed(format!("create halo_dir: {e}")))?;
    std::fs::write(&campaign_path, &campaign_toml)
        .map_err(|e| StepError::Failed(format!("write campaign.toml: {e}")))?;

    state::append_step(
        &ctx.state_jsonl,
        ctx.cycle,
        "synthesise_campaign",
        "STEP_SYNTHESISE_DONE",
        json!({"campaign_path": campaign_path.display().to_string()}),
    )
    .ok();
    Ok(())
}

fn step_prep_branch(ctx: &mut CycleCtx) -> StepResult {
    // Capture pre-orchestrate target_branch HEAD.
    let pre_sha = git_rev_parse(ctx.repo_root, &ctx.target_branch)
        .map_err(|e| StepError::Failed(format!("rev-parse target_branch: {e}")))?;
    ctx.pre_target_head = Some(pre_sha.clone());

    // Create/reset the per-cycle milestone branch from local target_branch.
    let branch_name = format!("halo/cycle-{}-{}", ctx.cycle, ctx.slug);
    let repo_s = ctx.repo_root.display().to_string();
    let out = std::process::Command::new("git")
        .args(["-C", &repo_s, "checkout", "-B", &branch_name, &ctx.target_branch])
        .output()
        .map_err(|e| StepError::Failed(format!("git checkout -B: {e}")))?;
    if !out.status.success() {
        return Err(StepError::Failed(format!(
            "git checkout -B {} failed: {}",
            branch_name,
            String::from_utf8_lossy(&out.stderr)
        )));
    }

    // Return to target_branch so orchestrate's own checkout is the one that
    // switches into the milestone branch.
    let back = std::process::Command::new("git")
        .args(["-C", &repo_s, "checkout", &ctx.target_branch])
        .output()
        .map_err(|e| StepError::Failed(format!("checkout target_branch: {e}")))?;
    if !back.status.success() {
        return Err(StepError::Failed(format!(
            "checkout {} after prep_branch failed: {}",
            ctx.target_branch,
            String::from_utf8_lossy(&back.stderr)
        )));
    }

    state::append_step(
        &ctx.state_jsonl,
        ctx.cycle,
        "prep_branch",
        "STEP_PREP_BRANCH_DONE",
        json!({
            "branch_name": branch_name,
            "base_sha": pre_sha,
            "pre_target_branch_head": pre_sha,
        }),
    )
    .ok();
    Ok(())
}

fn step_orchestrate(ctx: &mut CycleCtx) -> StepResult {
    let campaign_path = ctx
        .halo_dir
        .join(format!("cycle-{}-campaign.toml", ctx.cycle));

    ctx.orchestrate_start = Some(Instant::now());

    // Spawn orchestrate as a subprocess in its own process group.
    use std::os::unix::process::CommandExt;
    let mut cmd = std::process::Command::new(
        std::env::current_exe().unwrap_or_else(|_| PathBuf::from("pi")),
    );
    cmd.args(["--orchestrate", &campaign_path.display().to_string()])
        .env("PI_HALO_SUPPRESS_DETACHED_EVOLVE", "1")
        .env("GIT_DIR", ctx.repo_root.join(".git").display().to_string())
        .current_dir(ctx.repo_root)
        .stdin(std::process::Stdio::null());
    // Safety: process_group is safe to call before exec.
    cmd.process_group(0);

    let child = cmd
        .spawn()
        .map_err(|e| StepError::Failed(format!("spawn orchestrate: {e}")))?;
    let child_pid = child.id();
    ctx.orchestrate_child_pid = Some(child_pid);

    // Publish pid so the signal handler in run.rs can kill the PG.
    if let Some(ref shared) = ctx.orchestrate_pid_shared {
        shared.store(child_pid as i32, Ordering::SeqCst);
    }

    // Wait, checking for signals periodically.
    let output = wait_child(child, ctx.signal_received.clone(), Duration::from_millis(500));
    ctx.orchestrate_elapsed_secs = ctx
        .orchestrate_start
        .map(|s| s.elapsed().as_secs_f64())
        .unwrap_or(0.0);

    // Clear shared pid — orchestrate is gone.
    if let Some(ref shared) = ctx.orchestrate_pid_shared {
        shared.store(0, Ordering::SeqCst);
    }

    let exit_code = match output {
        Ok(status) => status.code().unwrap_or(-1),
        Err(_) => -1,
    };
    ctx.orchestrate_exit = exit_code;
    ctx.orchestrate_merged = exit_code == 0;

    state::append_step(
        &ctx.state_jsonl,
        ctx.cycle,
        "orchestrate",
        "STEP_ORCHESTRATE_DONE",
        json!({
            "exit_code": exit_code,
            "merged_count": if exit_code == 0 { 1 } else { 0 },
            "failed_count": if exit_code == 2 { 1 } else { 0 },
        }),
    )
    .ok();

    if ctx.signal_received.load(Ordering::SeqCst) {
        return Err(StepError::Aborted("sigint".into()));
    }

    if exit_code == 3 {
        // blocked — soft failure, not an error
        return Err(StepError::Failed("orchestrate exit 3: blocked".into()));
    }
    Ok(())
}

fn step_orchestrate_postcheckout(ctx: &mut CycleCtx) -> StepResult {
    // Unconditionally return to target_branch after orchestrate.
    let repo_s = ctx.repo_root.display().to_string();
    let out = std::process::Command::new("git")
        .args(["-C", &repo_s, "checkout", &ctx.target_branch])
        .output()
        .map_err(|e| StepError::Failed(format!("postcheckout: {e}")))?;
    if !out.status.success() {
        state::append_step(
            &ctx.state_jsonl,
            ctx.cycle,
            "orchestrate",
            "STEP_ORCHESTRATE_POSTCHECKOUT_FAILED",
            json!({"error": String::from_utf8_lossy(&out.stderr).to_string()}),
        )
        .ok();
        return Err(StepError::Failed("postcheckout failed".into()));
    }
    let post_sha = git_rev_parse(ctx.repo_root, &ctx.target_branch).unwrap_or_default();
    ctx.post_target_head = Some(post_sha.clone());
    state::append_step(
        &ctx.state_jsonl,
        ctx.cycle,
        "orchestrate",
        "STEP_ORCHESTRATE_POSTCHECKOUT_OK",
        json!({"post_target_branch_head": post_sha}),
    )
    .ok();
    Ok(())
}

fn step_keep_marker_scan(ctx: &mut CycleCtx) -> StepResult {
    let pre = match &ctx.pre_target_head {
        Some(s) => s.clone(),
        None => {
            state::append_step(
                &ctx.state_jsonl,
                ctx.cycle,
                "keep_marker_scan",
                "STEP_KEEP_MARKER_OK",
                json!({"detail": "no pre_sha"}),
            )
            .ok();
            return Ok(());
        }
    };
    let post = ctx.post_target_head.clone().unwrap_or_else(|| pre.clone());

    if pre == post {
        // No commits merged.
        state::append_step(
            &ctx.state_jsonl,
            ctx.cycle,
            "keep_marker_scan",
            "STEP_KEEP_MARKER_OK",
            json!({"detail": "no new commits"}),
        )
        .ok();
        return Ok(());
    }

    let repo_s = ctx.repo_root.display().to_string();
    // Rename-aware diff: get changed files (name-status).
    let diff_out = std::process::Command::new("git")
        .args(["-C", &repo_s, "diff", "--name-status", "-M", &format!("{pre}..{post}")])
        .output()
        .map_err(|e| StepError::Failed(format!("git diff: {e}")))?;

    let diff_text = String::from_utf8_lossy(&diff_out.stdout);
    let mut violation = false;

    for line in diff_text.lines() {
        let parts: Vec<&str> = line.split('\t').collect();
        let pre_image_path = match parts.first().and_then(|s| s.chars().next()) {
            Some('R') => parts.get(1).copied().unwrap_or(""),
            _ => parts.get(1).copied().unwrap_or(""),
        };
        if pre_image_path.is_empty() {
            continue;
        }
        let show_ref = format!("{pre}:{pre_image_path}");
        let blob = std::process::Command::new("git")
            .args(["-C", &repo_s, "show", &show_ref])
            .output();
        if let Ok(o) = blob {
            let content = String::from_utf8_lossy(&o.stdout);
            if content.contains("pi:halo:keep") {
                violation = true;
                break;
            }
        }
    }

    if violation {
        state::append_step(
            &ctx.state_jsonl,
            ctx.cycle,
            "keep_marker_scan",
            "STEP_KEEP_MARKER_VIOLATION",
            json!({}),
        )
        .ok();
        ctx.keep_marker_violated = true;
        return Err(StepError::Failed("keep_marker_violation".into()));
    }

    state::append_step(
        &ctx.state_jsonl,
        ctx.cycle,
        "keep_marker_scan",
        "STEP_KEEP_MARKER_OK",
        json!({}),
    )
    .ok();
    Ok(())
}

fn step_smoke(ctx: &mut CycleCtx) -> StepResult {
    let parts: Vec<&str> = ctx.smoke_cmd.split_whitespace().collect();
    let (prog, args) = parts.split_first().unwrap_or((&"true", &[]));
    let mut cmd = std::process::Command::new(prog);
    cmd.args(args).current_dir(ctx.repo_root);

    let child = cmd
        .spawn()
        .map_err(|e| StepError::Failed(format!("spawn smoke: {e}")))?;
    let status = wait_child(
        child,
        ctx.signal_received.clone(),
        Duration::from_secs(ctx.smoke_timeout),
    );

    let ok = matches!(&status, Ok(s) if s.success());
    if ok {
        state::append_step(
            &ctx.state_jsonl,
            ctx.cycle,
            "smoke",
            "STEP_SMOKE_PASSED",
            json!({}),
        )
        .ok();
        Ok(())
    } else {
        state::append_step(
            &ctx.state_jsonl,
            ctx.cycle,
            "smoke",
            "STEP_SMOKE_FAILED",
            json!({"exit_code": status.as_ref().ok().and_then(|s| s.code())}),
        )
        .ok();
        ctx.smoke_failed = true;
        Err(StepError::Failed("smoke failed".into()))
    }
}

fn step_rollback_if_regress(ctx: &mut CycleCtx) -> StepResult {
    if !ctx.smoke_failed && !ctx.keep_marker_violated {
        // No regression.
        state::append_step(
            &ctx.state_jsonl,
            ctx.cycle,
            "rollback_if_regress",
            "STEP_ROLLBACK_NONE_NEEDED",
            json!({}),
        )
        .ok();
        state::append_meta(&ctx.state_jsonl, "STREAK_RESET", json!({})).ok();
        return Ok(());
    }

    let pre = match &ctx.pre_target_head {
        Some(s) => s.clone(),
        None => {
            state::append_step(
                &ctx.state_jsonl,
                ctx.cycle,
                "rollback_if_regress",
                "STEP_ROLLBACK_NONE_NEEDED",
                json!({"detail": "no pre_sha"}),
            )
            .ok();
            state::append_meta(&ctx.state_jsonl, "STREAK_RESET", json!({})).ok();
            return Ok(());
        }
    };
    let post = ctx.post_target_head.clone().unwrap_or_else(|| pre.clone());

    if pre == post {
        state::append_step(
            &ctx.state_jsonl,
            ctx.cycle,
            "rollback_if_regress",
            "STEP_ROLLBACK_NONE_NEEDED",
            json!({"detail": "no commits in window"}),
        )
        .ok();
        if !ctx.keep_marker_violated {
            state::append_meta(&ctx.state_jsonl, "STREAK_INCREMENTED", json!({})).ok();
        }
        return Ok(());
    }

    let repo_s = ctx.repo_root.display().to_string();
    let rev_list = std::process::Command::new("git")
        .args(["-C", &repo_s, "rev-list", &format!("{pre}..{post}"), "--first-parent"])
        .output()
        .map_err(|e| StepError::Failed(format!("rev-list: {e}")))?;
    let shas: Vec<String> = String::from_utf8_lossy(&rev_list.stdout)
        .lines()
        .map(str::to_string)
        .filter(|s| !s.is_empty())
        .collect();

    if shas.is_empty() {
        state::append_step(
            &ctx.state_jsonl,
            ctx.cycle,
            "rollback_if_regress",
            "STEP_ROLLBACK_NONE_NEEDED",
            json!({"detail": "empty commit window"}),
        )
        .ok();
        if !ctx.keep_marker_violated {
            state::append_meta(&ctx.state_jsonl, "STREAK_INCREMENTED", json!({})).ok();
        }
        return Ok(());
    }

    state::append_step(
        &ctx.state_jsonl,
        ctx.cycle,
        "rollback_if_regress",
        "STEP_REVERT_COMMITS",
        json!({"shas": &shas}),
    )
    .ok();

    let mut reverted: Vec<String> = Vec::new();
    for sha in &shas {
        let out = std::process::Command::new("git")
            .args(["-C", &repo_s, "revert", "--no-edit", sha])
            .output()
            .map_err(|e| StepError::Failed(format!("revert {sha}: {e}")))?;
        if !out.status.success() {
            state::append_step(
                &ctx.state_jsonl,
                ctx.cycle,
                "rollback_if_regress",
                "STEP_REVERT_COMMITS_FAILED",
                json!({
                    "error_kind": "git_revert_conflict",
                    "partial_shas": &reverted,
                }),
            )
            .ok();
            return Err(StepError::Failed("revert failed".into()));
        }
        reverted.push(sha.clone());
    }

    state::append_step(
        &ctx.state_jsonl,
        ctx.cycle,
        "rollback_if_regress",
        "STEP_REVERT_COMMITS_DONE",
        json!({"reverted_shas": &reverted}),
    )
    .ok();

    // Post-revert smoke (only when keep_marker_violated is false).
    if !ctx.keep_marker_violated {
        let smoke_ok = run_smoke_once(ctx);
        if smoke_ok {
            state::append_step(
                &ctx.state_jsonl,
                ctx.cycle,
                "rollback_if_regress",
                "STEP_SMOKE_POST_REVERT_PASSED",
                json!({}),
            )
            .ok();
            state::append_step(
                &ctx.state_jsonl,
                ctx.cycle,
                "rollback_if_regress",
                "STEP_ROLLBACK_DONE",
                json!({}),
            )
            .ok();
            state::append_meta(&ctx.state_jsonl, "STREAK_INCREMENTED", json!({})).ok();
        } else {
            state::append_step(
                &ctx.state_jsonl,
                ctx.cycle,
                "rollback_if_regress",
                "STEP_SMOKE_POST_REVERT_FAILED",
                json!({}),
            )
            .ok();
            state::append_step(
                &ctx.state_jsonl,
                ctx.cycle,
                "rollback_if_regress",
                "STEP_ROLLBACK_FUTILE",
                json!({}),
            )
            .ok();
            state::append_meta(&ctx.state_jsonl, "STREAK_UNCHANGED_FUTILE", json!({})).ok();
            // Futile → PAUSED.
            state::cycle_aborted(
                &ctx.state_jsonl,
                ctx.cycle,
                json!({"reason": "paused", "subreason": "rollback_futile"}),
            )
            .ok();
            write_paused(&ctx.halo_dir).ok();
            return Err(StepError::Failed("rollback_futile".into()));
        }
    } else {
        state::append_step(
            &ctx.state_jsonl,
            ctx.cycle,
            "rollback_if_regress",
            "STEP_ROLLBACK_DONE",
            json!({"keep_marker_route": true}),
        )
        .ok();
    }

    Ok(())
}

fn step_evolve_tick(ctx: &mut CycleCtx) -> StepResult {
    // v0.27 fix (canary bug #11): wire the real evolve::run_tick.
    // Halo's supervisor loop is sync; run_tick is async + needs a
    // tokio runtime + a Replay backend. Build the inputs the same
    // way `run_internal_evolve_tick` does, block_on the call, log
    // outcome to state.jsonl. On `applied`, follow up with the
    // RFD §evolve_tick git-commit dance: `git add AGENTS.md &&
    // git commit -m "..." -m "Halo-Evolve: ..."`.
    use crate::context::{agent_dir, auth_path, sessions_dir, settings_paths};
    use crate::evolve::{run_tick, SubprocessReplay, TickInputs, TickReport};
    use pi_agent_core::Settings;
    use pi_ai::{AuthStorage, ModelRegistry};
    use std::time::Duration;

    let cwd = ctx.repo_root.to_path_buf();

    // Load settings (global + project).
    let (global, project) = settings_paths();
    let mut settings = Settings::load(&global);
    settings.merge_project(&project);

    // Auth: file then env. Per RFD 0027 §4.5 #8: binary uses
    // ENV_KEYS-explicit scan (own-machine trust model).
    let auth = AuthStorage::open(auth_path()).unwrap_or_else(|_| AuthStorage::in_memory());
    let env = AuthStorage::from_env_explicit(AuthStorage::ENV_KEYS)
        .unwrap_or_else(|_| AuthStorage::in_memory());
    for (p, _) in AuthStorage::ENV_KEYS {
        if let Some(m) = env.get(p) {
            auth.set(p, m);
        }
    }
    let registry = ModelRegistry::new(auth.clone());
    let agents_md_path = locate_agents_md_for_evolve(&cwd, &agent_dir());
    let pi_binary = std::env::current_exe().unwrap_or_else(|_| std::path::PathBuf::from("pi"));
    let replay = SubprocessReplay {
        pi_binary,
        timeout: Duration::from_secs(180),
        auto_approve: "auto-policy".into(),
        cwd: Some(cwd.clone()),
    };
    let inputs = TickInputs {
        cwd: &cwd,
        sessions_root: &sessions_dir(),
        agents_md_path: agents_md_path.clone(),
        settings: &settings,
        registry: &registry,
        auth: &auth,
    };

    let rt = match tokio::runtime::Builder::new_current_thread().enable_all().build() {
        Ok(rt) => rt,
        Err(e) => {
            state::append_step(
                &ctx.state_jsonl,
                ctx.cycle,
                "evolve_tick",
                "STEP_EVOLVE_TICK_DONE",
                json!({"skipped_reason": format!("tokio_runtime_build_failed: {e}")}),
            )
            .ok();
            return Ok(());
        }
    };

    let report = rt.block_on(async { run_tick(inputs, &replay).await });

    match report {
        Ok(TickReport::Skipped(why)) => {
            state::append_step(
                &ctx.state_jsonl,
                ctx.cycle,
                "evolve_tick",
                "STEP_EVOLVE_TICK_DONE",
                json!({"skipped_reason": format!("{:?}", why)}),
            )
            .ok();
        }
        Ok(TickReport::Ran { baseline, generations, applied_hash }) => {
            // On applied: commit AGENTS.md onto target_branch with
            // Halo-Evolve trailer per RFD §evolve_tick. The apply
            // step already wrote AGENTS.md to disk; we checkout
            // target_branch + add + commit. RFD's `<pre>→<post>`
            // format requires both hashes; we only get post (the
            // applied) from TickReport, so the trailer encodes just
            // the post-hash (still grep-friendly for bisect).
            if let Some(post) = &applied_hash {
                let _ = std::process::Command::new("git")
                    .args(["-C", &cwd.display().to_string(), "checkout", &ctx.target_branch])
                    .status();
                let _ = std::process::Command::new("git")
                    .args(["-C", &cwd.display().to_string(), "add", "AGENTS.md"])
                    .status();
                let msg1 = format!("halo cycle {}: evolve apply (post-hash {})", ctx.cycle, post);
                let msg2 = format!("Halo-Evolve: {}", post);
                let _ = std::process::Command::new("git")
                    .args([
                        "-C", &cwd.display().to_string(),
                        "commit", "-m", &msg1, "-m", &msg2,
                    ])
                    .status();
            }
            state::append_step(
                &ctx.state_jsonl,
                ctx.cycle,
                "evolve_tick",
                "STEP_EVOLVE_TICK_DONE",
                json!({
                    "applied_hash": applied_hash,
                    "baseline_pass_rate": baseline.pass_rate,
                    "n_generations": generations.len(),
                }),
            )
            .ok();
        }
        Err(e) => {
            state::append_step(
                &ctx.state_jsonl,
                ctx.cycle,
                "evolve_tick",
                "STEP_EVOLVE_TICK_DONE",
                json!({"skipped_reason": format!("error: {e}")}),
            )
            .ok();
        }
    }

    Ok(())
}

/// Locate AGENTS.md: project ancestors first, then global.
fn locate_agents_md_for_evolve(cwd: &Path, agent_dir: &Path) -> std::path::PathBuf {
    for dir in cwd.ancestors() {
        let p = dir.join("AGENTS.md");
        if p.is_file() {
            return p;
        }
    }
    agent_dir.join("AGENTS.md")
}

// ── Helpers ──────────────────────────────────────────────────────────────────

fn finish_aborted(ctx: &CycleCtx, reason: String) -> Result<CycleOutcome> {
    let signal_name = if ctx.sigint_received.load(Ordering::SeqCst) {
        "SIGINT"
    } else if ctx.sigterm_received.load(Ordering::SeqCst) {
        "SIGTERM"
    } else {
        "SIGINT"
    };
    let detail = if reason == "sigint" || reason == "sigterm" {
        json!({"reason": "sigint", "signal": signal_name})
    } else {
        json!({"reason": reason})
    };
    state::cycle_aborted(&ctx.state_jsonl, ctx.cycle, detail)?;
    if reason == "sigint" || reason == "sigterm" {
        // Re-queue dispatched proposal as pending.
        if let Some(ref pid) = ctx.proposal_id {
            append_proposal_status(
                &ctx.backlog_jsonl,
                pid,
                "pending",
                json!({"reason": "supervisor_interrupted", "signal": signal_name}),
            )
            .ok();
        }
        write_paused(&ctx.halo_dir)?;
    }
    Ok(CycleOutcome::Aborted { reason })
}

fn write_paused(halo_dir: &Path) -> Result<()> {
    std::fs::create_dir_all(halo_dir)?;
    std::fs::write(halo_dir.join("paused"), b"")?;
    Ok(())
}

fn git_rev_parse(repo: &Path, refname: &str) -> Result<String> {
    let out = std::process::Command::new("git")
        .args(["-C", &repo.display().to_string(), "rev-parse", refname])
        .output()
        .context("git rev-parse")?;
    if !out.status.success() {
        bail!("git rev-parse {refname}: {}", String::from_utf8_lossy(&out.stderr));
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

/// Read all `pending` proposals from `backlog.jsonl`.
/// Replay: latest event per id determines current status.
pub fn read_pending_proposals(backlog_jsonl: &Path) -> Vec<Value> {
    let Ok(text) = std::fs::read_to_string(backlog_jsonl) else {
        return vec![];
    };
    let mut status: std::collections::HashMap<String, String> = Default::default();
    let mut order: Vec<String> = Vec::new();
    let mut proposals: std::collections::HashMap<String, Value> = Default::default();

    for line in text.lines().filter(|l| !l.trim().is_empty()) {
        let Ok(evt): std::result::Result<Value, _> = serde_json::from_str(line) else {
            continue;
        };
        let Some(id) = evt.get("id").and_then(|v| v.as_str()) else {
            continue;
        };
        let id = id.to_string();
        match evt.get("kind").and_then(|v| v.as_str()) {
            Some("proposal_created") => {
                if !proposals.contains_key(&id) {
                    order.push(id.clone());
                }
                proposals.insert(id.clone(), evt.clone());
                status.insert(id, "pending".into());
            }
            Some("proposal_status_changed") => {
                if let Some(s) = evt.get("status").and_then(|v| v.as_str()) {
                    status.insert(id, s.to_string());
                }
            }
            Some("proposal_dropped") => {
                status.insert(id, "dropped".into());
            }
            _ => {}
        }
    }

    order
        .into_iter()
        .filter(|id| status.get(id).map(|s| s == "pending").unwrap_or(false))
        .filter_map(|id| proposals.get(&id).cloned())
        .collect()
}

pub fn append_proposal_status(
    backlog_jsonl: &Path,
    id: &str,
    status: &str,
    detail: Value,
) -> Result<()> {
    backlog::append_proposal_status_changed(backlog_jsonl, id, status, detail)
}

fn run_smoke_once(ctx: &CycleCtx) -> bool {
    let parts: Vec<&str> = ctx.smoke_cmd.split_whitespace().collect();
    let (prog, args) = parts.split_first().unwrap_or((&"true", &[]));
    std::process::Command::new(prog)
        .args(args)
        .current_dir(ctx.repo_root)
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// Wait for a child process, checking the signal flag every `poll_interval`.
/// Returns the ExitStatus or an Err on wait failure.
fn wait_child(
    mut child: std::process::Child,
    signal: Arc<AtomicBool>,
    poll_interval: Duration,
) -> std::io::Result<std::process::ExitStatus> {
    loop {
        match child.try_wait()? {
            Some(status) => return Ok(status),
            None => {
                if signal.load(Ordering::SeqCst) {
                    // Signal received — the handler will kill the child PG;
                    // just wait for it to exit.
                    return child.wait();
                }
                std::thread::sleep(poll_interval.min(Duration::from_millis(500)));
            }
        }
    }
}

/// Build the halo directory path for a repo.
pub fn halo_dir_for_repo(repo_root: &Path) -> Option<PathBuf> {
    let home = dirs::home_dir()?;
    let slug = repo_root
        .canonicalize()
        .unwrap_or_else(|_| repo_root.to_path_buf())
        .display()
        .to_string()
        .replace(['/', '\\', ':'], "_");
    Some(home.join(".pi").join("halo").join(slug))
}

/// Prune `halo/cycle-*` branches older than `keep_branches` (count-based).
/// `keep_branches == 0` means keep all.
pub fn prune_old_cycle_branches(repo_root: &Path, keep_branches: u32) {
    if keep_branches == 0 {
        return;
    }
    let repo_s = repo_root.display().to_string();
    let out = match std::process::Command::new("git")
        .args(["-C", &repo_s, "branch", "--list", "halo/cycle-*", "--format=%(refname:short)"])
        .output()
    {
        Ok(o) => o,
        Err(_) => return,
    };
    let mut branches: Vec<String> = String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter(|l| !l.trim().is_empty())
        .map(str::to_string)
        .collect();
    // Sort oldest→newest by branch name (cycle number embedded).
    branches.sort();

    let to_delete = if branches.len() as u32 > keep_branches {
        &branches[..(branches.len() - keep_branches as usize)]
    } else {
        return;
    };

    for branch in to_delete {
        let _ = std::process::Command::new("git")
            .args(["-C", &repo_s, "branch", "-D", branch])
            .output();
    }
}
