//! Native pi-rs tools (todo, ask, ttsr, …). Sibling to
//! [`crate::autoresearch`] but generic — these tools have no protocol of
//! their own, they're just native equivalents of common upstream-pi
//! tools.
//!
//! `trajectory` is the recorder + oracle for the autonomous AGENTS.md
//! evolution loop (G group). It is not a tool; the agent never sees it.

pub mod ask;
pub mod todo;
pub mod trajectory;
pub mod ttsr;
