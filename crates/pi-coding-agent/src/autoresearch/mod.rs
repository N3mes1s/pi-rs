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
//! | [`log`] | Append-only JSONL persistence ([`LogEntry`], [`JsonlLog`]) |
//! | [`session`] | Experiment session config & path helpers ([`Session`], [`SessionConfig`]) |
//! | [`tools`] | Three [`pi_tools::Tool`] impls exposed to the agent ([`InitExperimentTool`], [`RunExperimentTool`], [`LogExperimentTool`]) |

pub mod log;
pub mod session;
pub mod tools;

pub use log::{JsonlLog, LogEntry, LogEntryKind};
pub use session::{MetricDirection, Session, SessionConfig};
pub use tools::{InitExperimentTool, LogExperimentTool, RunExperimentTool};
