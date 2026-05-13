//! Trajectory recording + outcome judging (G1–G3).
//!
//! The session JSONL written by `pi-agent-core::session` is already a full
//! trajectory: every message, tool call, tool result, usage, and (since G1)
//! context-load and outcome entry. This module adds the layers that turn a
//! raw transcript into evolution-ready data:
//!
//! - `features::extract` — deterministic signal extraction (test/compile
//!   exit codes, edit error recovery, loop detection, termination state).
//!   These are *evidence*, not verdicts.
//! - `judge::judge_session` (G2) — agentic outcome judge: smol-model reads
//!   the user's prompt + extracted features + a transcript digest and
//!   returns a structured `{success, score, reason, ...}`. The verdict is
//!   what becomes the [`SessionEntryKind::Outcome`].
//! - (G3, next) `subscriber::TrajectorySubscriber` — attaches to the agent's
//!   [`AgentEvent`] stream, persists [`SessionEntryKind::ContextLoad`] and
//!   [`SessionEntryKind::EvolveMarker`] entries, and at session end runs
//!   the judge to append the [`SessionEntryKind::Outcome`].
//!
//! Why not deterministic? Tests-pass ≠ task solved when the user asked
//! "investigate this"; tests-fail ≠ task failed when the agent only
//! diagnosed. The verdict has to read the actual trajectory.

pub mod features;
pub mod flamegraph;
pub mod judge;
pub mod recorder;

pub use features::{extract, TrajectoryFeatures};
pub use judge::{
    build_user_message, features_only_outcome, judge_session, parse_verdict, Judge, JudgeConfig,
    JudgeError, JudgeVerdict,
};
pub use recorder::{build_judge_from_settings, finalize_for_runtime, finalize_session};
