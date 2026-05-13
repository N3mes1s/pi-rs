//! Session-end trajectory finalization (G3).
//!
//! At the end of every pi session, this module:
//! 1. Reads the current session branch from the [`SessionManager`].
//! 2. Runs the agentic [`Judge`] against it (or the heuristic fallback
//!    when no smol-model auth is configured).
//! 3. Appends a [`SessionEntryKind::Outcome`] entry to the session JSONL.
//!
//! All steps are best-effort: a failure here never blocks pi from
//! exiting. Trajectory recording is observational; if it can't write a
//! verdict the session JSONL just lacks an outcome entry and the evolver
//! ignores that session.
//!
//! The judge is constructed via [`build_judge_from_settings`] using the
//! `roles.smol` model (so users can route to the cheapest model they
//! have credentials for). `None` is returned when no smol model is
//! configured or no auth is available — `finalize_session` then falls
//! back to features-only.
//!
//! Wiring point: each mode (print/json/rpc/interactive) should call
//! [`finalize_session`] right before returning. Wiring is deferred to a
//! later commit because the modes/ files are being actively edited by
//! the parallel dogfood worktree (C2 powerline footer).

use pi_agent_core::{RuntimeConfig, SessionEntryKind, SessionManager, Settings};
use pi_ai::{AuthStorage, ModelRegistry};

use super::features::extract;
use super::judge::{features_only_outcome, Judge, JudgeConfig};

/// Build a [`Judge`] from settings + registry + auth, picking the
/// `roles.smol` model when set (else the default smol). Returns `None`
/// when the model can't be resolved or no auth exists for its provider —
/// caller falls back to features-only.
pub fn build_judge_from_settings(
    settings: &Settings,
    registry: &ModelRegistry,
    auth: &AuthStorage,
) -> Option<Judge> {
    // Prefer roles.smol; if unset, the default config's haiku is fine.
    let mut cfg = JudgeConfig::default();
    if let Some(smol) = &settings.roles.smol {
        // smol can be either "model" (assume default provider) or
        // "provider/model".
        if let Some((p, m)) = smol.split_once('/') {
            cfg.provider = p.to_string();
            cfg.model = m.to_string();
        } else {
            cfg.model = smol.clone();
        }
    }
    Judge::build(registry, auth, cfg).ok()
}

/// Finalize a session. Reads the current branch, scores it, appends an
/// `Outcome` entry. Returns the appended `SessionEntryKind` (or `None`
/// when no signals fired and no judge ran, in which case the session
/// JSONL is left without an outcome — that session simply won't be
/// part of any evolution benchmark).
///
/// Idempotent: if the branch already ends with an `Outcome` entry, this
/// fn does nothing and returns the existing one.
pub async fn finalize_session(
    mgr: &SessionManager,
    session_id: &str,
    judge: Option<&Judge>,
) -> Option<SessionEntryKind> {
    let branch = mgr.current_branch(session_id);
    if branch.is_empty() {
        return None;
    }

    // Idempotency: respect existing Outcome.
    if let Some(existing) = branch.iter().rev().find_map(|e| match &e.kind {
        SessionEntryKind::Outcome { .. } => Some(e.kind.clone()),
        _ => None,
    }) {
        return Some(existing);
    }

    // Try the agentic judge first.
    if let Some(j) = judge {
        if let Ok(verdict) = j.judge(&branch).await {
            let kind = SessionEntryKind::Outcome {
                success: verdict.success,
                source: pi_agent_core::OutcomeSource::LlmJudge,
                score: Some(verdict.score),
                notes: serde_json::to_string(&verdict).ok(),
            };
            let _ = mgr.append(session_id, kind.clone());
            return Some(kind);
        }
    }

    // Fall back to features-only. Cheap, never fails.
    if let Some(kind) = features_only_outcome(&extract(&branch)) {
        let _ = mgr.append(session_id, kind.clone());
        return Some(kind);
    }

    None
}

/// Convenience wrapper for the modes/ entry points: builds a judge
/// from the runtime config + settings, then runs the full finalize.
/// Never returns Err — failures are silently absorbed because
/// trajectory recording is observational, not on the critical path.
///
/// After finalize, fire-and-forget spawns `pi --internal-evolve-tick`
/// as a detached subprocess so the autonomous AGENTS.md evolution loop
/// can run in the background without blocking pi's exit. The tick
/// itself is gated by `should_run` (cost cap, sample threshold, time
/// since last tick); most ticks are no-ops on the happy path.
pub async fn finalize_for_runtime(
    cfg: &RuntimeConfig,
    settings: &Settings,
    session_id: &str,
) -> Option<SessionEntryKind> {
    let judge = build_judge_from_settings(settings, &cfg.model_registry, &cfg.auth_storage)
        .map(|j| j.with_system_prompt_bytes(cfg.system_prompt.len()));
    let outcome = finalize_session(&cfg.session_manager, session_id, judge.as_ref()).await;

    if settings.evolve.enabled {
        // Skip spawning detached evolve ticks when running under halo orchestration.
        if std::env::var("PI_HALO_SUPPRESS_DETACHED_EVOLVE").ok().as_deref() != Some("1") {
            spawn_evolve_tick_detached();
        }
    }

    outcome
}

/// Spawn `pi --internal-evolve-tick` detached from the parent. The
/// child inherits no stdio, so it can outlive the parent without
/// holding the terminal open. Errors are silently swallowed — this is
/// best-effort background optimisation, not on any critical path.
fn spawn_evolve_tick_detached() {
    let pi = match std::env::current_exe() {
        Ok(p) => p,
        Err(_) => return,
    };
    let _ = std::process::Command::new(pi)
        .arg("--internal-evolve-tick")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn();
    // Don't wait — we're done. The OS will reap it.
}
