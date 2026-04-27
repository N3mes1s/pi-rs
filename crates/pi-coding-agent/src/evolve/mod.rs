//! Autonomous AGENTS.md evolution loop (G5–G9).
//!
//! Sibling to [`crate::native::trajectory`] (which records sessions and
//! scores them). This module is the active mutation side of the loop:
//!
//! - `agents_md` — parse AGENTS.md into H2-delimited mutable modules,
//!   honour `<!-- pi:keep -->` markers, exact round-trip rendering. (G5)
//! - (G6, next) `mutate` — reflective mutation: slow model rewrites one
//!   section per generation given win/loss trajectories as evidence.
//! - (G7) `benchmark` — replay last N outcome-labelled prompts against a
//!   candidate AGENTS.md via `pi -p`, score with the trajectory judge.
//! - (G8) `tick` — `pi --internal-evolve-tick` subprocess + single-
//!   instance lock + spawn-at-session-end hook + cost cap.
//! - (G9) `apply` — Pareto frontier + auto-apply with backup + auto-
//!   rollback when applied candidate regresses on next K sessions.
//!
//! Design constraints from the user: fully autonomous (no `:up`/`:down`,
//! no `pi evolve apply`), single global toggle, never auto-applies a
//! candidate that regresses on previously-winning prompts.

pub mod agents_md;
pub mod apply;
pub mod benchmark;
pub mod mutate;
pub mod tick;

pub use agents_md::{AgentsMd, Section};
pub use benchmark::{
    load_cases, run_all, summarize, BenchmarkCase, BenchmarkError, BenchmarkSummary, Replay,
    RolloutResult, SubprocessReplay,
};
pub use mutate::{
    build_prompt, post_process, EvidenceItem, MutateError, Mutator, MutatorConfig,
    MutationEvidence,
};
pub use apply::{
    add_poison, append_generation, backup_and_apply, best_strict_improvement,
    is_poisoned, pareto_frontier, poisoned_hashes, read_generations, rollback,
    should_rollback, Candidate, GenerationLogEntry, PendingApply,
};
pub use tick::{
    evolve_dir, is_disabled, should_run, CostLedger, Lock, SkipReason, State, TickDecision,
};
