//! Tick orchestrator — strings together every G-group primitive (G8 part 2).
//!
//! This is the function that the `--internal-evolve-tick` subprocess
//! actually runs:
//!
//! 1. Acquire single-instance [`Lock`] (return early if held).
//! 2. Load [`State`] + [`CostLedger`] for the cwd.
//! 3. Locate AGENTS.md (project, then global).
//! 4. Load benchmark [`BenchmarkCase`]s from past trajectories
//!    (excluding `Replay`-source outcomes).
//! 5. Pass everything through [`should_run`]; bail with [`SkipReason`]
//!    if any gate fails.
//! 6. Build evidence (split cases by historical_success).
//! 7. Build [`Mutator`] (slow model) and [`Replay`] backend
//!    (subprocess, in production).
//! 8. Run baseline benchmark → [`Candidate`] with `mutated_section: None`.
//! 9. For each generation: pick a target section, mutate, build candidate
//!    [`AgentsMd`], run benchmark, append to candidates list. Cost-cap
//!    bail-out between iterations.
//! 10. [`pareto_frontier`] + [`best_strict_improvement`].
//! 11. If a winner is found: [`backup_and_apply`] + write
//!     [`PendingApply`] for the rollback monitor.
//! 12. Persist new state + cost ledger + append all candidates to the
//!     generations log.
//! 13. Lock auto-releases on Drop.
//!
//! Failures are logged into the generations log as a `note` field on a
//! synthetic skip-entry; the daemon never panics out.

use std::path::{Path, PathBuf};

use chrono::Utc;
use pi_agent_core::{EvolveSettings, Settings};
use pi_ai::{AuthStorage, ModelRegistry};

use super::agents_md::AgentsMd;
use super::apply::{
    add_poison, append_generation, backup_and_apply, best_strict_improvement,
    pareto_frontier, Candidate, GenerationLogEntry, PendingApply,
};
use super::benchmark::{
    load_cases, run_all, summarize, BenchmarkCase, BenchmarkSummary, Replay,
};
use super::mutate::{EvidenceItem, MutationEvidence, Mutator, MutatorConfig};
use super::tick::{should_run, CostLedger, Lock, SkipReason, State, TickDecision};

/// What a tick actually did. Returned to the daemon caller for logging.
#[derive(Debug)]
pub enum TickReport {
    Skipped(SkipReason),
    Ran {
        baseline: BenchmarkSummary,
        generations: Vec<Candidate>,
        applied_hash: Option<String>,
    },
}

#[derive(Debug, thiserror::Error)]
pub enum TickError {
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("benchmark: {0}")]
    Benchmark(#[from] super::benchmark::BenchmarkError),
    #[error("mutate: {0}")]
    Mutate(#[from] super::mutate::MutateError),
    #[error("apply: {0}")]
    Apply(#[from] super::agents_md::ReplaceError),
}

/// Inputs the daemon hands the orchestrator.
pub struct TickInputs<'a> {
    pub cwd: &'a Path,
    pub sessions_root: &'a Path,
    pub agents_md_path: PathBuf,
    pub settings: &'a Settings,
    pub registry: &'a ModelRegistry,
    pub auth: &'a AuthStorage,
}

/// Run one tick. The Replay backend is pluggable so tests can pass a
/// mock; production hands in a [`super::SubprocessReplay`].
pub async fn run_tick<R: Replay>(
    inputs: TickInputs<'_>,
    replay: &R,
) -> Result<TickReport, TickError> {
    // 1. Lock.
    let lock = Lock::try_acquire(inputs.cwd)?;
    let _lock_guard = match lock {
        Some(l) => l,
        None => return Ok(TickReport::Skipped(SkipReason::LockHeld)),
    };

    // 2. State + cost.
    let state = State::load(inputs.cwd);
    let mut cost = CostLedger::load(inputs.cwd);

    // 3. Locate AGENTS.md.
    let has_agents_md = inputs.agents_md_path.is_file();

    // 4. Load benchmark cases.
    let cwd_slug = slug(inputs.cwd);
    let cases = load_cases(
        inputs.sessions_root,
        &cwd_slug,
        inputs.settings.evolve.benchmark_size as usize,
    )?;

    // 5. Gate.
    let decision = should_run(
        &inputs.settings.evolve,
        &mut cost,
        &state,
        inputs.cwd,
        cases.len() as u32,
        has_agents_md,
    );
    if let TickDecision::Skip(why) = decision {
        return Ok(TickReport::Skipped(why));
    }

    // 6. Build evidence.
    let evidence = build_evidence(&cases);

    // 7. Build Mutator (only if we'll actually mutate). When no
    // generations are configured we still run the baseline benchmark
    // and emit a TickReport — useful for measuring the current AGENTS.md
    // before the daemon is fully wired with a slow-model.
    let n_gens = inputs.settings.evolve.generations_per_tick as usize;
    let mutator: Option<Mutator> = if n_gens > 0 {
        let mutator_cfg = mutator_config_from_settings(&inputs.settings.evolve, inputs.settings);
        match Mutator::build(inputs.registry, inputs.auth, mutator_cfg) {
            Ok(m) => Some(m),
            Err(_) => return Ok(TickReport::Skipped(SkipReason::NotEnabled)),
        }
    } else {
        None
    };

    // 8. Load + parse AGENTS.md, run baseline benchmark.
    let baseline_text = std::fs::read_to_string(&inputs.agents_md_path)?;
    let baseline_doc = AgentsMd::parse(&baseline_text);
    let baseline_results = run_all(replay, &cases, &baseline_doc.render()).await?;
    let baseline_summary = summarize(&baseline_results);

    let mut candidates: Vec<Candidate> = vec![Candidate {
        hash: baseline_doc.hash(),
        summary: baseline_summary.clone(),
        body: baseline_doc.render(),
        mutated_section: None,
        note: "baseline".into(),
    }];

    // 9. Generations.
    let mutable_indices: Vec<usize> = baseline_doc
        .mutable_sections()
        .map(|(i, _)| i)
        .collect();
    if mutable_indices.is_empty() {
        // Nothing we're allowed to touch (entire file pi:keep, or only
        // preamble). Log + return without applying.
        log_candidates(inputs.cwd, &candidates);
        return Ok(TickReport::Ran {
            baseline: baseline_summary,
            generations: candidates,
            applied_hash: None,
        });
    }

    let poisoned = super::apply::poisoned_hashes(inputs.cwd);

    for gen in 0..n_gens {
        let Some(mutator) = mutator.as_ref() else {
            break; // no mutator → can't mutate; skip the loop
        };
        // Cost cap mid-loop.
        if cost.today_spend() >= inputs.settings.evolve.daily_cost_cap_usd {
            break;
        }

        // Pick target section: cycle through mutable sections by gen.
        let target_idx = mutable_indices[gen % mutable_indices.len()];
        let new_body = match mutator
            .mutate_section(&baseline_doc, target_idx, &evidence)
            .await
        {
            Ok(b) => b,
            Err(_) => continue, // mutation failed — try next gen
        };

        let mut candidate_doc = baseline_doc.clone();
        candidate_doc.replace_section(target_idx, new_body)?;
        let cand_hash = candidate_doc.hash();

        // Skip if we've poisoned this exact body before.
        if poisoned.iter().any(|h| h == &cand_hash) {
            continue;
        }

        let cand_results = run_all(replay, &cases, &candidate_doc.render()).await?;
        let cand_summary = summarize(&cand_results);
        cost.add(cand_summary.total_cost_usd);

        candidates.push(Candidate {
            hash: cand_hash,
            summary: cand_summary,
            body: candidate_doc.render(),
            mutated_section: Some(target_idx),
            note: format!("gen{} section={}", gen, target_idx),
        });
    }

    // 10. Pareto + winner pick.
    let _frontier = pareto_frontier(&candidates);
    let winner_idx = best_strict_improvement(&candidates, 0);

    // 11. Apply winner if any.
    let mut applied_hash: Option<String> = None;
    if let Some(idx) = winner_idx {
        let winner = &candidates[idx];
        let backup = backup_and_apply(
            inputs.cwd,
            &inputs.agents_md_path,
            &winner.body,
            &candidates[0].hash,
        )?;
        let pending = PendingApply {
            applied_hash: winner.hash.clone(),
            previous_hash: candidates[0].hash.clone(),
            backup_path: backup,
            baseline_pass_rate: candidates[0].summary.pass_rate,
            applied_at_ms: Utc::now().timestamp_millis(),
            outcomes_seen_at_apply: state.outcomes_seen_lifetime,
        };
        pending.save(inputs.cwd)?;
        applied_hash = Some(winner.hash.clone());
    }

    // 12. Persist state + log.
    let new_state = State {
        last_tick_at_ms: Utc::now().timestamp_millis(),
        outcomes_seen_lifetime: state.outcomes_seen_lifetime.max(cases.len() as u32),
        outcomes_at_last_tick: cases.len() as u32,
        ticks_run: state.ticks_run + 1,
    };
    new_state.save(inputs.cwd)?;
    cost.save(inputs.cwd)?;
    log_candidates_with_apply(inputs.cwd, &candidates, applied_hash.as_deref());

    Ok(TickReport::Ran {
        baseline: baseline_summary,
        generations: candidates,
        applied_hash,
    })
}

/// Rollback monitor: call after recording a new outcome. If a
/// `PendingApply` exists and the post-apply pass rate has regressed,
/// restore the prior AGENTS.md and poison the offending hash.
///
/// `min_window_size` and `regression_threshold` are tunable; defaults
/// `(10, 0.15)` mean "wait for 10 sessions, rollback if pass rate
/// dropped by >= 15 percentage points".
pub fn check_rollback(
    cwd: &Path,
    agents_md_path: &Path,
    post_apply_outcomes: &[bool],
    min_window_size: u32,
    regression_threshold: f32,
) -> Result<bool, TickError> {
    let Some(pending) = PendingApply::load(cwd) else {
        return Ok(false);
    };
    if (post_apply_outcomes.len() as u32) < min_window_size {
        return Ok(false);
    }
    let pass_rate = post_apply_outcomes.iter().filter(|b| **b).count() as f32
        / post_apply_outcomes.len() as f32;
    if !super::apply::should_rollback(
        pending.baseline_pass_rate,
        pass_rate,
        post_apply_outcomes.len() as u32,
        min_window_size,
        regression_threshold,
    ) {
        return Ok(false);
    }
    super::apply::rollback(agents_md_path, &pending.backup_path)?;
    add_poison(cwd, &pending.applied_hash)?;
    PendingApply::clear(cwd)?;
    Ok(true)
}

// ─── helpers ───────────────────────────────────────────────────────────

fn slug(p: &Path) -> String {
    p.display().to_string().replace(['/', '\\', ':'], "_")
}

fn build_evidence(cases: &[BenchmarkCase]) -> MutationEvidence {
    let mut wins = Vec::new();
    let mut losses = Vec::new();
    for c in cases {
        let item = EvidenceItem {
            user_request: c.user_prompt.clone(),
            verdict_reason: c
                .historical_score
                .map(|s| format!("score={s:.2}"))
                .unwrap_or_else(|| "no score".into()),
        };
        match c.historical_success {
            Some(true) => wins.push(item),
            Some(false) => losses.push(item),
            None => {}
        }
    }
    MutationEvidence { wins, losses }
}

fn mutator_config_from_settings(_evolve: &EvolveSettings, settings: &Settings) -> MutatorConfig {
    let mut cfg = MutatorConfig::default();
    if let Some(slow) = &settings.roles.slow {
        if let Some((p, m)) = slow.split_once('/') {
            cfg.provider = p.to_string();
            cfg.model = m.to_string();
        } else {
            cfg.model = slow.clone();
        }
    }
    cfg
}

fn log_candidates(cwd: &Path, candidates: &[Candidate]) {
    log_candidates_with_apply(cwd, candidates, None);
}

fn log_candidates_with_apply(cwd: &Path, candidates: &[Candidate], applied_hash: Option<&str>) {
    let now = Utc::now().timestamp_millis();
    let baseline_hash = candidates.first().map(|c| c.hash.clone());
    for (i, c) in candidates.iter().enumerate() {
        let parent = if i == 0 { None } else { baseline_hash.clone() };
        let entry = GenerationLogEntry {
            timestamp_ms: now,
            hash: c.hash.clone(),
            parent_hash: parent,
            mutated_section: c.mutated_section,
            summary: c.summary.clone(),
            applied: applied_hash == Some(&c.hash),
            note: c.note.clone(),
        };
        let _ = append_generation(cwd, &entry);
    }
}
