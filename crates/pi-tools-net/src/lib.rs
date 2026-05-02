//! pi-tools-net — network-using tools (web_search, …).
//! Companion to pi-tools-core.
//!
//! Depends on `pi-tools-core` for the shared `Tool` / `ToolContext` /
//! `ToolError` types and on `pi-tool-types` for the POD types.

// Re-export the shared tool types so that web_search.rs can use
// `crate::{Tool, ToolContext, ToolError}` without modification.
pub use pi_tools_core::{Tool, ToolContext, ToolError, ToolResult, ToolSpec};

pub mod web_search;
pub use web_search::WebSearchTool;
