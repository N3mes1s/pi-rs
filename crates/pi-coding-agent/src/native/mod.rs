//! Native pi-rs tools (todo, ask, ttsr, …). Sibling to
//! [`crate::autoresearch`] but generic — these tools have no protocol of
//! their own, they're just native equivalents of common upstream-pi
//! tools.

pub mod ask;
pub mod lsp;
pub mod todo;
pub mod ttsr;
