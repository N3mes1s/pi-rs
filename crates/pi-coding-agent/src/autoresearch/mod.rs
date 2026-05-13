//! pi-autoresearch — native Rust port of the autonomous experiment loop.
//!
//! Tracks optimisation runs in `autoresearch.jsonl`, applies edits, runs
//! benchmarks, keeps improvements, and reverts regressions — all integrated
//! directly with the pi-rs agent loop rather than living in a subprocess
//! extension.
//!
//! # Module layout
//!
//! | Module | Contents |
//! |--------|----------|
//! | [`confidence`]     | MAD-based confidence scoring ([`ConfidenceScore`], [`ConfidenceBand`]) |
//! | [`dashboard`]      | Pure dashboard rendering ([`DashboardState`]) |
//! | [`hooks`]          | Lifecycle hook execution ([`hooks::run_before`], [`hooks::run_after`]) |
//! | [`log`]            | Append-only JSONL persistence ([`LogEntry`], [`JsonlLog`]) |
//! | [`session`]        | Experiment session config & path helpers ([`Session`], [`SessionConfig`]) |
//! | [`slash_helpers`]  | Pure helpers for the `/autoresearch` slash command |
//! | [`tools`]          | Three [`pi_tools::Tool`] impls exposed to the agent |

pub mod confidence;
pub mod dashboard;
pub mod hooks;
pub mod log;
pub mod session;
pub mod slash_helpers;
pub mod tools;

pub use confidence::{compute as compute_confidence, ConfidenceBand, ConfidenceScore};
pub use dashboard::{render_inline, render_table, DashboardState};
pub use log::{jsonl_path, BestDirection, ConfigEntry, JsonlLog, RunEntry, RunStatus};
pub use session::{MetricDirection, Session, SessionConfig};
pub use tools::{InitExperimentTool, LogExperimentTool, RunExperimentRecursiveTool, RunExperimentTool};
