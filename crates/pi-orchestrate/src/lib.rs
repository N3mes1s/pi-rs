//! `pi-orchestrate` — TOML campaign schema, validator, and plan formatter.
//!
//! Provides the core types and logic for `pi --orchestrate-dry-run`
//! (RFD 0021 M1 slice).

pub mod plan;
pub mod schema;
pub mod validate;

pub use plan::{format_plan, topological_order};
pub use schema::{Campaign, Defaults, Milestone, OverrideRule};
pub use validate::{validate, ValidationError};

/// Parse a campaign TOML from a string slice.
pub fn parse_campaign(toml_src: &str) -> Result<Campaign, toml::de::Error> {
    toml::from_str(toml_src)
}
