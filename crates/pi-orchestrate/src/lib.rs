//! `pi-orchestrate` — TOML campaign schema, validator, and plan formatter.
//!
//! Provides the core types and logic for `pi --orchestrate-dry-run`
//! (RFD 0021 M1 slice).

pub mod dispatch;
pub mod merge;
pub mod plan;
pub mod runner;
pub mod schema;
pub mod validate;
pub mod verdict;

pub use dispatch::{Dispatch, DispatchOutcome, DispatchRole, RealDispatch};
pub use merge::{cherry_pick_to_target, git_checkout, prune_stale_worktrees, rev_parse, MergeOutcome};
pub use plan::{format_plan, topological_order};
pub use runner::{replay, run, run_with, state_path_for, MilestoneOutcome, RunSummary, StateEvent};
pub use schema::{Campaign, Defaults, Milestone, OverrideRule};
pub use validate::{validate, ValidationError};
pub use verdict::{parse_verdict, MergeReadiness};

/// Parse a campaign TOML from a string slice.
pub fn parse_campaign(toml_src: &str) -> Result<Campaign, toml::de::Error> {
    toml::from_str(toml_src)
}
