//! Orchestrator runner — v1.
//!
//! Walks the topologically-ordered campaign milestones. For each
//! eligible milestone (all `depends_on` reached `MERGED`):
//!
//!   1. Dispatch implementer subagent (v1 dispatch is subprocess-based
//!      via [`crate::dispatch::Dispatch`]).
//!   2. Dispatch reviewer subagent on the resulting branch.
//!   3. Parse `Merge readiness:` line from reviewer's output.
//!   4. On `READY_TO_MERGE`: cherry-pick the branch onto target_branch.
//!      → `MERGED`. (`BLOCKED_ON_CONFLICT` if the cherry-pick fails.)
//!   5. On `NEEDS_FIX`: re-dispatch implementer with reviewer text
//!      appended, fix-loop counter ticks. Up to `fix_loop_max`
//!      iterations; exhaustion → `FAILED`.
//!   6. On `DO_NOT_MERGE` or unparseable verdict: `FAILED`.
//!
//! Every transition is appended to
//! `<state-root>/<campaign>/state.jsonl` (one event per line). On
//! resume the snapshot is rebuilt by replaying the log.
//!
//! v1 does NOT implement: parallel execution, worktree-per-milestone,
//! override-rule forwarding, structured Concerns extraction, retry
//! policy, BLOCKED_ON_REVIEW_STALE detection, MERGE-REPORT writer.
//! Each is called out in the lib-level `dispatch` / `merge` / `verdict`
//! module docs and in the v0→v1→v2 roadmap on RFD 0021.

use crate::dispatch::{agent_for, Dispatch, DispatchRole};
use crate::merge::{cherry_pick_to_target, git_checkout, rev_parse, MergeOutcome};
use crate::plan::topological_order;
use crate::schema::Campaign;
use crate::verdict::{parse_verdict, MergeReadiness};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

/// One state-transition event, persisted as a single line in
/// `state.jsonl`. The shape matches RFD 0021 §"Persisted state
/// layout": `{milestone, from, to, ts, detail}`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct StateEvent {
    pub milestone: String,
    pub from: String,
    pub to: String,
    pub ts: u64,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub detail: String,
}

/// Outcome of one milestone's full implementer→reviewer→merge cycle.
#[derive(Debug, Clone)]
pub struct MilestoneOutcome {
    pub id: String,
    pub final_state: String,
    pub fix_loop_iterations: u32,
    pub events_written: usize,
}

/// Aggregate summary for one `run` invocation.
#[derive(Debug, Clone)]
pub struct RunSummary {
    pub campaign: String,
    pub state_path: PathBuf,
    pub outcomes: Vec<MilestoneOutcome>,
    /// Per-RFD §"Exit codes":
    ///   0 — every non-FAILED milestone reached MERGED
    ///   2 — at least one FAILED
    ///   3 — at least one BLOCKED_ON_CONFLICT or BLOCKED_ON_REVIEW_STALE
    pub exit_code: i32,
}

/// State a milestone can reach. Strings on the wire match RFD's state
/// machine vocabulary so an external tool reading state.jsonl gets
/// the same names.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FinalState {
    Merged,
    Failed,
    BlockedOnConflict,
    BlockedOnReviewStale,
}

impl FinalState {
    fn label(self) -> &'static str {
        match self {
            FinalState::Merged => "MERGED",
            FinalState::Failed => "FAILED",
            FinalState::BlockedOnConflict => "BLOCKED_ON_CONFLICT",
            FinalState::BlockedOnReviewStale => "BLOCKED_ON_REVIEW_STALE",
        }
    }
}

/// Resolve `<state_root>/<campaign-name>/state.jsonl`, creating the
/// parent directory if needed. The campaign name is sanitised
/// (`/` → `_`) so a TOML name with slashes can't escape the root.
pub fn state_path_for(state_root: &Path, campaign_name: &str) -> std::io::Result<PathBuf> {
    let safe = campaign_name.replace(['/', '\\'], "_");
    let dir = state_root.join(&safe);
    fs::create_dir_all(&dir)?;
    Ok(dir.join("state.jsonl"))
}

/// Run the campaign with a real subprocess dispatcher rooted at the
/// caller's repo. Convenience wrapper around `run_with` that picks
/// `RealDispatch::default()`.
pub fn run(campaign: &Campaign, state_root: &Path) -> std::io::Result<RunSummary> {
    let dispatcher = crate::dispatch::RealDispatch::default();
    run_with(campaign, state_root, &dispatcher, &std::env::current_dir()?)
}

/// Full executor (testable). `repo_root` is the working tree that
/// contains all milestone branches and `target_branch`; the runner
/// will `git checkout` between branches and run `git cherry-pick`
/// against it.
pub fn run_with(
    campaign: &Campaign,
    state_root: &Path,
    dispatcher: &dyn Dispatch,
    repo_root: &Path,
) -> std::io::Result<RunSummary> {
    let state_path = state_path_for(state_root, &campaign.name)?;
    let mut log = OpenOptions::new()
        .create(true)
        .append(true)
        .open(&state_path)?;

    let mut outcomes: Vec<MilestoneOutcome> = Vec::with_capacity(campaign.milestones.len());
    let mut milestone_state: HashMap<String, FinalState> = HashMap::new();
    let mut events_for: HashMap<String, usize> = HashMap::new();

    for m in topological_order(campaign) {
        // ── Eligibility check ───────────────────────────────────
        // Skip if any dependency didn't reach MERGED. Skipped
        // milestones are recorded as FAILED with a clear detail
        // string so an operator can see the cascade.
        let blocked_by: Vec<&str> = m
            .depends_on
            .iter()
            .filter(|dep| {
                !matches!(
                    milestone_state.get(*dep),
                    Some(FinalState::Merged)
                )
            })
            .map(|s| s.as_str())
            .collect();
        if !blocked_by.is_empty() {
            emit_event(
                &mut log,
                &m.id,
                "PENDING",
                FinalState::Failed.label(),
                &format!("blocked: dependency not merged ({})", blocked_by.join(",")),
                &mut events_for,
            )?;
            milestone_state.insert(m.id.clone(), FinalState::Failed);
            outcomes.push(MilestoneOutcome {
                id: m.id.clone(),
                final_state: FinalState::Failed.label().into(),
                fix_loop_iterations: 0,
                events_written: events_for.get(&m.id).copied().unwrap_or(0),
            });
            continue;
        }

        // ── Fix-loop ────────────────────────────────────────────
        let max_iter = m.effective_fix_loop_max(&campaign.defaults);
        let default_reviewer = &campaign.defaults.reviewer;

        let mut accumulated_assignment = m.assignment.clone();
        let mut iter: u32 = 0;
        let final_outcome: FinalState = loop {
            iter += 1;

            // B2 fix: ensure the working tree is on m.branch before
            // every dispatch. Without this, the second-and-onward
            // milestones execute in `target_branch` (left there by
            // the previous milestone's cherry-pick), so the
            // implementer reads the wrong files and the reviewer
            // diffs the wrong refs.
            if let Err(e) = git_checkout(repo_root, &m.branch) {
                emit_event(
                    &mut log,
                    &m.id,
                    if iter == 1 { "PENDING" } else { "REVIEWED" },
                    "FAILED",
                    &format!("git checkout {} failed: {e}", m.branch),
                    &mut events_for,
                )?;
                break FinalState::Failed;
            }

            // Implementer dispatch.
            let implementer = agent_for(DispatchRole::Implementer, m, default_reviewer);
            emit_event(
                &mut log,
                &m.id,
                if iter == 1 { "PENDING" } else { "REVIEWED" },
                "DISPATCHED",
                &format!("iter={iter} agent={implementer}"),
                &mut events_for,
            )?;
            let imp_outcome = dispatcher
                .dispatch(
                    DispatchRole::Implementer,
                    &implementer,
                    &accumulated_assignment,
                    repo_root,
                )
                .unwrap_or_else(|e| crate::dispatch::DispatchOutcome {
                    agent: implementer.clone(),
                    success: false,
                    model_output: String::new(),
                    stderr: e.to_string(),
                    exit_code: -1,
                    duration_ms: 0,
                });
            if !imp_outcome.success {
                emit_event(
                    &mut log,
                    &m.id,
                    "DISPATCHED",
                    "FAILED",
                    &format!(
                        "implementer dispatch failed (exit={}): {}",
                        imp_outcome.exit_code,
                        truncate(&imp_outcome.stderr, 240)
                    ),
                    &mut events_for,
                )?;
                break FinalState::Failed;
            }

            // B3 fix: capture the review snapshot per RFD §"Review
            // snapshot" — the {branch_sha, target_head} pair as
            // observed *immediately before* the reviewer is
            // dispatched. These are used at merge time to detect
            // staleness and to cherry-pick the exact sha the
            // reviewer saw (not a fresh rev-parse that might
            // include post-review pushes).
            let branch_sha_at_review = rev_parse(repo_root, &m.branch).ok();
            let target_head_at_review = rev_parse(repo_root, &campaign.target_branch).ok();

            // Reviewer dispatch.
            let reviewer = agent_for(DispatchRole::Reviewer, m, default_reviewer);
            let rev_outcome = dispatcher
                .dispatch(
                    DispatchRole::Reviewer,
                    &reviewer,
                    &reviewer_assignment(m, &imp_outcome.model_output, &campaign.target_branch),
                    repo_root,
                )
                .unwrap_or_else(|e| crate::dispatch::DispatchOutcome {
                    agent: reviewer.clone(),
                    success: false,
                    model_output: String::new(),
                    stderr: e.to_string(),
                    exit_code: -1,
                    duration_ms: 0,
                });
            if !rev_outcome.success {
                emit_event(
                    &mut log,
                    &m.id,
                    "DISPATCHED",
                    "FAILED",
                    &format!(
                        "reviewer dispatch failed (exit={}): {}",
                        rev_outcome.exit_code,
                        truncate(&rev_outcome.stderr, 240)
                    ),
                    &mut events_for,
                )?;
                break FinalState::Failed;
            }

            emit_event(
                &mut log,
                &m.id,
                "DISPATCHED",
                "REVIEWED",
                &format!("iter={iter} reviewer={reviewer}"),
                &mut events_for,
            )?;

            // Verdict.
            let verdict = parse_verdict(&rev_outcome.model_output);
            match verdict {
                Some(MergeReadiness::Ready) => {
                    // Use the snapshot captured before the reviewer
                    // was dispatched, NOT a fresh rev-parse — this
                    // is what RFD §"Review snapshot" requires (so a
                    // post-review rogue push to the branch can't
                    // sneak unreviewed commits in).
                    let Some(branch_sha) = branch_sha_at_review.clone() else {
                        emit_event(
                            &mut log,
                            &m.id,
                            "REVIEWED",
                            "FAILED",
                            &format!("rev-parse {} (review snapshot) failed", m.branch),
                            &mut events_for,
                        )?;
                        break FinalState::Failed;
                    };
                    emit_event(
                        &mut log,
                        &m.id,
                        "REVIEWED",
                        "MERGE_PENDING",
                        &format!(
                            "branch_sha={branch_sha} target_head={}",
                            target_head_at_review.as_deref().unwrap_or("?")
                        ),
                        &mut events_for,
                    )?;

                    // B3 fix: re-rev-parse target_branch at merge
                    // time. If it's different from the value we
                    // captured before the reviewer was dispatched,
                    // the review is stale — some other commit
                    // landed on target_branch between review and
                    // merge, so we cannot trust that the reviewed
                    // diff still applies cleanly. Transition to
                    // BLOCKED_ON_REVIEW_STALE and let the operator
                    // decide whether to re-review (RFD §"Operator
                    // recovery"; v3 will add auto-rebase).
                    let target_head_now =
                        rev_parse(repo_root, &campaign.target_branch).ok();
                    if target_head_now != target_head_at_review {
                        emit_event(
                            &mut log,
                            &m.id,
                            "MERGE_PENDING",
                            "BLOCKED_ON_REVIEW_STALE",
                            &format!(
                                "target_head moved {:?} -> {:?} between review and merge",
                                target_head_at_review, target_head_now
                            ),
                            &mut events_for,
                        )?;
                        break FinalState::BlockedOnReviewStale;
                    }

                    let merge_result = cherry_pick_to_target(
                        repo_root,
                        &campaign.target_branch,
                        &branch_sha,
                    );
                    let outcome = match merge_result {
                        MergeOutcome::Merged => {
                            emit_event(
                                &mut log,
                                &m.id,
                                "MERGE_PENDING",
                                "MERGED",
                                &format!("branch_sha={branch_sha}"),
                                &mut events_for,
                            )?;
                            FinalState::Merged
                        }
                        MergeOutcome::Conflict => {
                            emit_event(
                                &mut log,
                                &m.id,
                                "MERGE_PENDING",
                                "BLOCKED_ON_CONFLICT",
                                &format!("cherry-pick {branch_sha} → {} conflicted", campaign.target_branch),
                                &mut events_for,
                            )?;
                            FinalState::BlockedOnConflict
                        }
                        MergeOutcome::GitError(e) => {
                            emit_event(
                                &mut log,
                                &m.id,
                                "MERGE_PENDING",
                                "FAILED",
                                &format!("git error: {e}"),
                                &mut events_for,
                            )?;
                            FinalState::Failed
                        }
                    };
                    break outcome;
                }
                Some(MergeReadiness::NeedsFix) => {
                    if iter >= max_iter {
                        emit_event(
                            &mut log,
                            &m.id,
                            "REVIEWED",
                            "FAILED",
                            &format!("fix_loop_max={max_iter} exhausted"),
                            &mut events_for,
                        )?;
                        break FinalState::Failed;
                    }
                    // Append reviewer's text to next implementer turn.
                    accumulated_assignment.push_str("\n\n# Reviewer NEEDS_FIX\n\n");
                    accumulated_assignment.push_str(&rev_outcome.model_output);
                    // Loop continues: REVIEWED → DISPATCHED transition
                    // is logged at top of next iteration.
                }
                Some(MergeReadiness::DoNotMerge) => {
                    emit_event(
                        &mut log,
                        &m.id,
                        "REVIEWED",
                        "FAILED",
                        "verdict=DO_NOT_MERGE",
                        &mut events_for,
                    )?;
                    break FinalState::Failed;
                }
                None => {
                    // Reviewer didn't produce a parseable verdict line.
                    // RFD's "fallback mode" treats this as NeedsFix; we
                    // do too — but if we're already at fix_loop_max,
                    // FAILED.
                    if iter >= max_iter {
                        emit_event(
                            &mut log,
                            &m.id,
                            "REVIEWED",
                            "FAILED",
                            "no parseable verdict; fix_loop exhausted",
                            &mut events_for,
                        )?;
                        break FinalState::Failed;
                    }
                    accumulated_assignment.push_str("\n\n# Reviewer (unparseable verdict)\n\n");
                    accumulated_assignment.push_str(&rev_outcome.model_output);
                }
            }
        };

        milestone_state.insert(m.id.clone(), final_outcome);
        outcomes.push(MilestoneOutcome {
            id: m.id.clone(),
            final_state: final_outcome.label().into(),
            fix_loop_iterations: iter,
            events_written: events_for.get(&m.id).copied().unwrap_or(0),
        });
    }

    let exit_code = compute_exit_code(&milestone_state);

    Ok(RunSummary {
        campaign: campaign.name.clone(),
        state_path,
        outcomes,
        exit_code,
    })
}

fn compute_exit_code(states: &HashMap<String, FinalState>) -> i32 {
    let mut has_blocked = false;
    let mut has_failed = false;
    for st in states.values() {
        match st {
            FinalState::Merged => {}
            FinalState::Failed => has_failed = true,
            FinalState::BlockedOnConflict | FinalState::BlockedOnReviewStale => {
                has_blocked = true
            }
        }
    }
    // RFD §"Exit codes": blocked outranks failed (manual-resolution).
    if has_blocked {
        3
    } else if has_failed {
        2
    } else {
        0
    }
}

fn reviewer_assignment(
    m: &crate::schema::Milestone,
    implementer_output: &str,
    target_branch: &str,
) -> String {
    // C1 from the review: previously the diff command was
    // hardcoded to `main`, breaking on campaigns whose
    // target_branch isn't main. Now we plumb the campaign's
    // configured target_branch through.
    format!(
        "Review milestone `{id}` on branch `{branch}`.\n\n\
         The implementer just produced this output:\n\n\
         ---\n{out}\n---\n\n\
         Read the diff (git diff {target}...{branch}). Produce a Markdown \
         verdict whose FINAL non-empty line is exactly one of \
         `Merge readiness: READY_TO_MERGE`, `Merge readiness: NEEDS_FIX`, \
         or `Merge readiness: DO_NOT_MERGE`.",
        id = m.id,
        branch = m.branch,
        target = target_branch,
        out = truncate(implementer_output, 4000),
    )
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    let mut out: String = s.chars().take(max).collect();
    out.push_str(" ...[truncated]");
    out
}

fn emit_event(
    log: &mut std::fs::File,
    milestone: &str,
    from: &str,
    to: &str,
    detail: &str,
    events_for: &mut HashMap<String, usize>,
) -> std::io::Result<()> {
    let evt = StateEvent {
        milestone: milestone.to_string(),
        from: from.to_string(),
        to: to.to_string(),
        ts: now_ms(),
        detail: detail.to_string(),
    };
    let line = serde_json::to_string(&evt)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    log.write_all(line.as_bytes())?;
    log.write_all(b"\n")?;
    *events_for.entry(milestone.to_string()).or_insert(0) += 1;
    Ok(())
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// Replay an existing `state.jsonl` into the in-memory event list.
/// Skips a truncated trailing line (RFD 0021 §"Persisted state layout":
/// a partial write must not corrupt the snapshot).
pub fn replay(state_path: &Path) -> std::io::Result<Vec<StateEvent>> {
    let text = match fs::read_to_string(state_path) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(e),
    };
    let mut events = Vec::new();
    for line in text.lines() {
        if line.trim().is_empty() {
            continue;
        }
        match serde_json::from_str::<StateEvent>(line) {
            Ok(e) => events.push(e),
            Err(_) => {
                // Truncated final line: stop replay here without
                // erroring. RFD §"Persisted state layout" — partial
                // writes can never corrupt state; resume drops the
                // truncated line.
                break;
            }
        }
    }
    Ok(events)
}
