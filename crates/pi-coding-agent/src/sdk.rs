//! **DEPRECATED — moved to `pi_sdk`.**
//!
//! This module is a back-compat shim that re-exports the pi-sdk façade.
//! It will be removed in pi-coding-agent 0.2 (RFD 0027 Commit K).
//!
//! Migration:
//! ```text
//! - use pi_coding_agent::sdk::*;
//! + use pi_sdk::*;
//! ```
//!
//! `pi_sdk` is published to crates.io as the canonical embed surface;
//! this module exists only so the `pi` binary's internal call sites
//! keep compiling during the 0.x → 1.0 migration window.

#[deprecated(
    since = "0.1.0",
    note = "use pi_sdk instead; pi_coding_agent::sdk will be removed in 0.2 (RFD 0027 Commit K)"
)]
pub use pi_sdk::*;
